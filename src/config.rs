mod host;

use std::ffi::OsString;
use std::fmt::{self, Display};
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::str::FromStr;

use thiserror::Error;

const CA_FILE: &str = "ca.pem";
const CERT_FILE: &str = "cert.pem";
const KEY_FILE: &str = "key.pem";

const DOCKER_CERT_PATH: &str = "DOCKER_CERT_PATH";
const DOCKER_CONFIG: &str = "DOCKER_CONFIG";
const DOCKER_HOST: &str = "DOCKER_HOST";
const DOCKER_TLS: &str = "DOCKER_TLS";
const DOCKER_TLS_VERIFY: &str = "DOCKER_TLS_VERIFY";

#[cfg(not(windows))]
const HOME: &str = "HOME";
#[cfg(windows)]
const HOME: &str = "USERPROFILE";

/// A Docker endpoint, parsed from a `DOCKER_HOST` value.
pub struct Endpoint {
    transport: Transport,
}

impl Endpoint {
    /// Parses `DOCKER_HOST`, and applies `DOCKER_TLS_VERIFY`, `DOCKER_TLS` and `DOCKER_CERT_PATH` like the Docker CLI. An unset or empty `DOCKER_HOST` is the platform's default socket, or `tcp://localhost:2376` with TLS.
    ///
    /// # Errors
    ///
    /// See [`ParseError`].
    pub fn from_env() -> Result<Endpoint, ParseError> {
        Endpoint::from_vars(|key| std::env::var_os(key).filter(|value| !value.is_empty()))
    }

    /// `var` returns a set, non-empty variable.
    fn from_vars<F: Fn(&str) -> Option<OsString>>(var: F) -> Result<Endpoint, ParseError> {
        let host = var(DOCKER_HOST)
            .unwrap_or_default()
            .into_string()
            .map_err(|_| ParseError::NotUnicode(DOCKER_HOST))?;

        let verify = var(DOCKER_TLS_VERIFY).is_some();

        if !verify && var(DOCKER_TLS).is_none() {
            return host.parse();
        }

        let transport = if host.trim().is_empty() {
            Transport::Tcp {
                address: host::default_tls_address(),
                tls: None,
            }
        } else {
            host::parse_host(&host)?
        };

        let cert_dir = var(DOCKER_CERT_PATH)
            .or_else(|| var(DOCKER_CONFIG))
            .map_or_else(
                || PathBuf::from(var(HOME).unwrap_or_default()).join(".docker"),
                PathBuf::from,
            );

        let tls = Tls {
            verify,
            ca_cert: Some(cert_dir.join(CA_FILE)),
            client_credentials: unless_missing(cert_dir.join(CERT_FILE))
                .zip(unless_missing(cert_dir.join(KEY_FILE)))
                .map(|(cert, key)| ClientCredentialPaths { cert, key }),
        };

        Ok(Endpoint { transport }.with_tls(tls))
    }

    /// Connects over TLS. A socket ignores `tls`.
    #[must_use]
    pub fn with_tls(self, tls: Tls) -> Endpoint {
        let transport = match self.transport {
            #[cfg(unix)]
            Transport::Unix(path) => Transport::Unix(path),
            #[cfg(windows)]
            Transport::NamedPipe(pipe) => Transport::NamedPipe(pipe),
            Transport::Tcp { address, .. } => Transport::Tcp {
                address,
                tls: Some(tls),
            },
        };

        Endpoint { transport }
    }

    pub(crate) fn into_transport(self) -> Transport {
        self.transport
    }
}

impl FromStr for Endpoint {
    type Err = ParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Endpoint {
            transport: host::parse_host(s)?,
        })
    }
}

impl Display for Endpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.transport {
            #[cfg(unix)]
            Transport::Unix(ref path) => write!(f, "unix://{}", path.display()),
            #[cfg(windows)]
            Transport::NamedPipe(ref pipe) => write!(f, "npipe://{}", pipe),
            Transport::Tcp { ref address, .. } => write!(f, "tcp://{}", address),
        }
    }
}

