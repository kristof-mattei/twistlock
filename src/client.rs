use std::path::PathBuf;
use std::sync::Arc;

use bytes::Bytes;
use http::uri::Scheme;
use http::{Method, StatusCode};
use http_body::Body;
use http_body_util::BodyExt as _;
use reqwest::redirect::Policy;
use reqwest::{ClientBuilder, Response};
use rustls::crypto::{CryptoProvider, aws_lc_rs};
use rustls::pki_types::pem::{Error as PemError, PemObject as _};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::{ClientConfig, RootCertStore};
use rustls_platform_verifier::BuilderVerifierExt as _;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::{Level, event};

use crate::config::Endpoint as ConfigEndpoint;
use crate::endpoint::{ApiEndpoint, ApiEndpointCallError, TransportError};
use crate::endpoints::containers::{
    InspectContainer, ListContainers, RestartContainer, RestartContainerRequest, StopContainer,
    StopContainerRequest,
};
use crate::endpoints::networks::{InspectNetwork, ListNetworks};
use crate::filters::Filters;
use crate::models::container::{ContainerSummary, StopOutcome, StopTimeout};
use crate::models::container_inspect::ContainerInspect;
use crate::models::events::{Event, EventDecodeError};
use crate::models::id::{ContainerRef, NetworkRef};
use crate::models::network::{NetworkInspect, NetworkSummary};

#[derive(Debug)]
pub struct ClientCredentialPaths {
    pub key: PathBuf,
    pub cert: PathBuf,
}

