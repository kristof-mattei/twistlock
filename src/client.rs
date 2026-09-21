use std::path::PathBuf;
use std::time::Duration;

use color_eyre::{Section as _, eyre};
use http_body_util::{BodyExt as _, Full};
use hyper::body::{Body, Bytes, Incoming};
use hyper::{Method, Response};
use hyper_rustls::{FixedServerNameResolver, HttpsConnector, HttpsConnectorBuilder};
#[cfg(not(target_os = "windows"))]
use hyper_unix_socket::UnixSocketConnector;
use hyper_util::client::legacy::Client as HttpClient;
use hyper_util::client::legacy::connect::HttpConnector;
use rustls::client::ClientConfig;
use rustls::pki_types::pem::PemObject as _;
use rustls::pki_types::{CertificateDer, DnsName, PrivateKeyDer, ServerName};
use rustls::{DEFAULT_VERSIONS, RootCertStore};
use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;
use tracing::{Level, event};

use crate::config::Endpoint as ConfigEndpoint;
use crate::endpoint::{ApiEndpoint, ApiEndpointCallError};
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

fn build_root_cert_store(cacert: Option<PathBuf>) -> Result<RootCertStore, eyre::Report> {
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
    ) -> Result<Client, eyre::Report> {
        let daemon = match endpoint {
            ConfigEndpoint::Direct(url) => {
                let client_credentials = client_credentials
                    .map(|paths| -> Result<ClientCredentials, eyre::Report> {
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

    async fn send_request<TError>(
        &self,
        path_and_query: &str,
        method: Method,
    ) -> Result<Response<Incoming>, ApiEndpointCallError<TError>>
    where
        TError: DeserializeOwned + std::fmt::Debug,
    {
        let request = build_request(self.uri.clone(), path_and_query, method)
            .map_err(ApiEndpointCallError::Transport)?;

        let response: Result<Response<Incoming>, eyre::Report> = match self.endpoint {
            DockerEndpoint::Tls(ref client) => {
                let response = execute_request(client, request);

                match timeout(self.docker_timeout, response).await {
                    Ok(Ok(response)) => Ok(response),
                    Ok(Err(error)) => Err(error.into()),
                    Err(error) => Err(error.into()),
                }
            },
            #[cfg(not(windows))]
            DockerEndpoint::Socket(ref client) => {
                let response = execute_request(client, request);

                match timeout(self.docker_timeout, response).await {
                    Ok(Ok(response)) => Ok(response),
                    Ok(Err(error)) => Err(error.into()),
                    Err(error) => Err(error.into()),
                }
            },
        };

        let response = response.map_err(ApiEndpointCallError::Transport)?;

        let status_code = response.status();

        if status_code.is_success() {
            return Ok(response);
        }

        let bytes = response
            .collect()
            .await
            .map_err(|error| ApiEndpointCallError::Transport(error.into()))?
            .to_bytes();

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
        let path_and_query = E::path_and_query(request)
            .map_err(|error| ApiEndpointCallError::Transport(error.into()))?;

        let response = self
            .send_request::<E::Error>(&path_and_query, E::METHOD)
            .await?;

        let bytes = response
            .collect()
            .await
            .map_err(|error| ApiEndpointCallError::Transport(error.into()))?
            .to_bytes();

        E::parse_response(&bytes).map_err(|error| {
            event!(Level::ERROR, ?error, message = %String::from_utf8_lossy(&bytes), "Failed to deserialize response");
            ApiEndpointCallError::Transport(error.into())
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

    /// Listen for events.
    ///
    /// An undecodable line is sent as an [`EventDecodeError`], and the stream continues.
    ///
    /// # Errors
    ///
    /// This function will return an error if:
    ///
    /// * Failure to send the request
    /// * Response is not success
    /// * Failure to read a frame
    /// * The daemon ends the stream
    /// * The receiver is closed
    pub async fn produce_events(
        &self,
        sender: tokio::sync::mpsc::Sender<Result<Event, EventDecodeError>>,
        cancellation_token: &CancellationToken,
    ) -> Result<(), eyre::Report> {
        let path_and_query = format!("/events{}", "");

        let response = self
            .send_request::<serde_json::Value>(&path_and_query, Method::GET)
            .await?;

        Client::forward_events(response, &sender, cancellation_token).await
    }

    async fn forward_events<B>(
        mut body: B,
        sender: &tokio::sync::mpsc::Sender<Result<Event, EventDecodeError>>,
        cancellation_token: &CancellationToken,
    ) -> Result<(), eyre::Report>
    where
        B: Body<Data = Bytes> + Unpin,
        B::Error: std::error::Error + Send + Sync + 'static,
    {
        let mut buffer = Vec::<u8>::new();

        // Inspired by https://github.com/EmbarkStudios/wasmtime/blob/056ccdec94f89d00325970d1239429a1b39ec729/crates/wasi-http/src/http_impl.rs#L246-L268
        loop {
            let frame = tokio::select! {
                frame = body.frame() => frame,
                () = cancellation_token.cancelled() => {
                    return Ok(());
                },
            };

            let frame = match frame {
                Some(Ok(frame)) => frame,
                Some(Err(error)) => {
                    return Err(eyre::Report::new(error).wrap_err("Failed to read frame"));
                },
                None => {
                    return Err(eyre::Report::msg("No more next frame, other side gone"));
                },
            };

            let Ok(data) = frame.into_data() else {
                // frame is trailers, ignored
                continue;
            };

            buffer.extend_from_slice(&data);

            while let Some(i) = buffer.iter().position(|b| b == &b'\n') {
                Client::decode_send(&buffer[0..i], sender).await?;

                buffer.drain(0..=i);
            }

            if !buffer.is_empty() {
                // sometimes we get multiple frames per event
                event!(
                    Level::TRACE,
                    leftover = ?String::from_utf8_lossy(&buffer),
                    "Buffer leftover"
                );
            }
        }
    }

    async fn decode_send(
        line: &[u8],
        sender: &tokio::sync::mpsc::Sender<Result<Event, EventDecodeError>>,
    ) -> Result<(), eyre::Report> {
        event!(Level::TRACE, data = %String::from_utf8_lossy(line), "New event");

        let decoded = serde_json::from_slice(line).map_err(|source| EventDecodeError {
            source,
            line: line.into(),
        });

        sender
            .send(decoded)
            .await
            .map_err(|error| eyre::Report::msg("Channel closed").error(error))
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read as _, Write as _};
    use std::net::TcpListener;
    use std::pin::Pin;
    use std::str::FromStr as _;
    use std::task::{Context, Poll};
    use std::time::Duration;

    use http_body_util::Empty;
    use hyper::body::{Body, Bytes, Frame};
    use pretty_assertions::assert_eq;
    use tokio_util::sync::CancellationToken;

    use crate::client::Client;
    use crate::config::Endpoint;
    use crate::endpoint::ApiEndpointCallError;
    use crate::models::events::Event;

    /// Yields its error once, then ends.
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

    #[tokio::test]
    async fn failed_frame_read_is_returned() {
        let (sender, _receiver) = tokio::sync::mpsc::channel(1);

        let error = Client::forward_events(
            FailingBody(Some(std::io::Error::other("connection reset"))),
            &sender,
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();

        assert_eq!(error.to_string(), "Failed to read frame");
        assert_eq!(error.root_cause().to_string(), "connection reset");
    }

    #[tokio::test]
    async fn ended_stream_is_an_error() {
        let (sender, _receiver) = tokio::sync::mpsc::channel(1);

        let error =
            Client::forward_events(Empty::<Bytes>::new(), &sender, &CancellationToken::new())
                .await
                .unwrap_err();

        assert_eq!(error.to_string(), "No more next frame, other side gone");
    }

    #[tokio::test]
    async fn decodable_line_is_sent_as_event() {
        let (sender, mut receiver) = tokio::sync::mpsc::channel(1);

        Client::decode_send(
            br#"{"Type":"container","Action":"start","Actor":{"ID":"0f9fc026ac74","Attributes":{}},"scope":"local","time":1,"timeNano":2}"#,
            &sender,
        )
        .await
        .unwrap();

        let Some(Ok(Event::Container(body))) = receiver.recv().await else {
            panic!("not a container event");
        };

        assert_eq!(&*body.action, "start");
    }

    #[tokio::test]
    async fn undecodable_line_is_sent_as_error() {
        let (sender, mut receiver) = tokio::sync::mpsc::channel(1);

        Client::decode_send(br#"{"Type":"container"}"#, &sender)
            .await
            .unwrap();

        let Some(Err(error)) = receiver.recv().await else {
            panic!("not a decode error");
        };

        assert_eq!(&*error.line, br#"{"Type":"container"}"#);
    }

    #[tokio::test]
    async fn closed_receiver_is_an_error() {
        let (sender, receiver) = tokio::sync::mpsc::channel(1);
        drop(receiver);

        let error = Client::decode_send(b"{}", &sender).await.unwrap_err();

        assert_eq!(error.to_string(), "Channel closed");
    }

    /// Answers one request with `head`, then writes each message of the returned channel to the connection.
    fn serve(head: &'static str) -> (Client, std::sync::mpsc::Sender<String>) {
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
        let client = Client::build(endpoint, None, None, Duration::from_secs(5)).unwrap();

        (client, body_sender)
    }

    #[tokio::test]
    async fn unsuccessful_response_is_an_error() {
        let (client, _body) = serve(
            "HTTP/1.1 500 Internal Server Error\r\ncontent-length: 18\r\n\r\n{\"message\":\"boom\"}",
        );
        let (sender, _receiver) = tokio::sync::mpsc::channel(1);

        let error = client
            .produce_events(sender, &CancellationToken::new())
            .await
            .unwrap_err();

        let Some(&ApiEndpointCallError::Typed(ref error)) =
            error.downcast_ref::<ApiEndpointCallError<serde_json::Value>>()
        else {
            panic!("not the daemon's error");
        };

        assert_eq!(error["message"], "boom");
    }
}