pub(crate) enum Transport {
    #[cfg(unix)]
    Unix(PathBuf),
    #[cfg(windows)]
    NamedPipe(String),
    Tcp {
        address: TcpAddress,
        tls: Option<Tls>,
    },
}

pub(crate) struct TcpAddress {
    pub host: TcpHost,
    pub port: u16,
    pub base_path: String,
}

impl Display for TcpAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.host {
            TcpHost::Ip(ip) => write!(f, "{}{}", SocketAddr::new(ip, self.port), self.base_path),
            TcpHost::Name(ref name) => write!(f, "{}:{}{}", name, self.port, self.base_path),
        }
    }
}

pub(crate) enum TcpHost {
    Ip(IpAddr),
    Name(String),
}

impl TcpHost {
    pub(crate) fn is_loopback(&self) -> bool {
        match *self {
            TcpHost::Ip(ip) => ip.is_loopback(),
            TcpHost::Name(ref name) => name.eq_ignore_ascii_case("localhost"),
        }
    }
}

/// The Docker CLI's `--tlsverify`, `--tlscacert`, `--tlscert` and `--tlskey`.
pub struct Tls {
    /// `--tlsverify`. Without it, any daemon certificate is accepted.
    pub verify: bool,
    /// `--tlscacert`, ignored without `verify`. Without it, the platform verifier checks the daemon certificate.
    pub ca_cert: Option<PathBuf>,
    /// `--tlscert` and `--tlskey`.
    pub client_credentials: Option<ClientCredentialPaths>,
}

pub struct ClientCredentialPaths {
    pub cert: PathBuf,
    pub key: PathBuf,
}

fn unless_missing(path: PathBuf) -> Option<PathBuf> {
    let missing =
        std::fs::metadata(&path).is_err_and(|error| error.kind() == io::ErrorKind::NotFound);

    (!missing).then_some(path)
}

