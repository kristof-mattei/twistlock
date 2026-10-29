mod host;

use std::fmt::{self, Display};
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::str::FromStr;

use thiserror::Error;

/// A Docker endpoint, parsed from a `DOCKER_HOST` value.
#[derive(Clone)]
pub struct Endpoint {
    host: Host,
}

impl Endpoint {
    /// Connects over TLS, to `tcp://localhost:2376` for an empty host. A socket ignores `tls`.
    #[must_use]
    pub fn with_tls(self, tls: Tls) -> Endpoint {
        let transport = match self.host {
            Host::Default => Transport::Tcp {
                address: host::default_tls_address(),
                tls: Some(tls),
            },
            #[cfg(unix)]
            Host::Explicit(Transport::Unix(path)) => Transport::Unix(path),
            #[cfg(windows)]
            Host::Explicit(Transport::NamedPipe(pipe)) => Transport::NamedPipe(pipe),
            Host::Explicit(Transport::Tcp { address, .. }) => Transport::Tcp {
                address,
                tls: Some(tls),
            },
        };

        Endpoint {
            host: Host::Explicit(transport),
        }
    }

    pub(crate) fn into_transport(self) -> Transport {
        match self.host {
            Host::Default => host::default_socket(),
            Host::Explicit(transport) => transport,
        }
    }
}

impl FromStr for Endpoint {
    type Err = ParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Endpoint {
            host: host::parse_host(s)?,
        })
    }
}

impl Display for Endpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.host {
            Host::Default => host::default_socket().fmt(f),
            Host::Explicit(ref transport) => transport.fmt(f),
        }
    }
}

#[derive(Clone)]
enum Host {
    /// An empty `DOCKER_HOST`.
    Default,
    Explicit(Transport),
}

#[derive(Clone)]
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

impl Display for Transport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            #[cfg(unix)]
            Transport::Unix(ref path) => write!(f, "unix://{}", path.display()),
            #[cfg(windows)]
            Transport::NamedPipe(ref pipe) => write!(f, "npipe://{}", pipe),
            Transport::Tcp { ref address, .. } => write!(f, "tcp://{}", address),
        }
    }
}

#[derive(Clone)]
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

#[derive(Clone)]
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
#[derive(Clone)]
pub struct Tls {
    /// `--tlsverify`. Without it, any daemon certificate is accepted.
    pub verify: bool,
    /// `--tlscacert`, ignored without `verify`. Without it, the platform verifier checks the daemon certificate.
    pub ca_cert: Option<PathBuf>,
    /// `--tlscert` and `--tlskey`.
    pub client_credentials: Option<ClientCredentialPaths>,
}

#[derive(Clone)]
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
    use pretty_assertions::assert_eq;

    use crate::config::{Endpoint, Host, Tls, Transport};

    fn tls() -> Tls {
        Tls {
            verify: true,
            ca_cert: None,
            client_credentials: None,
        }
    }

    #[cfg(unix)]
    #[test]
    fn socket_ignores_tls() {
        let endpoint = "unix:///run/docker.sock"
            .parse::<Endpoint>()
            .unwrap()
            .with_tls(tls());

        assert!(
            matches!(endpoint.host, Host::Explicit(Transport::Unix(_))),
            "a socket has no TLS"
        );
    }

    #[test]
    fn empty_host_with_tls_is_localhost_2376() {
        let endpoint = "".parse::<Endpoint>().unwrap().with_tls(tls());

        let Host::Explicit(Transport::Tcp {
            address,
            tls: Some(_),
        }) = endpoint.host
        else {
            panic!("not a TLS host");
        };

        assert_eq!(address.to_string(), "localhost:2376");
    }

    #[test]
    fn bare_tcp_host_with_tls_is_localhost_2375() {
        let endpoint = "tcp://".parse::<Endpoint>().unwrap().with_tls(tls());

        assert_eq!(endpoint.to_string(), "tcp://localhost:2375");
    }
}
