use std::net::IpAddr;
#[cfg(unix)]
use std::path::PathBuf;

use url::Url;

use crate::config::{ParseError, TcpAddress, TcpHost, Transport};

const DEFAULT_TCP_HOST: &str = "localhost";
const DEFAULT_TCP_PORT: u16 = 2375;
#[cfg(unix)]
const DEFAULT_UNIX_SOCKET: &str = "/var/run/docker.sock";
#[cfg(windows)]
const DEFAULT_NAMED_PIPE: &str = "//./pipe/docker_engine";

pub(super) fn parse_host(value: &str) -> Result<Transport, ParseError> {
    let value = value.trim();

    if value.is_empty() {
        #[cfg(unix)]
        return Ok(Transport::Unix(PathBuf::from(DEFAULT_UNIX_SOCKET)));
        #[cfg(windows)]
        return Ok(Transport::NamedPipe(DEFAULT_NAMED_PIPE.to_owned()));
    }

    let (proto, addr) = value.split_once("://").unwrap_or(("tcp", value));

    match proto {
        "tcp" => parse_tcp_addr(value, addr).map(|address| Transport::Tcp { address, tls: None }),
        #[cfg(unix)]
        "unix" => parse_socket_addr("unix", addr, DEFAULT_UNIX_SOCKET)
            .map(|path| Transport::Unix(PathBuf::from(path))),
        #[cfg(windows)]
        "npipe" => parse_socket_addr("npipe", addr, DEFAULT_NAMED_PIPE).map(Transport::NamedPipe),
        #[cfg(not(unix))]
        "unix" => Err(ParseError::UnsupportedHost(value.to_owned())),
        #[cfg(not(windows))]
        "npipe" => Err(ParseError::UnsupportedHost(value.to_owned())),
        "fd" | "ssh" => Err(ParseError::UnsupportedHost(value.to_owned())),
        _ => Err(ParseError::InvalidBindAddress(value.to_owned())),
    }
}

fn parse_socket_addr(
    proto: &'static str,
    addr: &str,
    default_addr: &str,
) -> Result<String, ParseError> {
    let addr = addr
        .strip_prefix(proto)
        .and_then(|rest| rest.strip_prefix("://"))
        .unwrap_or(addr);

    if addr.contains("://") {
        return Err(ParseError::InvalidProto {
            expected: proto,
            address: addr.to_owned(),
        });
    }

    let addr = if addr.is_empty() { default_addr } else { addr };

    Ok(addr.to_owned())
}

