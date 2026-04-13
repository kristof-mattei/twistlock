use std::net::{Ipv4Addr, Ipv6Addr};
use std::str::FromStr;

use hashbrown::HashMap;
use serde::de::Error;
use serde::{Deserialize, Deserializer};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct ContainerInspect {
    pub name: Box<str>,
    pub id: Box<str>,
    pub config: ContainerConfig,
    pub state: ContainerState,
    pub network_settings: ContainerNetworkSettings,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct ContainerConfig {
    pub hostname: Box<str>,
    pub labels: HashMap<Box<str>, Box<str>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct ContainerState {
    pub running: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct ContainerNetworkSettings {
    pub networks: HashMap<Box<str>, ContainerNetwork>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct ContainerNetwork {
    #[serde(
        rename(deserialize = "IPAddress"),
        deserialize_with = "deserialize_empty_as_none"
    )]
    pub ip_address: Option<Ipv4Addr>,

    #[serde(
        rename(deserialize = "GlobalIPv6Address"),
        deserialize_with = "deserialize_empty_as_none"
    )]
    pub global_ipv6_address: Option<Ipv6Addr>,
}

// Docker passes empty strings if value absent
fn deserialize_empty_as_none<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: FromStr,
    T::Err: std::fmt::Display,
{
    // both absent and "" are None
    match Option::<&str>::deserialize(deserializer)? {
        None | Some("") => Ok(None),
        Some(s) => T::from_str(s).map(Some).map_err(Error::custom),
    }
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, Ipv6Addr};

    use pretty_assertions::assert_eq;

    use super::ContainerNetwork;

    fn parse(json: &str) -> Result<ContainerNetwork, serde_json::Error> {
        serde_json::from_str(json)
    }

    #[test]
    fn ipv4_only() {
        let container_network =
            parse(r#"{"IPAddress":"192.168.1.1","GlobalIPv6Address":""}"#).unwrap();

        assert_eq!(
            container_network.ip_address,
            Some(Ipv4Addr::new(192, 168, 1, 1))
        );
        assert_eq!(container_network.global_ipv6_address, None);
    }

    #[test]
    fn ipv6_only() {
        let container_network =
            parse(r#"{"IPAddress":"","GlobalIPv6Address":"2001:db8::1"}"#).unwrap();

        assert_eq!(container_network.ip_address, None);
        assert_eq!(
            container_network.global_ipv6_address,
            Some(Ipv6Addr::new(0x2001, 0x0db8, 0, 0, 0, 0, 0, 1))
        );
    }

    #[test]
    fn both_ipv4_and_ipv6() {
        let container_network =
            parse(r#"{"IPAddress":"10.0.0.2","GlobalIPv6Address":"fe80::1"}"#).unwrap();

        assert_eq!(
            container_network.ip_address,
            Some(Ipv4Addr::new(10, 0, 0, 2))
        );
        assert_eq!(
            container_network.global_ipv6_address,
            Some(Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 1))
        );
    }

    #[test]
    fn both_empty_strings() {
        let container_network = parse(r#"{"IPAddress":"","GlobalIPv6Address":""}"#).unwrap();

        assert_eq!(container_network.ip_address, None);
        assert_eq!(container_network.global_ipv6_address, None);
    }

    #[test]
    fn invalid_ipv4_is_error() {
        parse(r#"{"IPAddress":"not-an-ip","GlobalIPv6Address":""}"#).unwrap_err();
    }

    #[test]
    fn invalid_ipv6_is_error() {
        parse(r#"{"IPAddress":"","GlobalIPv6Address":"not-an-ipv6"}"#).unwrap_err();
    }
}
