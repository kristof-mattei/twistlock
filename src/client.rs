use std::path::PathBuf;
use std::time::Duration;

use http_body_util::{BodyExt as _, Full};
use hyper::body::{Body, Bytes, Incoming};
use hyper::{Method, Response};
use hyper_rustls::{FixedServerNameResolver, HttpsConnector, HttpsConnectorBuilder};
#[cfg(not(target_os = "windows"))]
use hyper_unix_socket::UnixSocketConnector;
use hyper_util::client::legacy::Client as HttpClient;
use hyper_util::client::legacy::connect::HttpConnector;
use rustls::client::ClientConfig;
use rustls::pki_types::pem::{Error as PemError, PemObject as _};
use rustls::pki_types::{CertificateDer, DnsName, PrivateKeyDer, ServerName};
use rustls::{DEFAULT_VERSIONS, RootCertStore};
use serde::Serialize;
use serde::de::DeserializeOwned;
use thiserror::Error;
use tokio::time::timeout;
use tracing::{Level, event};

use crate::config::Endpoint as ConfigEndpoint;
use crate::endpoint::{ApiEndpoint, ApiEndpointCallError, TransportError};
use crate::endpoints::containers::{
    InspectContainer, ListContainers, RestartContainer, RestartContainerRequest,
};
use crate::endpoints::networks::{InspectNetwork, ListNetworks};
use crate::filters::Filters;
use crate::http_client;
use crate::http_client::{build_request, execute_request};
use crate::models::container::ContainerSummary;
use crate::models::container_inspect::ContainerInspect;
use crate::models::events::{Event, EventDecodeError};
use crate::models::id::{ContainerRef, NetworkRef};
use crate::models::network::{NetworkInspect, NetworkSummary};

#[derive(Debug)]
pub(crate) enum DockerEndpoint {
    #[cfg(not(windows))]
    Socket(HttpClient<UnixSocketConnector<PathBuf>, Full<Bytes>>),
    Tls(HttpClient<HttpsConnector<HttpConnector>, Full<Bytes>>),
}

#[derive(Debug)]
pub struct ClientCredentialPaths {
    pub key: PathBuf,
    pub cert: PathBuf,
}

struct ClientCredentials {
    key: PrivateKeyDer<'static>,
    certs: Vec<CertificateDer<'static>>,
}