fn parse_tcp_addr(value: &str, addr: &str) -> Result<TcpAddress, ParseError> {
    if addr.is_empty() {
        return Ok(TcpAddress {
            host: TcpHost::Name(DEFAULT_TCP_HOST.to_owned()),
            port: DEFAULT_TCP_PORT,
            base_path: String::new(),
        });
    }

    if addr.contains("://") {
        return Err(ParseError::InvalidProto {
            expected: "tcp",
            address: addr.to_owned(),
        });
    }

    // `url` rejects a port without a host
    let url = if addr.starts_with(':') {
        Url::parse(&format!("tcp://{}{}", DEFAULT_TCP_HOST, addr))
    } else {
        Url::parse(&format!("tcp://{}", addr))
    };

    let Ok(url) = url else {
        return Err(ParseError::InvalidBindAddress(value.to_owned()));
    };

    let host = match url.host() {
        None | Some(url::Host::Domain("")) => TcpHost::Name(DEFAULT_TCP_HOST.to_owned()),
        // `tcp` is a non-special scheme, so `url` returns an IPv4 host as a domain
        Some(url::Host::Domain(domain)) => domain
            .parse()
            .map_or_else(|_| TcpHost::Name(domain.to_owned()), TcpHost::Ip),
        Some(url::Host::Ipv4(address)) => TcpHost::Ip(IpAddr::V4(address)),
        Some(url::Host::Ipv6(address)) => TcpHost::Ip(IpAddr::V6(address)),
    };

    Ok(TcpAddress {
        host,
        port: url.port().unwrap_or(DEFAULT_TCP_PORT),
        base_path: url.path().trim_end_matches('/').to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use crate::config::host::parse_host;
    use crate::config::{Endpoint, ParseError, Transport};

    fn render(value: &str) -> String {
        Endpoint {
            transport: parse_host(value).unwrap(),
        }
        .to_string()
    }

    #[test]
    fn invalid_hosts_are_rejected() {
        for value in [
            "something with spaces",
            "://",
            "unknown://",
            "tcp://:port",
            "tcp://invalid:port",
            "tcp:a.b.c.d",
            "tcp:a.b.c.d/path",
        ] {
            let Err(_) = parse_host(value) else {
                panic!("not an error: `{}`", value);
            };
        }
    }

    #[test]
    fn unknown_proto_is_an_invalid_bind_address() {
        let Err(error) = parse_host("udp://127.0.0.1") else {
            panic!("not an error");
        };

        assert_eq!(
            error.to_string(),
            "Invalid bind address format: udp://127.0.0.1"
        );
    }

    #[test]
    fn nested_proto_is_an_invalid_proto() {
        let Err(error) = parse_host("tcp://unix:///run/docker.sock") else {
            panic!("not an error");
        };

        assert_eq!(
            error.to_string(),
            "Invalid proto, expected tcp: unix:///run/docker.sock"
        );
    }

    #[test]
    fn fd_and_ssh_are_unsupported() {
        for value in ["fd://", "fd://3", "ssh://user@example.com"] {
            assert!(
                matches!(parse_host(value), Err(ParseError::UnsupportedHost(_))),
                "`{}` is unsupported",
                value
            );
        }
    }

    #[test]
    fn tcp_hosts_are_completed() {
        for (value, expected) in [
            ("tcp://", "tcp://localhost:2375"),
            ("tcp://host:", "tcp://host:2375"),
            ("tcp://:2375", "tcp://localhost:2375"),
            ("tcp://:2376", "tcp://localhost:2376"),
            ("tcp://0.0.0.0:8080", "tcp://0.0.0.0:8080"),
            ("tcp://192.168.0.0:12000", "tcp://192.168.0.0:12000"),
            ("tcp://192.168:8080", "tcp://192.168:8080"),
            (" tcp://:7777/path ", "tcp://localhost:7777/path"),
            ("tcp://docker.com:2375", "tcp://docker.com:2375"),
            ("0.0.0.1:", "tcp://0.0.0.1:2375"),
            ("0.0.0.1:5555/path", "tcp://0.0.0.1:5555/path"),
            ("[::1]:", "tcp://[::1]:2375"),
            ("[::1]:5555/path", "tcp://[::1]:5555/path"),
            ("[0:0:0:0:0:0:0:1]:5555", "tcp://[::1]:5555"),
            (":6666", "tcp://localhost:6666"),
            (":6666/path", "tcp://localhost:6666/path"),
            ("localhost:", "tcp://localhost:2375"),
            ("localhost:5555/path", "tcp://localhost:5555/path"),
        ] {
            assert_eq!(render(value), expected);
        }
    }

    #[test]
    fn base_path_drops_dot_segments_and_the_trailing_slash() {
        assert_eq!(
            render("tcp://example.com:2375/a/./b/../c/"),
            "tcp://example.com:2375/a/c"
        );
    }

    #[test]
    fn localhost_and_loopback_addresses_are_loopback() {
        for (value, expected) in [
            ("tcp://localhost:2375", true),
            ("tcp://LOCALHOST:2375", true),
            ("tcp://127.0.0.1:2375", true),
            ("tcp://127.1.2.3:2375", true),
            ("tcp://[::1]:2375", true),
            ("tcp://example.com:2375", false),
            ("tcp://192.168.0.1:2375", false),
            ("tcp://[::2]:2375", false),
        ] {
            let Ok(Transport::Tcp { address, .. }) = parse_host(value) else {
                panic!("not a TCP host: `{}`", value);
            };

            assert_eq!(address.host.is_loopback(), expected, "`{}`", value);
        }
    }

    #[cfg(unix)]
    #[test]
    fn empty_host_is_the_default_socket() {
        for value in ["", " "] {
            assert_eq!(render(value), "unix:///var/run/docker.sock");
        }
    }

    #[cfg(unix)]
    #[test]
    fn unix_hosts_default_the_path() {
        for (value, expected) in [
            ("unix://", "unix:///var/run/docker.sock"),
            ("unix://path/to/socket", "unix://path/to/socket"),
            ("unix:///run/docker.sock", "unix:///run/docker.sock"),
        ] {
            assert_eq!(render(value), expected);
        }
    }

    #[cfg(unix)]
    #[test]
    fn unix_host_rejects_another_proto() {
        for value in ["unix://tcp://127.0.0.1", "unix://unix://tcp://127.0.0.1"] {
            let Err(error) = parse_host(value) else {
                panic!("not an error: `{}`", value);
            };

            assert_eq!(
                error.to_string(),
                "Invalid proto, expected unix: tcp://127.0.0.1"
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn empty_host_is_the_default_named_pipe() {
        assert_eq!(render(""), "npipe:////./pipe/docker_engine");
    }

    #[cfg(windows)]
    #[test]
    fn npipe_hosts_default_the_pipe() {
        for (value, expected) in [
            ("npipe://", "npipe:////./pipe/docker_engine"),
            ("npipe:////./pipe/foo", "npipe:////./pipe/foo"),
        ] {
            assert_eq!(render(value), expected);
        }
    }
}
