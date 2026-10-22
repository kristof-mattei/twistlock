use hyper::{Method, StatusCode};

use crate::endpoint::ApiEndpoint;
use crate::models::id::NetworkRef;
use crate::models::network::{NetworkInspect, NetworkSummary};

pub(crate) struct ListNetworks;

impl ApiEndpoint for ListNetworks {
    type Request<'r> = ();
    type Response = Vec<NetworkSummary>;

    const METHOD: Method = Method::GET;

    fn path_and_query(_request: &Self::Request<'_>) -> Result<String, std::io::Error> {
        Ok("/networks".to_owned())
    }

    fn parse_response(
        _status: StatusCode,
        bytes: &[u8],
    ) -> Result<Self::Response, serde_json::Error> {
        serde_json::from_slice(bytes)
    }
}

pub(crate) struct InspectNetwork;

impl ApiEndpoint for InspectNetwork {
    type Request<'r> = NetworkRef<'r>;
    type Response = NetworkInspect;

    const METHOD: Method = Method::GET;

    fn path_and_query(request: &Self::Request<'_>) -> Result<String, std::io::Error> {
        Ok(format!("/networks/{}", request.as_str()))
    }

    fn parse_response(
        _status: StatusCode,
        bytes: &[u8],
    ) -> Result<Self::Response, serde_json::Error> {
        serde_json::from_slice(bytes)
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use crate::endpoint::ApiEndpoint as _;
    use crate::endpoints::networks::InspectNetwork;
    use crate::models::id::{NetworkId, NetworkRef};

    #[test]
    fn inspect_network_path_from_id() {
        let id = NetworkId::new("88cad55e9ed7");

        assert_eq!(
            InspectNetwork::path_and_query(&NetworkRef::Id(&id)).unwrap(),
            "/networks/88cad55e9ed7"
        );
    }
}
