use std::path::PathBuf;
use std::time::Duration;

use color_eyre::{Section as _, eyre};
use http::StatusCode;
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

use crate::docker::config::{Config, Endpoint};
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
            Endpoint::Direct { url, timeout } => {
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
            Endpoint::Socket(path_buf) => {
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

    /// Get all containers based on a filter.
    ///
    /// # Errors
    ///
    /// This function will return an error if:
    ///
    /// * Failure to send the request
    /// * Response is not success
    pub async fn get_containers(&self, filters: &Filters) -> Result<Vec<Container>, eyre::Report> {
        let path_and_query = format!("/containers/json?filters={}", &url_encode(&filters)?);

        let response = self.send_request(&path_and_query, Method::GET).await?;

        let bytes = response.collect().await?.to_bytes();

        let result = serde_json::from_slice::<Vec<Container>>(&bytes).inspect_err(|error| {
            event!(Level::ERROR, ?error, message = %String::from_utf8_lossy(&bytes), "Failed to deserialize response");
        })?;

        Ok(result)
    }

    /// Inspect container.
    ///
    /// # Errors
    ///
    /// This function will return an error if:
    ///
    /// * Failure to send the request
    /// * Response is not success
    pub async fn inspect_container(&self, id: &str) -> Result<ContainerInspect, eyre::Report> {
        let path_and_query = format!("/containers/{}/json", id);

        let response = self.send_request(&path_and_query, Method::GET).await?;

        let bytes = response.collect().await?.to_bytes();

        let result = serde_json::from_slice::<ContainerInspect>(&bytes).inspect_err(|error| {
            event!(Level::ERROR, ?error, message = %String::from_utf8_lossy(&bytes), "Failed to deserialize response");
        })?;

        Ok(result)
    }

    /// Get all networks.
    ///
    /// # Errors
    ///
    /// This function will return an error if:
    ///
    /// * Failure to send the request
    /// * Response is not success
    pub async fn list_networks(&self) -> Result<Vec<NetworkSummary>, eyre::Report> {
        let response = self.send_request("/networks", Method::GET).await?;

        let bytes = response.collect().await?.to_bytes();

        let result = serde_json::from_slice::<Vec<NetworkSummary>>(&bytes).inspect_err(|error| {
            event!(Level::ERROR, ?error, message = %String::from_utf8_lossy(&bytes), "Failed to deserialize response");
        })?;

        Ok(result)
    }

    /// Inspect network.
    ///
    /// # Errors
    ///
    /// This function will return an error if:
    ///
    /// * Failure to send the request
    /// * Response is not success
    pub async fn inspect_network(&self, id: &str) -> Result<NetworkInspect, eyre::Report> {
        let path_and_query = format!("/networks/{id}");

        let response = self.send_request(&path_and_query, Method::GET).await?;

        let status_code = response.status();

        let bytes = response.collect().await?.to_bytes();

        if StatusCode::is_success(&status_code) {
            let result = serde_json::from_slice::<NetworkInspect>(&bytes).inspect_err(|error| {
                event!(Level::ERROR, ?error, message = %String::from_utf8_lossy(&bytes), "Failed to deserialize response");
            })?;

            Ok(result)
        } else {
            event!(Level::ERROR, %status_code, message = %String::from_utf8_lossy(&bytes), "Invalid response");

            Err(eyre::Report::msg(format!(
                "Tried to inspect network but it failed with {}",
                status_code
            )))
        }
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
    ) -> Result<(), eyre::Report> {
        let path_and_query = format!(
            "/containers/{}/restart?t={}",
            container_id,
            timeout.as_secs()
        );

        let response = self.send_request(&path_and_query, Method::POST).await?;

        let status_code = response.status();

        if StatusCode::is_success(&status_code) {
            Ok(())
        } else {
            Err(eyre::Report::msg(format!(
                "Tried to restart container but it failed with {:?}",
                status_code
            )))
        }
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
