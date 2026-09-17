use std::net::{Ipv4Addr, Ipv6Addr};

use hashbrown::HashMap;
use serde::Deserialize;

use crate::models::deserializers::{deserialize_empty_as_none, deserialize_null_as_empty};
use crate::models::id::{ContainerId, NetworkId};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct ContainerInspect {
    pub name: Box<str>,
    pub id: ContainerId,
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
        rename(deserialize = "NetworkID"),
        default,
        deserialize_with = "deserialize_empty_as_none"
    )]
    pub network_id: Option<NetworkId>,

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

    #[serde(
        rename(deserialize = "DNSNames"),
        default,
        deserialize_with = "deserialize_null_as_empty"
    )]
    pub dns_names: Box<[Box<str>]>,
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, Ipv6Addr};

    use pretty_assertions::assert_eq;

    use super::ContainerNetwork;
    use crate::models::id::NetworkId;

    fn parse(json: &str) -> Result<ContainerNetwork, serde_json::Error> {
        serde_json::from_str(json)
    }

    #[test]
    fn network_id_is_parsed() {
        let container_network = parse(
            r#"{"NetworkID":"88cad55e9ed7797a340f76b9a6bd4963c2dea4dc680ab37984f298ae2dfc6c3c","IPAddress":"172.18.0.2","GlobalIPv6Address":""}"#,
        )
        .unwrap();

        assert_eq!(
            container_network.network_id,
            Some(NetworkId::new(
                "88cad55e9ed7797a340f76b9a6bd4963c2dea4dc680ab37984f298ae2dfc6c3c"
            ))
        );
    }

    #[test]
    fn empty_network_id_is_none() {
        let container_network =
            parse(r#"{"NetworkID":"","IPAddress":"","GlobalIPv6Address":""}"#).unwrap();

        assert_eq!(container_network.network_id, None);
    }

    #[test]
    fn absent_network_id_is_none() {
        let container_network = parse(r#"{"IPAddress":"","GlobalIPv6Address":""}"#).unwrap();

        assert_eq!(container_network.network_id, None);
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

    #[test]
    fn absent_dns_names_are_empty() {
        let container_network = parse(r#"{"IPAddress":"","GlobalIPv6Address":""}"#).unwrap();

        assert!(container_network.dns_names.is_empty());
    }

    #[test]
    fn null_dns_names_are_empty() {
        let container_network =
            parse(r#"{"IPAddress":"","GlobalIPv6Address":"","DNSNames":null}"#).unwrap();

        assert!(container_network.dns_names.is_empty());
    }

    #[test]
    fn dns_names_are_kept_in_order() {
        let container_network = parse(
            r#"{"IPAddress":"","GlobalIPv6Address":"","DNSNames":["ubuntu","1a2b3c4d5e6f"]}"#,
        )
        .unwrap();

        assert_eq!(
            container_network
                .dns_names
                .iter()
                .map(|dns_name| &**dns_name)
                .collect::<Vec<_>>(),
            ["ubuntu", "1a2b3c4d5e6f"]
        );
    }
}
