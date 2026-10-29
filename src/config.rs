mod host;

use std::fmt::{self, Display};
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::str::FromStr;

use thiserror::Error;

/// A Docker endpoint, parsed from a `DOCKER_HOST` value.
pub struct Endpoint {
    transport: Transport,
}

impl Endpoint {
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

#[derive(Debug, Error)]
pub enum ParseError {
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
    #[cfg(unix)]
    use crate::config::{Endpoint, Tls, Transport};

    #[cfg(unix)]
    #[test]
    fn socket_ignores_tls() {
        let endpoint = "unix:///run/docker.sock"
            .parse::<Endpoint>()
            .unwrap()
            .with_tls(Tls {
                verify: true,
                ca_cert: None,
                client_credentials: None,
            });

        assert!(
            matches!(endpoint.transport, Transport::Unix(_)),
            "a socket has no TLS"
        );
    }
}
