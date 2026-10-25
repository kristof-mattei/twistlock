mod host;

use std::fmt::{self, Display};
use std::net::{IpAddr, SocketAddr};
#[cfg(unix)]
use std::path::PathBuf;
use std::str::FromStr;

use thiserror::Error;

const DOCKER_HOST: &str = "DOCKER_HOST";

/// A Docker endpoint, parsed from a `DOCKER_HOST` value.
pub struct Endpoint {
    transport: Transport,
}

impl Endpoint {
    /// Parses `DOCKER_HOST`. An unset or empty variable is the platform's default socket.
    ///
    /// # Errors
    ///
    /// See [`ParseError`].
    pub fn from_env() -> Result<Endpoint, ParseError> {
        std::env::var_os(DOCKER_HOST)
            .unwrap_or_default()
            .into_string()
            .map_err(|_| ParseError::NotUnicode(DOCKER_HOST))?
            .parse()
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
            Transport::Tcp(ref address) => write!(f, "tcp://{}", address),
        }
    }
}

pub(crate) enum Transport {
    #[cfg(unix)]
    Unix(PathBuf),
    #[cfg(windows)]
    NamedPipe(String),
    Tcp(TcpAddress),
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
