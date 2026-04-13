#[cfg(not(target_os = "windows"))]
use std::path::PathBuf;
use std::time::Duration;

#[derive(Clone, Debug)]
pub enum RawEndpoint {
    Direct(http::Uri),
    #[cfg(not(target_os = "windows"))]
    Socket(PathBuf),
}

impl std::fmt::Display for RawEndpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match *self {
            RawEndpoint::Direct(ref uri) => {
                write!(f, "{}", uri)
            },
            #[cfg(not(target_os = "windows"))]
            RawEndpoint::Socket(ref socket) => {
                write!(f, "{}", socket.display())
            },
        }
    }
}

#[expect(unused, reason = "WIP")]
fn parse_docker(value: &str) -> Result<RawEndpoint, String> {
    const TCP_START: &str = "tcp://";

    let endpoint = if let Some(stripped) = value.strip_prefix(TCP_START) {
        let uri = format!("http://{}", stripped);

        RawEndpoint::Direct(
            uri.parse()
                .map_err(|error| format!("Failed to convert `{}` to URL: {}", uri, error))?,
        )
    } else {
        #[cfg(target_os = "windows")]
        {
            return Err(format!(
                "On Windows, you can connect to docker with tcp. You tried to connect with \"{}\"",
                value
            ));
        }

        #[cfg(not(target_os = "windows"))]
        {
            if value.is_empty() {
                return Err("Docker socket cannot be empty".to_owned());
            }

            RawEndpoint::Socket(PathBuf::from(value))
        }
    };

    Ok(endpoint)
}

pub struct Config {
    pub endpoint: Endpoint,
}

pub enum Endpoint {
    Direct {
        url: http::Uri,
        timeout: Duration,
    },
    #[cfg(not(target_os = "windows"))]
    Socket(PathBuf),
}

impl Config {
    pub fn build(raw_endpoint: RawEndpoint, timeout: Duration) -> Config {
        let endpoint = match raw_endpoint {
            RawEndpoint::Direct(uri) => Endpoint::Direct { url: uri, timeout },
            #[cfg(not(target_os = "windows"))]
            RawEndpoint::Socket(path_buf) => Endpoint::Socket(path_buf),
        };

        Config { endpoint }
    }
}
