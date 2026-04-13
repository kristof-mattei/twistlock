use std::path::PathBuf;
use std::time::Duration;

use color_eyre::{Section as _, eyre};
use http_body_util::{BodyExt as _, Full};
use hyper::body::{Bytes, Incoming};
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
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;
use tracing::{Level, event};

use crate::docker::config::{Config, Endpoint as ConfigEndpoint};
use crate::docker::endpoint::{Endpoint, EndpointCallError};
use crate::docker::endpoints::containers::{
    InspectContainer, ListContainers, RestartContainer, RestartContainerRequest,
};
use crate::docker::endpoints::networks::{InspectNetwork, ListNetworks};
use crate::filters::Filters;
use crate::http_client;
use crate::http_client::{build_request, execute_request};
use crate::models::container::Container;
use crate::models::container_inspect::ContainerInspect;
use crate::models::events::Event;
use crate::models::network::{NetworkInspect, NetworkSummary};

pub enum DockerEndpoint {
    Socket(HttpClient<UnixSocketConnector<PathBuf>, Full<Bytes>>),
    Tls(HttpClient<HttpsConnector<HttpConnector>, Full<Bytes>>),
}

pub struct DockerClient {}

struct ClientCredentials {
    key: PrivateKeyDer<'static>,
    certs: Vec<CertificateDer<'static>>,
}

