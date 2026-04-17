#[cfg(not(target_os = "windows"))]
use std::path::PathBuf;
use std::str::FromStr;

impl std::fmt::Display for Endpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match *self {
            Endpoint::Direct(ref uri) => {
                write!(f, "{}", uri)
            },
            #[cfg(not(target_os = "windows"))]
            Endpoint::Socket(ref socket) => {
                write!(f, "{}", socket.display())
            },
        }
    }
}

impl FromStr for Endpoint {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        const TCP_START: &str = "tcp://";

        let endpoint = if let Some(stripped) = s.strip_prefix(TCP_START) {
            let uri = format!("http://{}", stripped);

            Endpoint::Direct(
                uri.parse()
                    .map_err(|error| format!("Failed to convert `{}` to URL: {}", uri, error))?,
            )
        } else {
            #[cfg(target_os = "windows")]
            {
                return Err(format!(
                    "On Windows, you can connect to docker with tcp. You tried to connect with \"{}\"",
                    s
                ));
            }

            #[cfg(not(target_os = "windows"))]
            {
                if s.is_empty() {
                    return Err("Docker socket cannot be empty".to_owned());
                }

                Endpoint::Socket(PathBuf::from(s))
            }
        };

        Ok(endpoint)
    }
}

#[derive(Clone, Debug)]
pub enum Endpoint {
    Direct(http::Uri),
    #[cfg(not(target_os = "windows"))]
    Socket(PathBuf),
}
