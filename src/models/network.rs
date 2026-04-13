use ipnet::IpNet;
use serde::{Deserialize, Deserializer};

fn null_as_empty_vec<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Ok(Option::<Vec<T>>::deserialize(deserializer)?.unwrap_or_default())
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct NetworkInspect {
    pub id: Box<str>,
    pub name: Box<str>,
    #[serde(rename = "IPAM")]
    pub ipam: NetworkIpam,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct NetworkIpam {
    #[serde(default, deserialize_with = "null_as_empty_vec")]
    pub config: Vec<NetworkIpamConfig>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct NetworkIpamConfig {
    pub subnet: Option<IpNet>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct NetworkSummary {
    pub id: Box<str>,
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::{NetworkInspect, NetworkIpamConfig};

    fn parse_config(json: &str) -> Result<NetworkIpamConfig, serde_json::Error> {
        serde_json::from_str(json)
    }

    fn parse_inspect(json: &str) -> Result<NetworkInspect, serde_json::Error> {
        serde_json::from_str(json)
    }

    #[test]
    fn ipam_config_ipv4_subnet() {
        let config = parse_config(r#"{"Subnet":"192.168.1.0/24"}"#).unwrap();

        assert_eq!(config.subnet, Some("192.168.1.0/24".parse().unwrap()));
    }

    #[test]
    fn ipam_config_ipv6_subnet() {
        let config = parse_config(r#"{"Subnet":"2001:db8::/32"}"#).unwrap();

        assert_eq!(config.subnet, Some("2001:db8::/32".parse().unwrap()));
    }

    #[test]
    fn ipam_config_subnet_null() {
        let config = parse_config(r#"{"Subnet":null}"#).unwrap();

        assert_eq!(config.subnet, None);
    }

    #[test]
    fn ipam_config_subnet_absent() {
        let config = parse_config("{}").unwrap();

        assert_eq!(config.subnet, None);
    }

    #[test]
    fn ipam_config_invalid_subnet_is_error() {
        parse_config(r#"{"Subnet":"not-a-cidr"}"#).unwrap_err();
    }

    #[test]
    fn network_inspect_ipam_rename() {
        // Verifies the IPAM field (all-caps) deserializes correctly and
        // that multi-config entries are all captured.
        let inspect = parse_inspect(
            r#"{
                "Id": "abc123",
                "Name": "my-network",
                "IPAM": {
                    "Config": [
                        {"Subnet": "10.0.0.0/8"},
                        {"Subnet": "fd00::/8"}
                    ]
                }
            }"#,
        )
        .unwrap();

        assert_eq!(inspect.id.as_ref(), "abc123");
        assert_eq!(inspect.name.as_ref(), "my-network");
        assert_eq!(inspect.ipam.config.len(), 2);
        assert_eq!(
            inspect.ipam.config[0].subnet,
            Some("10.0.0.0/8".parse().unwrap())
        );
        assert_eq!(
            inspect.ipam.config[1].subnet,
            Some("fd00::/8".parse().unwrap())
        );
    }

    #[test]
    fn network_inspect_empty_ipam_config() {
        let inspect = parse_inspect(
            r#"{
                "Id": "def456",
                "Name": "empty-net",
                "IPAM": {"Config": []}
            }"#,
        )
        .unwrap();

        assert_eq!(inspect.ipam.config.len(), 0);
    }

    #[test]
    fn network_inspect_null_ipam_config() {
        // Docker returns null Config for networks like "none"
        let inspect = parse_inspect(
            r#"{
                "Id": "789b90d0",
                "Name": "none",
                "IPAM": {"Driver": "default", "Options": null, "Config": null}
            }"#,
        )
        .unwrap();

        assert_eq!(inspect.ipam.config.len(), 0);
    }

    #[test]
    fn network_inspect_none_network_full_response() {
        // Full Docker response for the built-in "none" network, which has null IPAM config
        let inspect = parse_inspect(r#"{"Name":"none","Id":"789b90d02ff7f8705ae644eb3d3aa0a9ca5b3b1acb5cf2a8b2f4343072359026","Created":"2025-11-28T00:26:17.88395535-07:00","Scope":"local","Driver":"null","EnableIPv4":true,"EnableIPv6":false,"IPAM":{"Driver":"default","Options":null,"Config":null},"Internal":false,"Attachable":false,"Ingress":false,"ConfigFrom":{"Network":""},"ConfigOnly":false,"Options":{},"Labels":{},"Containers":{},"Status":{"IPAM":{}}}"#).unwrap();

        assert_eq!(inspect.name.as_ref(), "none");
        assert_eq!(
            inspect.id.as_ref(),
            "789b90d02ff7f8705ae644eb3d3aa0a9ca5b3b1acb5cf2a8b2f4343072359026"
        );
        assert_eq!(inspect.ipam.config.len(), 0);
    }
}
