use std::path::PathBuf;
use std::time::Duration;

use color_eyre::eyre;
use http_body_util::{BodyExt as _, Full};
use hyper::body::{Bytes, Incoming};
use hyper::{Method, Response, StatusCode};
use hyper_rustls::{FixedServerNameResolver, HttpsConnector, HttpsConnectorBuilder};
use hyper_unix_socket::UnixSocketConnector;
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use rustls::client::ClientConfig;
use rustls::pki_types::pem::PemObject as _;
use rustls::pki_types::{CertificateDer, DnsName, PrivateKeyDer, ServerName};
use rustls::{DEFAULT_VERSIONS, RootCertStore};
use tokio::time::timeout;
use tracing::{Level, event};

use crate::docker::container::Container;
use crate::http_client;
use crate::http_client::{build_request, execute_request};

pub enum DockerEndpoint {
    Socket(Client<UnixSocketConnector<PathBuf>, Full<Bytes>>),
    Tls(Client<HttpsConnector<HttpConnector>, Full<Bytes>>),
}

pub struct DockerClient {
    pub endpoint: DockerEndpoint,
    pub uri: http::Uri,
    pub docker_timeout: Duration,
}

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

impl DockerClient {
    /// Build a new client.
    ///
    /// # Errors
    ///
    /// * Invalid certificate path / setup
    /// * `docker_socket_or_uri` is not a valid path or `Uri`
    #[expect(clippy::missing_panics_doc, reason = "Not needed")]
    pub fn build(
        docker_socket_or_uri: String,
        cacert: Option<PathBuf>,
        client_key: Option<PathBuf>,
        client_cert: Option<PathBuf>,
        timeout: Duration,
    ) -> Result<DockerClient, eyre::Report> {
        const TCP_START: &str = "tcp://";

        let endpoint = if docker_socket_or_uri.starts_with(TCP_START) {
            let mut docker_socket_or_uri = docker_socket_or_uri;
            docker_socket_or_uri.replace_range(..TCP_START.len(), "https://");

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

            DockerClient {
                endpoint: DockerEndpoint::Tls(http_client::build_client(connector)),
                uri: docker_socket_or_uri.parse()?,
                docker_timeout: timeout,
            }
        } else {
            // we're connecting over a socket, so the url is localhost

            let connector: UnixSocketConnector<PathBuf> =
                UnixSocketConnector::new(PathBuf::from(docker_socket_or_uri));

            DockerClient {
                endpoint: DockerEndpoint::Socket(http_client::build_client(connector)),
                uri: http::Uri::from_static("http://localhost"),
                docker_timeout: timeout,
            }
        };

        Ok(endpoint)
    }

    /// Gets all containers based on a filter:
    ///
    /// # Errors
    ///
    /// This function will return an error if:
    ///
    /// * Failure to send the request
    /// * Response is not success
    pub async fn get_containers(
        &self,
        encoded_filters: &str,
    ) -> Result<Vec<Container>, eyre::Report> {
        let path_and_query = format!("/containers/json?filters={}", encoded_filters);

        let response = self.send_request(&path_and_query, Method::GET).await?;

        let bytes = response.collect().await?.to_bytes();

        let result = serde_json::from_slice::<Vec<Container>>(&bytes)?;

        Ok(result)
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
        timeout: u32,
    ) -> Result<(), eyre::Report> {
        let path_and_query = format!("/containers/{}/restart?t={}", container_id, timeout);

        let response = self.send_request(&path_and_query, Method::POST).await?;

        let status_code = response.status();

        if StatusCode::is_success(&status_code) {
            Ok(())
        } else {
            Err(eyre::Report::msg(format!(
                "Tried to refresh container but it failed with {:?}",
                status_code
            )))
        }
    }
}
