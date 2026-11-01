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
use crate::endpoint::{ApiEndpoint, ApiEndpointCallError, ParseError, TransportError, is_success};
use crate::endpoints::containers::{
    InspectContainer, ListContainers, RestartContainer, RestartRequest, StopContainer, StopRequest,
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

async fn within<T, E: From<TransportError>>(
    limit: Option<Duration>,
    future: impl Future<Output = Result<T, E>>,
) -> Result<T, E> {
    match limit {
        Some(limit) => tokio::time::timeout(limit, future)
            .await
            .map_err(|_elapsed| TransportError::Timeout(limit))?,
        None => future.await,
    }
}

#[derive(Deserialize)]
struct ErrorResponse {
    message: String,
}

fn status_error(status_code: StatusCode, bytes: &[u8]) -> ApiEndpointCallError {
    if let Ok(error_response) = serde_json::from_slice::<ErrorResponse>(bytes) {
        return ApiEndpointCallError::Daemon {
            status: status_code,
            message: error_response.message,
        };
    }

    let body = String::from_utf8_lossy(bytes).into_owned();

    event!(Level::ERROR, %status_code, message = %body, "Invalid response");

    ApiEndpointCallError::HttpError {
        status: status_code,
        body,
    }
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

fn parse<T>(
    parse_response: fn(StatusCode, &[u8]) -> Result<T, ParseError>,
    status: StatusCode,
    bytes: &[u8],
) -> Result<T, ApiEndpointCallError> {
    parse_response(status, bytes).map_err(|error| match error {
        ParseError::Status => status_error(status, bytes),
        ParseError::Body(error) => {
            event!(Level::ERROR, ?error, message = %String::from_utf8_lossy(bytes), "Failed to deserialize response");
            ApiEndpointCallError::Deserialize(error)
        },
    })
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
    ) -> Result<Response, TransportError> {
        self.client
            .request(method, format!("{}{}", self.base_url, path_and_query))
            .send()
            .await
            .map_err(TransportError::Send)
    }

    async fn exchange(
        &self,
        path_and_query: &str,
        method: Method,
    ) -> Result<(StatusCode, Bytes), TransportError> {
        let response = self.send_request(path_and_query, method).await?;

        let status = response.status();

        Ok((status, read_body(response).await?))
    }

    fn prepare<Api: ApiEndpoint>(&self, request: &Api::Request<'_>) -> Query<'_, Api::Response> {
        Query {
            client: self,
            path_and_query: Api::path_and_query(request),
            method: Api::METHOD,
            parse_response: Api::parse_response,
            timeout: None,
        }
    }

    /// List all containers based on a filter.
    pub fn list_containers(&self, filters: &Filters) -> Query<'_, Vec<ContainerSummary>> {
        self.prepare::<ListContainers>(filters)
    }

    /// Inspect container.
    pub fn inspect_container<'r, C: Into<ContainerRef<'r>>>(
        &self,
        container: C,
    ) -> Query<'_, ContainerInspect> {
        self.prepare::<InspectContainer>(&container.into())
    }

    /// Get all networks.
    pub fn list_networks(&self) -> Query<'_, Vec<NetworkSummary>> {
        self.prepare::<ListNetworks>(&())
    }

    /// Inspect network.
    pub fn inspect_network<'r, N: Into<NetworkRef<'r>>>(
        &self,
        network: N,
    ) -> Query<'_, NetworkInspect> {
        self.prepare::<InspectNetwork>(&network.into())
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
        let request = RestartRequest {
            container: container.into(),
            timeout,
        };

        self.prepare::<RestartContainer>(&request).send().await
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
    /// * The response has a status other than `2xx` or `304`
    pub async fn stop_container<'r, C: Into<ContainerRef<'r>>>(
        &self,
        container: C,
        timeout: StopTimeout,
    ) -> Result<StopOutcome, ApiEndpointCallError> {
        let request = StopRequest {
            container: container.into(),
            timeout,
        };

        self.prepare::<StopContainer>(&request).send().await
    }

    /// Subscribe to the daemon's events.
    pub fn subscribe_events(&self) -> Subscribe<'_> {
        Subscribe {
            client: self,
            timeout: None,
        }
    }
}

/// A request to the daemon. Nothing reaches the daemon until [`Query::send`] is awaited.
#[must_use]
pub struct Query<'c, T> {
    client: &'c Client,
    #[expect(
        clippy::struct_field_names,
        reason = "the same term as `ApiEndpoint::path_and_query`"
    )]
    path_and_query: Result<String, std::io::Error>,
    method: Method,
    parse_response: fn(StatusCode, &[u8]) -> Result<T, ParseError>,
    timeout: Option<Duration>,
}

impl<T> Query<'_, T> {
    /// Sets a time limit on [`Query::send`]. Default is no timeout.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);

        self
    }

    /// Send the request and parse the response.
    ///
    /// # Errors
    ///
    /// This function will return an error if:
    ///
    /// * Failure to build the path and query
    /// * Failure to send the request
    /// * Failure to read the response body
    /// * Response is not success
    /// * Failure to deserialize the response
    /// * No complete response within the timeout, as [`TransportError::Timeout`]
    pub async fn send(self) -> Result<T, ApiEndpointCallError> {
        let path_and_query = self.path_and_query.map_err(ApiEndpointCallError::Query)?;

        let (status, bytes) = within(
            self.timeout,
            self.client.exchange(&path_and_query, self.method),
        )
        .await?;

        parse(self.parse_response, status, &bytes)
    }
}

/// An event subscription. Nothing reaches the daemon until [`Subscribe::send`] is awaited.
#[must_use]
pub struct Subscribe<'c> {
    client: &'c Client,
    timeout: Option<Duration>,
}

impl Subscribe<'_> {
    /// Sets a time limit on [`Subscribe::send`], which covers the response head and, for a non-`2xx` status, the error body. The event stream has no time limit. Default is no timeout.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);

        self
    }

    /// Resolves after the response head arrives.
    ///
    /// # Errors
    ///
    /// * Failure to send the request
    /// * Failure to read the response body
    /// * Response is not success
    /// * The time limit elapses, as [`TransportError::Timeout`]
    pub async fn send(self) -> Result<EventSubscription, ApiEndpointCallError> {
        let response = within(self.timeout, async {
            let response = self.client.send_request("/events", Method::GET).await?;

            let status = response.status();

            if is_success(status) {
                Ok(response)
            } else {
                Err(status_error(status, &read_body(response).await?))
            }
        })
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
    use std::path::{Path, PathBuf};
    use std::pin::Pin;
    use std::str::FromStr as _;
    use std::task::{Context, Poll};

    use bytes::Bytes;
    use http_body::{Body, Frame};
    use http_body_util::Empty;
    use pretty_assertions::assert_eq;
    use rcgen::{CertificateParams, KeyPair};
    use reqwest::ClientBuilder;
    use tempfile::TempDir;

    use crate::client::{BuildError, Client, EventLines, with_tls};
    use crate::config::{ClientCredentialPaths, Endpoint, Tls};
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

    fn write(directory: &Path, name: &str, contents: &str) -> PathBuf {
        let path = directory.join(name);

        std::fs::write(&path, contents).unwrap();

        path
    }

    fn tls_client(host: &str, tls: Tls) -> Client {
        Client::build(Endpoint::from_str(host).unwrap().with_tls(tls)).unwrap()
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