#[derive(Debug, Error)]
pub enum BuildError {
    #[error("Failed to read a PEM file")]
    Pem(#[from] PemError),
    #[error("Failed to build the TLS configuration")]
    Tls(#[from] rustls::Error),
}

fn build_root_cert_store(cacert: Option<PathBuf>) -> Result<RootCertStore, BuildError> {
    let mut store = RootCertStore::empty();

    if let Some(cacert) = cacert {
        for cert in CertificateDer::pem_file_iter(cacert)? {
            store.add(cert?)?;
        }
    } else {
        let native_certs = rustls_native_certs::load_native_certs();

        for error in native_certs.errors {
            event!(Level::ERROR, ?error, "Failed to load certificate");
        }

        let (added, ignored) = store.add_parsable_certificates(native_certs.certs);

        if ignored > 0 {
            event!(
                Level::WARN,
                added,
                ignored,
                "Ignored unparsable certificates from the OS trust store"
            );
        }
    }

    Ok(store)
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

#[derive(Debug)]
pub struct Client {
    endpoint: DockerEndpoint,
    uri: http::Uri,
    docker_timeout: Duration,
}

impl Client {
    /// Build a new client.
    ///
    /// # Errors
    ///
    /// * Invalid certificate path / setup
    /// * `docker_socket_or_uri` is not a valid path or `Uri`
    #[expect(clippy::missing_panics_doc, reason = "Not needed")]
    pub fn build(
        endpoint: ConfigEndpoint,
        cacert: Option<PathBuf>,
        client_credentials: Option<ClientCredentialPaths>,
        timeout: Duration,
    ) -> Result<Client, BuildError> {
        let daemon = match endpoint {
            ConfigEndpoint::Direct(url) => {
                let client_credentials = client_credentials
                    .map(|paths| -> Result<ClientCredentials, BuildError> {
                        Ok(ClientCredentials {
                            key: PrivateKeyDer::from_pem_file(paths.key)?,
                            certs: CertificateDer::pem_file_iter(paths.cert)?
                                .collect::<Result<Vec<_>, _>>()?,
                        })
                    })
                    .transpose()?;

                let root_store = build_root_cert_store(cacert)?;

                let client_config = ClientConfig::builder_with_protocol_versions(DEFAULT_VERSIONS)
                    .with_root_certificates(root_store);

                let client_config = if let Some(client_credentials) = client_credentials {
                    client_config
                        .with_client_auth_cert(client_credentials.certs, client_credentials.key)?
                } else {
                    client_config.with_no_client_auth()
                };

                let connector = HttpsConnectorBuilder::new()
                    .with_tls_config(client_config)
                    .https_or_http()
                    .with_server_name_resolver(FixedServerNameResolver::new(ServerName::DnsName(
                        DnsName::try_from_str("docker.localhost").unwrap(),
                    )))
                    .enable_http1()
                    .build();

                Client {
                    endpoint: DockerEndpoint::Tls(http_client::build_client(connector)),
                    uri: url,
                    docker_timeout: timeout,
                }
            },
            #[cfg(not(windows))]
            ConfigEndpoint::Socket(path_buf) => {
                // we're connecting over a socket, so the url is localhost

                let connector: UnixSocketConnector<PathBuf> = UnixSocketConnector::new(path_buf);

                Client {
                    endpoint: DockerEndpoint::Socket(http_client::build_client(connector)),
                    uri: http::Uri::from_static("http://localhost"),
                    docker_timeout: timeout,
                }
            },
        };

        Ok(daemon)
    }

    async fn read_body(&self, response: Response<Incoming>) -> Result<Bytes, TransportError> {
        match timeout(self.docker_timeout, response.collect()).await {
            Ok(Ok(collected)) => Ok(collected.to_bytes()),
            Ok(Err(error)) => Err(TransportError::Body(error)),
            Err(_elapsed) => Err(TransportError::Timeout),
        }
    }

    async fn send_request<TError>(
        &self,
        path_and_query: &str,
        method: Method,
    ) -> Result<Response<Incoming>, ApiEndpointCallError<TError>>
    where
        TError: DeserializeOwned + std::fmt::Debug,
    {
        let request = build_request(self.uri.clone(), path_and_query, method)
            .map_err(TransportError::Build)?;

        let response = match self.endpoint {
            DockerEndpoint::Tls(ref client) => {
                timeout(self.docker_timeout, execute_request(client, request)).await
            },
            #[cfg(not(windows))]
            DockerEndpoint::Socket(ref client) => {
                timeout(self.docker_timeout, execute_request(client, request)).await
            },
        };

        let response = response
            .map_err(|_elapsed| TransportError::Timeout)?
            .map_err(TransportError::Send)?;

        let status_code = response.status();

        if status_code.is_success() {
            return Ok(response);
        }

        let bytes = self.read_body(response).await?;

        if let Ok(typed_err) = serde_json::from_slice::<TError>(&bytes) {
            return Err(ApiEndpointCallError::Typed(typed_err));
        }

        if let Ok(generic) = serde_json::from_slice::<serde_json::Value>(&bytes) {
            return Err(ApiEndpointCallError::Generic(generic));
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
    /// * Response is not success
    /// * Failed to deserialize the response
    pub async fn call<E: ApiEndpoint>(
        &self,
        request: &E::Request<'_>,
    ) -> Result<E::Response, ApiEndpointCallError<E::Error>> {
        let path_and_query = E::path_and_query(request).map_err(ApiEndpointCallError::Query)?;

        let response = self
            .send_request::<E::Error>(&path_and_query, E::METHOD)
            .await?;

        let bytes = self.read_body(response).await?;

        E::parse_response(&bytes).map_err(|error| {
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
    ) -> Result<Vec<ContainerSummary>, ApiEndpointCallError<<ListContainers as ApiEndpoint>::Error>>
    {
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
    ) -> Result<ContainerInspect, ApiEndpointCallError<<InspectContainer as ApiEndpoint>::Error>>
    {
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
    pub async fn list_networks(
        &self,
    ) -> Result<Vec<NetworkSummary>, ApiEndpointCallError<<ListNetworks as ApiEndpoint>::Error>>
    {
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
    ) -> Result<NetworkInspect, ApiEndpointCallError<<InspectNetwork as ApiEndpoint>::Error>> {
        self.call::<InspectNetwork>(&network.into()).await
    }

    /// Restart a container.
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
        timeout: std::time::Duration,
    ) -> Result<(), ApiEndpointCallError<<RestartContainer as ApiEndpoint>::Error>> {
        self.call::<RestartContainer>(&RestartContainerRequest {
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
    pub async fn subscribe_events(
        &self,
    ) -> Result<EventSubscription, ApiEndpointCallError<serde_json::Value>> {
        let response = self.send_request("/events", Method::GET).await?;

        Ok(EventSubscription {
            lines: EventLines::new(response.into_body()),
        })
    }
}

#[derive(Debug, Error)]
pub enum EventStreamError {
    #[error("Failed to read frame")]
    Frame(#[source] hyper::Error),
    #[error("The daemon ended the event stream")]
    Ended,
}

pub struct EventSubscription {
    lines: EventLines<Incoming>,
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
    use std::io::{Read as _, Write as _};
    use std::net::TcpListener;
    use std::pin::Pin;
    use std::str::FromStr as _;
    use std::task::{Context, Poll};
    use std::time::Duration;

    use http_body_util::Empty;
    use hyper::body::{Body, Bytes, Frame};
    use pretty_assertions::assert_eq;

    use crate::client::{Client, EventLines, EventStreamError};
    use crate::config::Endpoint;
    use crate::endpoint::{ApiEndpointCallError, TransportError};
    use crate::filters::Filters;
    use crate::models::events::{Event, EventDecodeError};

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
    fn serve(head: &'static str) -> (Client, std::sync::mpsc::Sender<String>) {
        serve_with_timeout(head, Duration::from_secs(5))
    }

    fn serve_with_timeout(
        head: &'static str,
        docker_timeout: Duration,
    ) -> (Client, std::sync::mpsc::Sender<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (body_sender, body_receiver) = std::sync::mpsc::channel::<String>();

        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();

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

        let endpoint = Endpoint::from_str(&format!("tcp://{}", address)).unwrap();
        let client = Client::build(endpoint, None, None, docker_timeout).unwrap();

        (client, body_sender)
    }

    #[tokio::test]
    async fn unsent_body_is_a_timeout() {
        // the head announces 5 bytes, and the connection stays open without them
        let (client, _body) = serve_with_timeout(
            "HTTP/1.1 200 OK\r\ncontent-length: 5\r\n\r\n",
            Duration::from_millis(100),
        );

        let Err(ApiEndpointCallError::Transport(error)) =
            client.list_containers(&Filters::default()).await
        else {
            panic!("not a transport error");
        };

        assert!(matches!(error, TransportError::Timeout));
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
    async fn unsuccessful_subscription_returns_the_daemon_error() {
        let (client, _body) = serve(
            "HTTP/1.1 500 Internal Server Error\r\ncontent-length: 18\r\n\r\n{\"message\":\"boom\"}",
        );

        let Err(ApiEndpointCallError::Typed(error)) = client.subscribe_events().await else {
            panic!("not the daemon's error");
        };

        assert_eq!(error["message"], "boom");
    }
}
