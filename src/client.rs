use std::path::PathBuf;
use std::time::Duration;

use bytes::Bytes;
use http::{Method, StatusCode};
use http_body::Body;
use http_body_util::BodyExt as _;
use reqwest::redirect::Policy;
use reqwest::{Certificate, ClientBuilder, Identity, Response};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::{Level, event};

use crate::config::{Endpoint, TcpAddress, Tls, Transport};
use crate::endpoint::{ApiEndpoint, ApiEndpointCallError, TransportError, is_success};
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

// the Docker CLI sends this `Host` header over a socket
const SOCKET_BASE_URL: &str = "http://api.moby.localhost";

#[derive(Debug, Error)]
pub enum BuildError {
    #[error("Failed to read `{}`", .path.display())]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("No certificate in the CA file")]
    EmptyCa,
    #[error("Failed to parse the CA certificate")]
    InvalidCa(#[source] reqwest::Error),
    #[error("Failed to load the client certificate and key")]
    InvalidIdentity(#[source] reqwest::Error),
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

fn without_loopback_proxy(builder: ClientBuilder, address: &TcpAddress) -> ClientBuilder {
    if address.host.is_loopback() {
        builder.no_proxy()
    } else {
        builder
    }
}

fn without_tls(builder: ClientBuilder) -> ClientBuilder {
    builder.tls_certs_only([])
}

fn read(path: PathBuf) -> Result<Vec<u8>, BuildError> {
    std::fs::read(&path).map_err(|source| BuildError::Read { path, source })
}

fn with_tls(builder: ClientBuilder, tls: Tls) -> Result<ClientBuilder, BuildError> {
    let builder = match (tls.verify, tls.ca_cert) {
        (true, Some(path)) => {
            let certificates =
                Certificate::from_pem_bundle(&read(path)?).map_err(BuildError::InvalidCa)?;

            if certificates.is_empty() {
                return Err(BuildError::EmptyCa);
            }

            builder.tls_certs_only(certificates)
        },
        (true, None) => builder,
        (false, _) => builder.tls_danger_accept_invalid_certs(true),
    };

    match tls.client_credentials {
        Some(paths) => {
            // the certificate file can end without a newline
            let pem = [read(paths.cert)?, read(paths.key)?].join(&b'\n');

            Identity::from_pem(&pem)
                .map(|identity| builder.identity(identity))
                .map_err(BuildError::InvalidIdentity)
        },
        None => Ok(builder),
    }
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
    pub fn build(endpoint: Endpoint) -> Result<Client, BuildError> {
        // the Docker CLI's transport settings
        let builder = ClientBuilder::new()
            .http1_only()
            .redirect(Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .tcp_keepalive(Duration::from_secs(15))
            .tcp_keepalive_interval(Duration::from_secs(15))
            .tcp_keepalive_retries(9)
            .pool_idle_timeout(Duration::from_secs(30))
            .pool_max_idle_per_host(2);

        #[cfg(any(target_os = "android", target_os = "fuchsia", target_os = "linux"))]
        let builder = builder.tcp_user_timeout(None);

        let (builder, base_url) = match endpoint.into_transport() {
            #[cfg(unix)]
            Transport::Unix(path) => (
                without_tls(builder.unix_socket(path)),
                SOCKET_BASE_URL.to_owned(),
            ),
            #[cfg(windows)]
            Transport::NamedPipe(pipe) => (
                without_tls(builder.windows_named_pipe(pipe)),
                SOCKET_BASE_URL.to_owned(),
            ),
            Transport::Tcp { address, tls: None } => (
                without_tls(without_loopback_proxy(builder, &address)),
                format!("http://{}", address),
            ),
            Transport::Tcp {
                address,
                tls: Some(tls),
            } => (
                with_tls(without_loopback_proxy(builder, &address), tls)?,
                format!("https://{}", address),
            ),
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
    /// The future resolves after the container has stopped and started again.
    ///
    /// Dropping the future before it resolves leaves the container in one of two states:
    ///
    /// * Unchanged
    /// * Restarted
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
    /// The future resolves after the container has stopped. With [`StopTimeout::Indefinite`], the daemon does not kill the container, so the future resolves only after the container exits on its own.
    ///
    /// Dropping the future before it resolves leaves the container in one of two states:
    ///
    /// * Unchanged
    /// * Stopped
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
            .send_request("/events", Method::GET, is_success)
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
    use std::path::{Path, PathBuf};
    use std::pin::Pin;
    use std::str::FromStr as _;
    use std::sync::Arc;
    use std::task::{Context, Poll};

    use bytes::Bytes;
    use http_body::{Body, Frame};
    use http_body_util::Empty;
    use pretty_assertions::assert_eq;
    use rcgen::{BasicConstraints, CertificateParams, CertifiedIssuer, IsCa, KeyPair};
    use reqwest::ClientBuilder;
    use rustls::crypto::aws_lc_rs;
    use rustls::server::WebPkiClientVerifier;
    use rustls::{
        ConfigBuilder, RootCertStore, ServerConfig, ServerConnection, StreamOwned, WantsVerifier,
    };
    use tempfile::TempDir;

    use crate::client::{BuildError, Client, EventLines, EventStreamError, with_tls};
    use crate::config::{ClientCredentialPaths, Endpoint, Tls};
    use crate::endpoint::{ApiEndpointCallError, TransportError};
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
        let client = Client::build(endpoint).unwrap();

        (client, body)
    }

    fn write(directory: &Path, name: &str, contents: &str) -> PathBuf {
        let path = directory.join(name);

        std::fs::write(&path, contents).unwrap();

        path
    }

    fn certificate_authority() -> CertifiedIssuer<'static, KeyPair> {
        let mut params = CertificateParams::default();
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);

        CertifiedIssuer::self_signed(params, KeyPair::generate().unwrap()).unwrap()
    }

    fn daemon_config() -> ConfigBuilder<ServerConfig, WantsVerifier> {
        ServerConfig::builder_with_provider(Arc::new(aws_lc_rs::default_provider()))
            .with_safe_default_protocol_versions()
            .unwrap()
    }

    /// Returns the daemon's `DOCKER_HOST`.
    fn serve_tls(
        config: ServerConfig,
        head: &'static str,
    ) -> (String, std::sync::mpsc::Sender<String>) {
        let config = Arc::new(config);
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let body = spawn_daemon(
            move || {
                StreamOwned::new(
                    ServerConnection::new(config).unwrap(),
                    listener.accept().unwrap().0,
                )
            },
            head,
        );

        (format!("tcp://{}", address), body)
    }

    fn tls_client(host: &str, tls: Tls) -> Client {
        Client::build(Endpoint::from_str(host).unwrap().with_tls(tls)).unwrap()
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
            .stop_container(
                ContainerRef::try_from("ubuntu").unwrap(),
                StopTimeout::Default,
            )
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

        let endpoint = Endpoint::from_str(&format!("unix://{}", path.display())).unwrap();
        let client = Client::build(endpoint).unwrap();

        let outcome = client
            .stop_container(
                ContainerRef::try_from("ubuntu").unwrap(),
                StopTimeout::Default,
            )
            .await
            .unwrap();

        assert!(matches!(outcome, StopOutcome::Stopped));
    }

    #[tokio::test]
    async fn stop_with_not_modified_is_already_stopped() {
        let (client, _body) = serve("HTTP/1.1 304 Not Modified\r\n\r\n");

        let outcome = client
            .stop_container(
                ContainerRef::try_from("ubuntu").unwrap(),
                StopTimeout::Default,
            )
            .await
            .unwrap();

        assert!(matches!(outcome, StopOutcome::AlreadyStopped));
    }

    #[tokio::test]
    async fn restart_with_not_modified_is_an_error() {
        let (client, _body) = serve("HTTP/1.1 304 Not Modified\r\n\r\n");

        let Err(ApiEndpointCallError::HttpError { status, .. }) = client
            .restart_container(
                ContainerRef::try_from("ubuntu").unwrap(),
                StopTimeout::Default,
            )
            .await
        else {
            panic!("not an HTTP error");
        };

        assert_eq!(status, 304);
    }

    #[tokio::test]
    async fn stop_with_not_found_is_the_daemon_error() {
        let (client, _body) = serve(
            "HTTP/1.1 404 Not Found\r\ncontent-length: 39\r\n\r\n{\"message\":\"No such container: ubuntu\"}",
        );

        let Err(ApiEndpointCallError::Daemon { status, message }) = client
            .stop_container(
                ContainerRef::try_from("ubuntu").unwrap(),
                StopTimeout::Default,
            )
            .await
        else {
            panic!("not the daemon's error");
        };

        assert_eq!(status, 404);
        assert_eq!(message, "No such container: ubuntu");
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

    #[tokio::test]
    async fn stop_over_mutual_tls_is_stopped() {
        let ca = certificate_authority();

        let daemon_key = KeyPair::generate().unwrap();
        let daemon_cert = CertificateParams::new(["127.0.0.1".to_owned()])
            .unwrap()
            .signed_by(&daemon_key, &ca)
            .unwrap();

        let client_key = KeyPair::generate().unwrap();
        let client_cert = CertificateParams::default()
            .signed_by(&client_key, &ca)
            .unwrap();

        let mut roots = RootCertStore::empty();
        roots.add(ca.der().clone()).unwrap();

        let config = daemon_config()
            .with_client_cert_verifier(
                WebPkiClientVerifier::builder_with_provider(
                    Arc::new(roots),
                    Arc::new(aws_lc_rs::default_provider()),
                )
                .build()
                .unwrap(),
            )
            .with_single_cert(vec![daemon_cert.der().clone()], daemon_key.into())
            .unwrap();

        let (host, _body) = serve_tls(config, "HTTP/1.1 204 No Content\r\n\r\n");

        let directory = TempDir::new().unwrap();

        let client = tls_client(
            &host,
            Tls {
                verify: true,
                ca_cert: Some(write(directory.path(), "ca.pem", &ca.pem())),
                client_credentials: Some(ClientCredentialPaths {
                    cert: write(directory.path(), "cert.pem", &client_cert.pem()),
                    key: write(directory.path(), "key.pem", &client_key.serialize_pem()),
                }),
            },
        );

        let outcome = client
            .stop_container(
                ContainerRef::try_from("ubuntu").unwrap(),
                StopTimeout::Default,
            )
            .await
            .unwrap();

        assert!(matches!(outcome, StopOutcome::Stopped));
    }

    #[tokio::test]
    async fn stop_over_unverified_tls_is_stopped() {
        let daemon_key = KeyPair::generate().unwrap();
        let daemon_cert = CertificateParams::new(["127.0.0.1".to_owned()])
            .unwrap()
            .self_signed(&daemon_key)
            .unwrap();

        let config = daemon_config()
            .with_no_client_auth()
            .with_single_cert(vec![daemon_cert.der().clone()], daemon_key.into())
            .unwrap();

        let (host, _body) = serve_tls(config, "HTTP/1.1 204 No Content\r\n\r\n");

        let client = tls_client(
            &host,
            Tls {
                verify: false,
                ca_cert: None,
                client_credentials: None,
            },
        );

        let outcome = client
            .stop_container(
                ContainerRef::try_from("ubuntu").unwrap(),
                StopTimeout::Default,
            )
            .await
            .unwrap();

        assert!(matches!(outcome, StopOutcome::Stopped));
    }

    #[tokio::test]
    async fn daemon_certificate_from_another_ca_is_a_send_error() {
        let daemon_key = KeyPair::generate().unwrap();
        let daemon_cert = CertificateParams::new(["127.0.0.1".to_owned()])
            .unwrap()
            .self_signed(&daemon_key)
            .unwrap();

        let config = Arc::new(
            daemon_config()
                .with_no_client_auth()
                .with_single_cert(vec![daemon_cert.der().clone()], daemon_key.into())
                .unwrap(),
        );

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();

        std::thread::spawn(move || {
            let mut stream = StreamOwned::new(
                ServerConnection::new(config).unwrap(),
                listener.accept().unwrap().0,
            );

            stream.read(&mut [0_u8; 1]).unwrap_err();
        });

        let directory = TempDir::new().unwrap();

        let client = tls_client(
            &format!("tcp://{}", address),
            Tls {
                verify: true,
                ca_cert: Some(write(
                    directory.path(),
                    "ca.pem",
                    &certificate_authority().pem(),
                )),
                client_credentials: None,
            },
        );

        let result = client
            .stop_container(
                ContainerRef::try_from("ubuntu").unwrap(),
                StopTimeout::Default,
            )
            .await;

        assert!(matches!(
            result,
            Err(ApiEndpointCallError::Transport(TransportError::Send(_)))
        ));
    }

    #[test]
    fn unparsable_ca_file_is_an_invalid_ca_error() {
        let directory = TempDir::new().unwrap();

        let result = with_tls(
            ClientBuilder::new(),
            Tls {
                verify: true,
                ca_cert: Some(write(
                    directory.path(),
                    "ca.pem",
                    "-----BEGIN CERTIFICATE-----\n!\n-----END CERTIFICATE-----\n",
                )),
                client_credentials: None,
            },
        );

        assert!(matches!(result, Err(BuildError::InvalidCa(_))));
    }

    #[test]
    fn ca_file_without_a_certificate_is_an_empty_ca_error() {
        let directory = TempDir::new().unwrap();

        let result = with_tls(
            ClientBuilder::new(),
            Tls {
                verify: true,
                ca_cert: Some(write(
                    directory.path(),
                    "ca.pem",
                    &KeyPair::generate().unwrap().serialize_pem(),
                )),
                client_credentials: None,
            },
        );

        assert!(matches!(result, Err(BuildError::EmptyCa)));
    }

    #[test]
    fn missing_ca_file_is_a_read_error() {
        let directory = TempDir::new().unwrap();
        let ca_cert = directory.path().join("ca.pem");

        let Err(BuildError::Read { path, .. }) = with_tls(
            ClientBuilder::new(),
            Tls {
                verify: true,
                ca_cert: Some(ca_cert.clone()),
                client_credentials: None,
            },
        ) else {
            panic!("not a read error");
        };

        assert_eq!(path, ca_cert);
    }

    #[test]
    fn missing_ca_file_without_verification_is_not_read() {
        let directory = TempDir::new().unwrap();

        tls_client(
            "tcp://localhost:2376",
            Tls {
                verify: false,
                ca_cert: Some(directory.path().join("ca.pem")),
                client_credentials: None,
            },
        );
    }

    #[test]
    fn key_file_without_a_key_is_an_invalid_identity_error() {
        let directory = TempDir::new().unwrap();
        let cert = CertificateParams::default()
            .self_signed(&KeyPair::generate().unwrap())
            .unwrap();

        let result = with_tls(
            ClientBuilder::new(),
            Tls {
                verify: true,
                ca_cert: None,
                client_credentials: Some(ClientCredentialPaths {
                    cert: write(directory.path(), "cert.pem", &cert.pem()),
                    key: write(directory.path(), "key.pem", "not a key"),
                }),
            },
        );

        assert!(matches!(result, Err(BuildError::InvalidIdentity(_))));
    }
}