#[derive(Debug, Error)]
pub enum BuildError {
    #[error("Endpoint `{}` is not an absolute `http` or `https` URI", .0)]
    InvalidEndpoint(http::Uri),
    #[error("The process-default crypto provider is unusable")]
    Provider(#[source] rustls::Error),
    #[error("Failed to read `{}`", .path.display())]
    Read {
        path: PathBuf,
        #[source]
        source: PemError,
    },
    #[error("No certificate in the CA file")]
    EmptyCa,
    #[error("Failed to parse the CA certificate")]
    InvalidCa(#[source] rustls::Error),
    #[error("Failed to load the system CA certificates")]
    SystemCa(#[source] rustls::Error),
    #[error("Failed to load the client certificate and key")]
    InvalidIdentity(#[source] rustls::Error),
    #[error("Failed to build the HTTP client")]
    Client(#[source] reqwest::Error),
}

pub(crate) fn url_encode<T>(filter: &T) -> Result<Box<str>, std::io::Error>
where
    T: Serialize,
{
    let encoded = percent_encoding::percent_encode(
        serde_json::to_string(filter)?.as_bytes(),
        percent_encoding::NON_ALPHANUMERIC,
    )
    .to_string()
    .into_boxed_str();

    Ok(encoded)
}

async fn read_body(response: Response) -> Result<Bytes, TransportError> {
    response.bytes().await.map_err(TransportError::Body)
}

#[derive(Deserialize)]
struct ErrorResponse {
    message: String,
}

fn read_certificates(path: PathBuf) -> Result<Vec<CertificateDer<'static>>, BuildError> {
    CertificateDer::pem_file_iter(&path)
        .and_then(Iterator::collect)
        .map_err(|source| BuildError::Read { path, source })
}

fn tls_config(
    cacert: Option<PathBuf>,
    client_credentials: Option<ClientCredentialPaths>,
) -> Result<ClientConfig, BuildError> {
    let provider = CryptoProvider::get_default()
        .cloned()
        .unwrap_or_else(|| Arc::new(aws_lc_rs::default_provider()));

    let builder = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(BuildError::Provider)?;

    let builder = match cacert {
        Some(cacert) => {
            let mut roots = RootCertStore::empty();

            for certificate in read_certificates(cacert)? {
                roots.add(certificate).map_err(BuildError::InvalidCa)?;
            }

            if roots.is_empty() {
                return Err(BuildError::EmptyCa);
            }

            builder.with_root_certificates(roots)
        },
        None => builder
            .with_platform_verifier()
            .map_err(BuildError::SystemCa)?,
    };

    match client_credentials {
        Some(paths) => {
            let certificates = read_certificates(paths.cert)?;
            let key =
                PrivateKeyDer::from_pem_file(&paths.key).map_err(|source| BuildError::Read {
                    path: paths.key,
                    source,
                })?;

            builder
                .with_client_auth_cert(certificates, key)
                .map_err(BuildError::InvalidIdentity)
        },
        None => Ok(builder.with_no_client_auth()),
    }
}

fn without_tls(builder: ClientBuilder) -> ClientBuilder {
    builder.tls_certs_only([])
}

#[derive(Debug)]
pub struct Client {
    client: reqwest::Client,
    base_url: String,
}

impl Client {
    /// Build a new client.
    ///
    /// # Errors
    ///
    /// See [`BuildError`].
    pub fn build(
        endpoint: ConfigEndpoint,
        cacert: Option<PathBuf>,
        client_credentials: Option<ClientCredentialPaths>,
    ) -> Result<Client, BuildError> {
        let builder = ClientBuilder::new()
            .http1_only()
            .no_proxy()
            .redirect(Policy::none());

        let (builder, base_url) = match endpoint {
            ConfigEndpoint::Direct(uri) => {
                let (Some(scheme), Some(authority)) = (uri.scheme(), uri.authority()) else {
                    return Err(BuildError::InvalidEndpoint(uri));
                };

                let builder = if *scheme == Scheme::HTTPS {
                    builder.tls_backend_preconfigured(tls_config(cacert, client_credentials)?)
                } else if *scheme == Scheme::HTTP {
                    without_tls(builder)
                } else {
                    return Err(BuildError::InvalidEndpoint(uri));
                };

                (builder, format!("{}://{}", scheme, authority))
            },
            #[cfg(not(windows))]
            ConfigEndpoint::Socket(path) => {
                // we're connecting over a socket, so the url is localhost
                (
                    without_tls(builder.unix_socket(path)),
                    "http://localhost".to_owned(),
                )
            },
        };

        Ok(Client {
            client: builder.build().map_err(BuildError::Client)?,
            base_url,
        })
    }

    async fn send_request(
        &self,
        path_and_query: &str,
        method: Method,
        accepts: fn(StatusCode) -> bool,
    ) -> Result<Response, ApiEndpointCallError> {
        let response = self
            .client
            .request(method, format!("{}{}", self.base_url, path_and_query))
            .send()
            .await
            .map_err(TransportError::Send)?;

        let status_code = response.status();

        if accepts(status_code) {
            return Ok(response);
        }

        let bytes = read_body(response).await?;

        if let Ok(error_response) = serde_json::from_slice::<ErrorResponse>(&bytes) {
            return Err(ApiEndpointCallError::Daemon {
                status: status_code,
                message: error_response.message,
            });
        }

        let body = String::from_utf8_lossy(&bytes).into_owned();

        event!(Level::ERROR, %status_code, message = %body, "Invalid response");

        Err(ApiEndpointCallError::HttpError {
            status: status_code,
            body,
        })
    }

    /// Call a typed [`ApiEndpoint`], returning a structured error on failure.
    ///
    /// # Errors
    ///
    /// This function will return an error if:
    ///
    /// * Failure to build the path and query
    /// * Failure to send the request
    /// * The endpoint does not accept the status
    /// * Failed to deserialize the response
    pub(crate) async fn call<E: ApiEndpoint>(
        &self,
        request: &E::Request<'_>,
    ) -> Result<E::Response, ApiEndpointCallError> {
        let path_and_query = E::path_and_query(request).map_err(ApiEndpointCallError::Query)?;

        let response = self
            .send_request(&path_and_query, E::METHOD, E::accepts)
            .await?;

        let status = response.status();

        let bytes = read_body(response).await?;

        E::parse_response(status, &bytes).map_err(|error| {
            event!(Level::ERROR, ?error, message = %String::from_utf8_lossy(&bytes), "Failed to deserialize response");
            ApiEndpointCallError::Deserialize(error)
        })
    }

    /// List all containers based on a filter.
    ///
    /// # Errors
    ///
    /// This function will return an error if:
    ///
    /// * Failure to send the request
    /// * Response is not success
    pub async fn list_containers(
        &self,
        filters: &Filters,
    ) -> Result<Vec<ContainerSummary>, ApiEndpointCallError> {
        self.call::<ListContainers>(filters).await
    }

    /// Inspect container.
    ///
    /// # Errors
    ///
    /// This function will return an error if:
    ///
    /// * Failure to send the request
    /// * Response is not success
    pub async fn inspect_container<'r, C: Into<ContainerRef<'r>>>(
        &self,
        container: C,
    ) -> Result<ContainerInspect, ApiEndpointCallError> {
        self.call::<InspectContainer>(&container.into()).await
    }

    /// Get all networks.
    ///
    /// # Errors
    ///
    /// This function will return an error if:
    ///
    /// * Failure to send the request
    /// * Response is not success
    pub async fn list_networks(&self) -> Result<Vec<NetworkSummary>, ApiEndpointCallError> {
        self.call::<ListNetworks>(&()).await
    }

    /// Inspect network.
    ///
    /// # Errors
    ///
    /// This function will return an error if:
    ///
    /// * Failure to send the request
    /// * Response is not success
    pub async fn inspect_network<'r, N: Into<NetworkRef<'r>>>(
        &self,
        network: N,
    ) -> Result<NetworkInspect, ApiEndpointCallError> {
        self.call::<InspectNetwork>(&network.into()).await
    }

    /// Restart a container.
    ///
    /// See [`Client::stop_container`] for when the future resolves.
    ///
    /// # Errors
    ///
    /// This function will return an error if:
    ///
    /// * Failure to send the request
    /// * Response is not success
    pub async fn restart_container<'r, C: Into<ContainerRef<'r>>>(
        &self,
        container: C,
        timeout: StopTimeout,
    ) -> Result<(), ApiEndpointCallError> {
        self.call::<RestartContainer>(&RestartContainerRequest {
            container: container.into(),
            timeout,
        })
        .await
    }

    /// Stop a container.
    ///
    /// The daemon answers once the container has stopped, so under [`StopTimeout::Indefinite`] the future resolves only after the container exits. Since Docker Engine 25.0, dropping the future ends only the client's wait; the stop continues in the daemon.
    ///
    /// # Errors
    ///
    /// This function will return an error if:
    ///
    /// * Failure to send the request
    /// * The daemon answers with a status other than `2xx` or `304`
    pub async fn stop_container<'r, C: Into<ContainerRef<'r>>>(
        &self,
        container: C,
        timeout: StopTimeout,
    ) -> Result<StopOutcome, ApiEndpointCallError> {
        self.call::<StopContainer>(&StopContainerRequest {
            container: container.into(),
            timeout,
        })
        .await
    }

    /// Waits for the daemon's response head.
    ///
    /// # Errors
    ///
    /// * Failure to send the request
    /// * Response is not success
    pub async fn subscribe_events(&self) -> Result<EventSubscription, ApiEndpointCallError> {
        let response = self
            .send_request("/events", Method::GET, |status| status.is_success())
            .await?;

        Ok(EventSubscription {
            lines: EventLines::new(reqwest::Body::from(response)),
        })
    }
}

#[derive(Debug, Error)]
pub enum EventStreamError {
    #[error("Failed to read frame")]
    Frame(#[source] reqwest::Error),
    #[error("The daemon ended the event stream")]
    Ended,
}

pub struct EventSubscription {
    lines: EventLines<reqwest::Body>,
}

impl EventSubscription {
    /// This method is cancel safe.
    ///
    /// # Errors
    ///
    /// See [`EventStreamError`].
    pub async fn next(&mut self) -> Result<Result<Event, EventDecodeError>, EventStreamError> {
        match self.lines.next().await {
            Ok(Some(event)) => Ok(event),
            Ok(None) => Err(EventStreamError::Ended),
            Err(error) => Err(EventStreamError::Frame(error)),
        }
    }
}

struct EventLines<B> {
    body: B,
    buffer: Vec<u8>,
}

impl<B> EventLines<B>
where
    B: Body<Data = Bytes> + Unpin,
{
    fn new(body: B) -> Self {
        EventLines {
            body,
            buffer: Vec::new(),
        }
    }

    async fn next(&mut self) -> Result<Option<Result<Event, EventDecodeError>>, B::Error> {
        // Inspired by https://github.com/EmbarkStudios/wasmtime/blob/056ccdec94f89d00325970d1239429a1b39ec729/crates/wasi-http/src/http_impl.rs#L246-L268
        loop {
            if let Some(i) = self.buffer.iter().position(|b| b == &b'\n') {
                let decoded = decode_event(&self.buffer[0..i]);

                self.buffer.drain(0..=i);

                return Ok(Some(decoded));
            }

            if !self.buffer.is_empty() {
                // sometimes we get multiple frames per event
                event!(
                    Level::TRACE,
                    leftover = ?String::from_utf8_lossy(&self.buffer),
                    "Buffer leftover"
                );
            }

            let Some(frame) = self.body.frame().await.transpose()? else {
                return Ok(None);
            };

            let Ok(data) = frame.into_data() else {
                // frame is trailers, ignored
                continue;
            };

            self.buffer.extend_from_slice(&data);
        }
    }
}

fn decode_event(line: &[u8]) -> Result<Event, EventDecodeError> {
    event!(Level::TRACE, data = %String::from_utf8_lossy(line), "New event");

    serde_json::from_slice(line).map_err(|source| EventDecodeError {
        source,
        line: line.into(),
    })
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::convert::Infallible;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    #[cfg(unix)]
    use std::os::unix::net::UnixListener;
    use std::pin::Pin;
    use std::str::FromStr as _;
    use std::task::{Context, Poll};

    use bytes::Bytes;
    use http::Uri;
    use http_body::{Body, Frame};
    use http_body_util::Empty;
    use pretty_assertions::assert_eq;
    use tempfile::TempDir;

    use crate::client::{BuildError, Client, EventLines, EventStreamError};
    use crate::config::Endpoint;
    use crate::endpoint::ApiEndpointCallError;
    use crate::models::container::{StopOutcome, StopTimeout};
    use crate::models::events::{Event, EventDecodeError};
    use crate::models::id::ContainerRef;

    struct FailingBody(Option<std::io::Error>);

    impl Body for FailingBody {
        type Data = Bytes;
        type Error = std::io::Error;

        fn poll_frame(
            mut self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Option<Result<Frame<Bytes>, std::io::Error>>> {
            Poll::Ready(self.0.take().map(Err))
        }
    }

    enum Step {
        Data(Bytes),
        Pending,
    }

    struct Frames(VecDeque<Step>);

    impl Body for Frames {
        type Data = Bytes;
        type Error = Infallible;

        fn poll_frame(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
        ) -> Poll<Option<Result<Frame<Bytes>, Infallible>>> {
            match self.0.pop_front() {
                Some(Step::Data(data)) => Poll::Ready(Some(Ok(Frame::data(data)))),
                Some(Step::Pending) => {
                    cx.waker().wake_by_ref();

                    Poll::Pending
                },
                None => Poll::Ready(None),
            }
        }
    }

    fn data(text: &str) -> Step {
        Step::Data(Bytes::copy_from_slice(text.as_bytes()))
    }

    fn frames<const N: usize>(steps: [Step; N]) -> EventLines<Frames> {
        EventLines::new(Frames(VecDeque::from(steps)))
    }

    fn line(action: &str) -> String {
        let mut line = format!(
            r#"{{"Type":"container","Action":"{}","Actor":{{"ID":"0f9fc026ac74","Attributes":{{}}}},"scope":"local","time":1,"timeNano":2}}"#,
            action
        );

        line.push('\n');

        line
    }

    fn container_action(next: Option<Result<Event, EventDecodeError>>) -> Box<str> {
        let Some(Ok(Event::Container(body))) = next else {
            panic!("not a container event");
        };

        body.action
    }

    #[tokio::test]
    async fn failed_frame_read_is_returned() {
        let mut lines =
            EventLines::new(FailingBody(Some(std::io::Error::other("connection reset"))));

        let Err(error) = lines.next().await else {
            panic!("not a frame error");
        };

        assert_eq!(error.to_string(), "connection reset");
    }

    #[tokio::test]
    async fn ended_body_is_none() {
        let mut lines = EventLines::new(Empty::<Bytes>::new());

        assert!(matches!(lines.next().await, Ok(None)));
    }

    #[tokio::test]
    async fn decodable_line_is_returned_as_event() {
        let start = line("start");
        let mut lines = frames([data(&start)]);

        assert_eq!(&*container_action(lines.next().await.unwrap()), "start");
    }

    #[tokio::test]
    async fn undecodable_line_leaves_the_stream_open() {
        let start = line("start");
        let mut lines = frames([data("{\"Type\":\"container\"}\n"), data(&start)]);

        let Ok(Some(Err(error))) = lines.next().await else {
            panic!("not a decode error");
        };

        assert_eq!(&*error.line, br#"{"Type":"container"}"#);

        assert_eq!(&*container_action(lines.next().await.unwrap()), "start");
    }

    #[tokio::test]
    async fn split_event_is_one_event() {
        let start = line("start");
        let (head, tail) = start.split_at(40);
        let mut lines = frames([data(head), data(tail)]);

        assert_eq!(&*container_action(lines.next().await.unwrap()), "start");

        assert!(matches!(lines.next().await, Ok(None)));
    }

    #[tokio::test]
    async fn shared_frame_returns_its_events_in_order() {
        let both = format!("{}{}", line("start"), line("die"));
        let mut lines = frames([data(&both)]);

        assert_eq!(&*container_action(lines.next().await.unwrap()), "start");

        assert_eq!(&*container_action(lines.next().await.unwrap()), "die");
    }

    #[tokio::test]
    async fn dropped_call_keeps_the_partial_line() {
        let start = line("start");
        let (head, tail) = start.split_at(40);
        let mut lines = frames([data(head), Step::Pending, data(tail)]);

        tokio::select! {
            biased;
            _ = lines.next() => panic!("the line is incomplete"),
            () = std::future::ready(()) => {},
        }

        assert_eq!(&*container_action(lines.next().await.unwrap()), "start");
    }

    /// A dropped sender closes the connection.
    fn spawn_daemon<A, S>(accept: A, head: &'static str) -> std::sync::mpsc::Sender<String>
    where
        A: FnOnce() -> S + Send + 'static,
        S: Read + Write,
    {
        let (body_sender, body_receiver) = std::sync::mpsc::channel::<String>();

        std::thread::spawn(move || {
            let mut stream = accept();

            let mut request = Vec::new();
            let mut byte = [0_u8; 1];

            while !request.ends_with(b"\r\n\r\n") {
                stream.read_exact(&mut byte).unwrap();
                request.push(byte[0]);
            }

            stream.write_all(head.as_bytes()).unwrap();

            for part in body_receiver {
                stream.write_all(part.as_bytes()).unwrap();
            }
        });

        body_sender
    }

    fn serve(head: &'static str) -> (Client, std::sync::mpsc::Sender<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let body = spawn_daemon(move || listener.accept().unwrap().0, head);

        let endpoint = Endpoint::from_str(&format!("tcp://{}", address)).unwrap();
        let client = Client::build(endpoint, None, None).unwrap();

        (client, body)
    }

    #[test]
    fn build_without_a_scheme_is_an_invalid_endpoint() {
        let endpoint = Endpoint::Direct(Uri::from_static("localhost:2375"));

        let error = Client::build(endpoint, None, None).unwrap_err();

        assert!(matches!(error, BuildError::InvalidEndpoint(_)));
        assert_eq!(
            error.to_string(),
            "Endpoint `localhost:2375` is not an absolute `http` or `https` URI"
        );
    }

    #[test]
    fn build_with_a_tcp_scheme_is_an_invalid_endpoint() {
        let endpoint = Endpoint::Direct(Uri::from_static("tcp://localhost:2375"));

        let error = Client::build(endpoint, None, None).unwrap_err();

        assert!(matches!(error, BuildError::InvalidEndpoint(_)));
    }

    const CA: &str = "-----BEGIN CERTIFICATE-----
MIIBbjCCARSgAwIBAgIUb7Qa5hvzqbuVb40da0AWEtL9sNYwCgYIKoZIzj0EAwIw
HDEaMBgGA1UEAwwRdHdpc3Rsb2NrLXRlc3QtY2EwIBcNMjYxMDI0MjEzMzA3WhgP
MjEyNjA5MzAyMTMzMDdaMBwxGjAYBgNVBAMMEXR3aXN0bG9jay10ZXN0LWNhMFkw
EwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAESmNBh9yqkZ148CRopkySiSkiSfZbGWrq
oJSm/TlTAcUOzm+M7F1O6mcvaFHno5EjlLBuenNb4W1rXm8+ozSv/aMyMDAwHQYD
VR0OBBYEFKIIioaOK+fb6Apo4QCcQE1f4GbfMA8GA1UdEwEB/wQFMAMBAf8wCgYI
KoZIzj0EAwIDSAAwRQIhAOx/F7iSPZItpjKBb3DkDiYg56rh/APHd2BIeho0Jtft
AiBCu5dC/LB/Ab9mrn3lFzr3Er7sSpCUvoX2Ku31aK7fqg==
-----END CERTIFICATE-----
";

    #[test]
    fn build_with_an_https_endpoint_and_a_ca_file_is_ok() {
        let directory = TempDir::new().unwrap();
        let cacert = directory.path().join("ca.pem");
        std::fs::write(&cacert, CA).unwrap();

        let endpoint = Endpoint::Direct(Uri::from_static("https://localhost:2376"));

        Client::build(endpoint, Some(cacert), None).unwrap();
    }

    #[tokio::test]
    async fn response_head_returns_the_subscription() {
        let (client, body) = serve("HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\n\r\n");

        let mut subscription = client.subscribe_events().await.unwrap();

        let start = line("start");

        body.send(format!("{:x}\r\n{}\r\n0\r\n\r\n", start.len(), start))
            .unwrap();
        drop(body);

        let Ok(Ok(Event::Container(event))) = subscription.next().await else {
            panic!("not a container event");
        };

        assert_eq!(&*event.action, "start");

        assert!(matches!(
            subscription.next().await,
            Err(EventStreamError::Ended)
        ));
    }

    #[tokio::test]
    async fn broken_chunk_is_a_frame_error() {
        let (client, body) = serve("HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\n\r\n");

        let mut subscription = client.subscribe_events().await.unwrap();

        // the chunk announces 5 bytes, and the connection closes after 2
        body.send("5\r\nab".to_owned()).unwrap();
        drop(body);

        let Err(error) = subscription.next().await else {
            panic!("not an error");
        };

        assert!(matches!(error, EventStreamError::Frame(_)));
        assert_eq!(error.to_string(), "Failed to read frame");
    }

    #[tokio::test]
    async fn stop_with_no_content_is_stopped() {
        let (client, _body) = serve("HTTP/1.1 204 No Content\r\n\r\n");

        let outcome = client
            .stop_container(ContainerRef::IdOrName("ubuntu"), StopTimeout::Default)
            .await
            .unwrap();

        assert!(matches!(outcome, StopOutcome::Stopped));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stop_over_a_socket_is_stopped() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("docker.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let _body = spawn_daemon(
            move || listener.accept().unwrap().0,
            "HTTP/1.1 204 No Content\r\n\r\n",
        );

        let client = Client::build(Endpoint::Socket(path), None, None).unwrap();

        let outcome = client
            .stop_container(ContainerRef::IdOrName("ubuntu"), StopTimeout::Default)
            .await
            .unwrap();

        assert!(matches!(outcome, StopOutcome::Stopped));
    }

    #[tokio::test]
    async fn stop_with_not_modified_is_already_stopped() {
        let (client, _body) = serve("HTTP/1.1 304 Not Modified\r\n\r\n");

        let outcome = client
            .stop_container(ContainerRef::IdOrName("ubuntu"), StopTimeout::Default)
            .await
            .unwrap();

        assert!(matches!(outcome, StopOutcome::AlreadyStopped));
    }

    #[tokio::test]
    async fn restart_with_not_modified_is_an_error() {
        let (client, _body) = serve("HTTP/1.1 304 Not Modified\r\n\r\n");

        let Err(ApiEndpointCallError::HttpError { status, .. }) = client
            .restart_container(ContainerRef::IdOrName("ubuntu"), StopTimeout::Default)
            .await
        else {
            panic!("not an HTTP error");
        };

        assert_eq!(status, 304);
    }

    #[tokio::test]
    async fn unsuccessful_subscription_returns_the_daemon_error() {
        let (client, _body) = serve(
            "HTTP/1.1 500 Internal Server Error\r\ncontent-length: 18\r\n\r\n{\"message\":\"boom\"}",
        );

        let Err(ApiEndpointCallError::Daemon { status, message }) = client.subscribe_events().await
        else {
            panic!("not the daemon's error");
        };

        assert_eq!(status, 500);
        assert_eq!(message, "boom");
    }

    #[tokio::test]
    async fn unsuccessful_subscription_without_an_error_response_returns_the_body() {
        let (client, _body) = serve(
            "HTTP/1.1 500 Internal Server Error\r\ncontent-length: 16\r\n\r\n{\"error\":\"boom\"}",
        );

        let Err(ApiEndpointCallError::HttpError { status, body }) = client.subscribe_events().await
        else {
            panic!("not an HTTP error");
        };

        assert_eq!(status, 500);
        assert_eq!(body, r#"{"error":"boom"}"#);
    }
}
