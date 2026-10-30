use std::convert::Infallible;
use std::str::FromStr;

use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, PercentEncode};
use serde::Deserialize;
use thiserror::Error;

// the unreserved characters of RFC 3986 stay unencoded
const PATH_SEGMENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

fn short(id: &str) -> &str {
    id.get(..12).unwrap_or(id)
}

#[derive(Deserialize, Debug, PartialEq, Eq, Hash)]
pub struct ContainerId(Box<str>);

impl ContainerId {
    #[must_use]
    pub fn new<I: Into<Box<str>>>(id: I) -> Self {
        Self(id.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn as_short(&self) -> &str {
        short(&self.0)
    }
}

impl std::fmt::Display for ContainerId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A container ID or name.
///
/// Converting a `&str` fails unless it matches `/?[a-zA-Z0-9][a-zA-Z0-9_.-]+`, the container name grammar of the Docker Engine API. The conversion drops the leading `/`.
pub struct ContainerRef<'r>(&'r str);

#[derive(Debug, Error)]
#[error("Not a container ID or name: `{}`", .0)]
pub struct InvalidContainerRef(Box<str>);

impl<'r> ContainerRef<'r> {
    pub(crate) fn as_str(&self) -> &'r str {
        self.0
    }
}

impl<'r> From<&'r ContainerId> for ContainerRef<'r> {
    fn from(id: &'r ContainerId) -> Self {
        ContainerRef(id.as_str())
    }
}

impl<'r> TryFrom<&'r str> for ContainerRef<'r> {
    type Error = InvalidContainerRef;

    fn try_from(id_or_name: &'r str) -> Result<Self, Self::Error> {
        let without_slash = id_or_name.strip_prefix('/').unwrap_or(id_or_name);

        let &[first, ref rest @ ..] = without_slash.as_bytes() else {
            return Err(InvalidContainerRef(id_or_name.into()));
        };

        let matches_grammar = first.is_ascii_alphanumeric()
            && !rest.is_empty()
            && rest
                .iter()
                .all(|&byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'));

        if matches_grammar {
            Ok(ContainerRef(without_slash))
        } else {
            Err(InvalidContainerRef(id_or_name.into()))
        }
    }
}

#[derive(Deserialize, Debug, PartialEq, Eq, Hash)]
pub struct NetworkId(Box<str>);

impl NetworkId {
    #[must_use]
    pub fn new<I: Into<Box<str>>>(id: I) -> Self {
        Self(id.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn as_short(&self) -> &str {
        short(&self.0)
    }
}

impl std::fmt::Display for NetworkId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for NetworkId {
    type Err = Infallible;

    fn from_str(id: &str) -> Result<Self, Self::Err> {
        Ok(Self::new(id))
    }
}

/// A network ID or name.
///
/// Converting a `&str` fails for an empty string, `.` and `..`.
pub struct NetworkRef<'r>(&'r str);

#[derive(Debug, Error)]
#[error("Not a network ID or name: `{}`", .0)]
pub struct InvalidNetworkRef(Box<str>);

impl<'r> NetworkRef<'r> {
    pub(crate) fn path_segment(&self) -> PercentEncode<'r> {
        percent_encoding::utf8_percent_encode(self.0, PATH_SEGMENT)
    }
}

impl<'r> From<&'r NetworkId> for NetworkRef<'r> {
    fn from(id: &'r NetworkId) -> Self {
        NetworkRef(id.as_str())
    }
}

impl<'r> TryFrom<&'r str> for NetworkRef<'r> {
    type Error = InvalidNetworkRef;

    fn try_from(id_or_name: &'r str) -> Result<Self, Self::Error> {
        // `url` resolves `.` and `..` as path segments, percent-encoded or not
        if matches!(id_or_name, "" | "." | "..") {
            Err(InvalidNetworkRef(id_or_name.into()))
        } else {
            Ok(NetworkRef(id_or_name))
        }
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use crate::models::id::{ContainerId, ContainerRef, NetworkId, NetworkRef};

    #[test]
    fn as_short_truncates_to_twelve() {
        let id =
            ContainerId("0f9fc026ac7481ed0d6fc51f34cf7db5d821a3942cf719cbeef5925f853ab8e8".into());

        assert_eq!(id.as_short(), "0f9fc026ac74");
    }

    #[test]
    fn as_short_passes_shorter_id_through() {
        assert_eq!(NetworkId("abc".into()).as_short(), "abc");
    }

    #[test]
    fn container_ref_accepts_a_full_id() {
        let id = "0f9fc026ac7481ed0d6fc51f34cf7db5d821a3942cf719cbeef5925f853ab8e8";

        assert_eq!(ContainerRef::try_from(id).unwrap().as_str(), id);
    }

    #[test]
    fn container_ref_drops_the_leading_slash() {
        assert_eq!(
            ContainerRef::try_from("/ubuntu").unwrap().as_str(),
            "ubuntu"
        );
    }

    #[test]
    fn container_ref_rejects_what_the_name_grammar_rejects() {
        for id_or_name in [
            "",
            "/",
            ".",
            "..",
            "u",
            "-ubuntu",
            "//ubuntu",
            "victim/kill?signal=KILL#",
        ] {
            assert!(
                ContainerRef::try_from(id_or_name).is_err(),
                "{}",
                id_or_name
            );
        }
    }

    #[test]
    fn network_ref_keeps_unreserved_characters() {
        assert_eq!(
            NetworkRef::try_from("my_network.v-1~x")
                .unwrap()
                .path_segment()
                .to_string(),
            "my_network.v-1~x"
        );
    }

    #[test]
    fn network_ref_percent_encodes_reserved_characters() {
        assert_eq!(
            NetworkRef::try_from("a/b?c#d e")
                .unwrap()
                .path_segment()
                .to_string(),
            "a%2Fb%3Fc%23d%20e"
        );
    }

    #[test]
    fn network_ref_rejects_dot_segments_and_an_empty_string() {
        for id_or_name in ["", ".", ".."] {
            assert!(NetworkRef::try_from(id_or_name).is_err(), "{}", id_or_name);
        }
    }
}