fn build_root_cert_store(cacert: Option<PathBuf>) -> Result<RootCertStore, eyre::Report> {
    let mut store = RootCertStore::empty();

    if let Some(cacert) = cacert {
        store.add(CertificateDer::from_pem_file(cacert)?)?;
    } else {
        let native_certs = rustls_native_certs::load_native_certs();

        for error in native_certs.errors {
            event!(Level::ERROR, ?error, "Failed to load certificate");
        }

        for cert in native_certs.certs {
            store.add(cert).unwrap();
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

pub struct Client {
    pub endpoint: DockerEndpoint,
    pub uri: http::Uri,
    pub docker_timeout: Duration,
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
        config: Config,
        cacert: Option<PathBuf>,
        client_key: Option<PathBuf>,
        client_cert: Option<PathBuf>,
        timeout: Duration,
    ) -> Result<Client, eyre::Report> {
        let daemon = match config.endpoint {
            ConfigEndpoint::Direct { url, timeout } => {
                let client_credentials = match (client_cert, client_key) {
                    (Some(client_cert), Some(client_key)) => Some(ClientCredentials {
                        key: PrivateKeyDer::from_pem_file(client_key)?,
                        certs: vec![CertificateDer::from_pem_file(client_cert)?],
                    }),
                    _ => None,
                };

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

    async fn send_request(
        &self,
        path_and_query: &str,
        method: Method,
    ) -> Result<Response<Incoming>, eyre::Report> {
        let request = build_request(self.uri.clone(), path_and_query, method)?;

        match self.endpoint {
            DockerEndpoint::Tls(ref client) => {
                let response = execute_request(client, request);

                match timeout(self.docker_timeout, response).await {
                    Ok(Ok(response)) => Ok(response),
                    Ok(Err(error)) => Err(error.into()),
                    Err(error) => Err(error.into()),
                }
            },
            DockerEndpoint::Socket(ref client) => {
                let response = execute_request(client, request);

                match timeout(self.docker_timeout, response).await {
                    Ok(Ok(response)) => Ok(response),
                    Ok(Err(error)) => Err(error.into()),
                    Err(error) => Err(error.into()),
                }
            },
        }
    }

    /// Call a typed [`Endpoint`], returning a structured error on failure.
    ///
    /// # Errors
    ///
    /// This function will return an error if:
    ///
    /// * Failure to build the path and query
    /// * Failure to send the request
    /// * Response is not success
    /// * Failed to deserialize the response
    pub async fn call<E: Endpoint>(
        &self,
        request: &E::Request,
    ) -> Result<E::Response, EndpointCallError<E::Error>> {
        let path_and_query = E::path_and_query(request)
            .map_err(|error| EndpointCallError::Transport(error.into()))?;

        let response = self
            .send_request(&path_and_query, E::METHOD)
            .await
            .map_err(EndpointCallError::Transport)?;

        let status_code = response.status();

        let bytes = response
            .collect()
            .await
            .map_err(|error| EndpointCallError::Transport(error.into()))?
            .to_bytes();

        if status_code.is_success() {
            E::parse_response(&bytes).map_err(|error| {
                event!(Level::ERROR, ?error, message = %String::from_utf8_lossy(&bytes), "Failed to deserialize response");
                EndpointCallError::Transport(error.into())
            })
        } else {
            if let Ok(typed_err) = serde_json::from_slice::<E::Error>(&bytes) {
                return Err(EndpointCallError::Typed(typed_err));
            }

            if let Ok(generic) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                return Err(EndpointCallError::Generic(generic));
            }

            let body = String::from_utf8_lossy(&bytes).into_owned();

            event!(Level::ERROR, %status_code, message = %body, "Invalid response");

            Err(EndpointCallError::HttpError {
                status: status_code,
                body,
            })
        }
    }

    /// Get all containers based on a filter.
    ///
    /// # Errors
    ///
    /// This function will return an error if:
    ///
    /// * Failure to send the request
    /// * Response is not success
    pub async fn get_containers(
        &self,
        filters: &Filters,
    ) -> Result<Vec<Container>, EndpointCallError<<ListContainers as Endpoint>::Error>> {
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
    pub async fn inspect_container(
        &self,
        id: &str,
    ) -> Result<ContainerInspect, EndpointCallError<<InspectContainer as Endpoint>::Error>> {
        self.call::<InspectContainer>(id).await
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
    ) -> Result<Vec<NetworkSummary>, EndpointCallError<<ListNetworks as Endpoint>::Error>> {
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
    pub async fn inspect_network(
        &self,
        id: &str,
    ) -> Result<NetworkInspect, EndpointCallError<<InspectContainer as Endpoint>::Error>> {
        self.call::<InspectNetwork>(id).await
    }

    /// Restart a container.
    ///
    /// # Errors
    ///
    /// This function will return an error if:
    ///
    /// * Failure to send the request
    /// * Response is not success
    pub async fn restart_container(
        &self,
        container_id: &str,
        timeout: std::time::Duration,
    ) -> Result<(), EndpointCallError<<RestartContainer as Endpoint>::Error>> {
        self.call::<RestartContainer>(&RestartContainerRequest {
            id: container_id.to_owned(),
            timeout,
        })
        .await
    }

    /// Listen for events.
    ///
    /// # Errors
    ///
    /// This function will return an error if:
    ///
    /// * Failure to send the request
    /// * Failed to decode JSON line
    pub async fn produce_events(
        &self,
        sender: tokio::sync::mpsc::Sender<Event>,
        cancellation_token: &CancellationToken,
    ) -> Result<(), eyre::Report> {
        let path_and_query = format!("/events{}", "");

        let mut response = self.send_request(&path_and_query, Method::GET).await?;

        let mut buffer = Vec::<u8>::new();

        // Inspired by https://github.com/EmbarkStudios/wasmtime/blob/056ccdec94f89d00325970d1239429a1b39ec729/crates/wasi-http/src/http_impl.rs#L246-L268
        loop {
            let frame = tokio::select! {
                frame = response.frame() => frame,
                () = cancellation_token.cancelled() => {
                    return Ok(());
                },
            };

            let frame = match frame {
                Some(Ok(frame)) => frame,
                Some(Err(error)) => {
                    event!(Level::ERROR, ?error, "Failed to read frame");

                    continue;
                },
                None => {
                    // TODO is this correct? If the server stops?
                    return Err(eyre::Report::msg("No more next frame, other side gone"));
                },
            };

            let Ok(data) = frame.into_data() else {
                // frame is trailers, ignored
                continue;
            };

            buffer.extend_from_slice(&data);

            while let Some(i) = buffer.iter().position(|b| b == &b'\n') {
                Client::decode_send(&buffer[0..=i], &sender).await?;

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
        data: &[u8],
        sender: &tokio::sync::mpsc::Sender<Event>,
    ) -> Result<(), eyre::Report> {
        event!(Level::TRACE, data = %String::from_utf8_lossy(data), "New event");

        let decoded = match serde_json::from_slice(data) {
            Ok(event) => event,
            Err(error) => {
                event!(
                    Level::ERROR,
                    ?error,
                    data = %String::from_utf8_lossy(data),
                    "Failed to parse json to struct"
                );

                return Ok(());
            },
        };

        sender
            .send(decoded)
            .await
            .map_err(|error| eyre::Report::msg("Channel closed").error(error))
    }
}