#[derive(Debug, Error)]
pub enum ParseError {
    #[error("`{}` is not valid Unicode", .0)]
    NotUnicode(&'static str),
    #[error("Invalid bind address format: {}", .0)]
    InvalidBindAddress(String),
    #[error("Invalid proto, expected {}: {}", .expected, .address)]
    InvalidProto {
        expected: &'static str,
        address: String,
    },
    #[error("Unsupported docker host `{}`", .0)]
    UnsupportedHost(String),
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::path::Path;

    use hashbrown::HashMap;
    use pretty_assertions::assert_eq;
    use tempfile::TempDir;

    use crate::config::{Endpoint, HOME, Tls, Transport};

    fn from_vars<const N: usize, V: Into<OsString>>(vars: [(&'static str, V); N]) -> Endpoint {
        let vars: HashMap<&str, OsString> = vars
            .into_iter()
            .map(|(key, value)| (key, value.into()))
            .collect();

        Endpoint::from_vars(|key| vars.get(key).cloned()).unwrap()
    }

    fn tls(endpoint: Endpoint) -> Option<Tls> {
        let Transport::Tcp { tls, .. } = endpoint.transport else {
            panic!("not a TCP endpoint");
        };

        tls
    }

    fn cert_dir(files: &[&str]) -> TempDir {
        let directory = TempDir::new().unwrap();

        for &file in files {
            std::fs::write(directory.path().join(file), "").unwrap();
        }

        directory
    }

    #[test]
    fn docker_host_without_tls_variables_is_plain_tcp() {
        let endpoint = from_vars([("DOCKER_HOST", "tcp://example.com:2375")]);

        assert!(tls(endpoint).is_none(), "plain TCP has no TLS");
    }

    #[test]
    fn docker_tls_verify_reads_the_cert_path() {
        let directory = cert_dir(&["cert.pem", "key.pem"]);

        let endpoint = from_vars([
            ("DOCKER_HOST", Path::new("tcp://example.com:2376")),
            ("DOCKER_TLS_VERIFY", Path::new("1")),
            ("DOCKER_CERT_PATH", directory.path()),
        ]);

        let Some(tls) = tls(endpoint) else {
            panic!("no TLS");
        };

        let Some(client_credentials) = tls.client_credentials else {
            panic!("no client credentials");
        };

        assert!(tls.verify, "`DOCKER_TLS_VERIFY` verifies the daemon");
        assert_eq!(tls.ca_cert, Some(directory.path().join("ca.pem")));
        assert_eq!(client_credentials.cert, directory.path().join("cert.pem"));
        assert_eq!(client_credentials.key, directory.path().join("key.pem"));
    }

    #[test]
    fn docker_tls_verify_of_zero_verifies() {
        let endpoint = from_vars([
            ("DOCKER_HOST", "tcp://example.com:2376"),
            ("DOCKER_TLS_VERIFY", "0"),
        ]);

        let Some(tls) = tls(endpoint) else {
            panic!("no TLS");
        };

        assert!(tls.verify, "any non-empty `DOCKER_TLS_VERIFY` verifies");
    }

    #[test]
    fn docker_tls_is_tls_without_verification() {
        let endpoint = from_vars([
            ("DOCKER_HOST", "tcp://example.com:2376"),
            ("DOCKER_TLS", "1"),
        ]);

        let Some(tls) = tls(endpoint) else {
            panic!("no TLS");
        };

        assert!(!tls.verify, "`DOCKER_TLS` skips verification");
    }

    #[test]
    fn missing_default_client_file_drops_the_client_credentials() {
        let directory = cert_dir(&["cert.pem"]);

        let endpoint = from_vars([
            ("DOCKER_HOST", Path::new("tcp://example.com:2376")),
            ("DOCKER_TLS_VERIFY", Path::new("1")),
            ("DOCKER_CERT_PATH", directory.path()),
        ]);

        let Some(tls) = tls(endpoint) else {
            panic!("no TLS");
        };

        assert!(
            tls.client_credentials.is_none(),
            "a client certificate needs its key"
        );
        assert_eq!(tls.ca_cert, Some(directory.path().join("ca.pem")));
    }

    #[test]
    fn cert_path_falls_back_to_docker_config() {
        let directory = TempDir::new().unwrap();

        let endpoint = from_vars([
            ("DOCKER_HOST", Path::new("tcp://example.com:2376")),
            ("DOCKER_TLS_VERIFY", Path::new("1")),
            ("DOCKER_CONFIG", directory.path()),
        ]);

        let Some(tls) = tls(endpoint) else {
            panic!("no TLS");
        };

        assert_eq!(tls.ca_cert, Some(directory.path().join("ca.pem")));
    }

    #[test]
    fn cert_path_falls_back_to_the_home_directory() {
        let directory = TempDir::new().unwrap();

        let endpoint = from_vars([
            ("DOCKER_HOST", Path::new("tcp://example.com:2376")),
            ("DOCKER_TLS_VERIFY", Path::new("1")),
            (HOME, directory.path()),
        ]);

        let Some(tls) = tls(endpoint) else {
            panic!("no TLS");
        };

        assert_eq!(
            tls.ca_cert,
            Some(directory.path().join(".docker").join("ca.pem"))
        );
    }

    #[test]
    fn empty_docker_host_with_tls_is_the_tls_port() {
        let endpoint = from_vars([("DOCKER_TLS_VERIFY", "1")]);

        assert_eq!(endpoint.to_string(), "tcp://localhost:2376");
    }

    #[cfg(unix)]
    #[test]
    fn socket_ignores_tls() {
        let endpoint = from_vars([
            ("DOCKER_HOST", "unix:///run/docker.sock"),
            ("DOCKER_TLS_VERIFY", "1"),
        ]);

        assert!(
            matches!(endpoint.transport, Transport::Unix(_)),
            "a socket has no TLS"
        );
    }
}
