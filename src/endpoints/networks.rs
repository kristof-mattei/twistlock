use hyper::Method;

use crate::endpoint::ApiEndpoint;
use crate::models::network::{NetworkInspect, NetworkSummary};

pub struct ListNetworks;

impl ApiEndpoint for ListNetworks {
    type Request = ();
    type Response = Vec<NetworkSummary>;
    type Error = serde_json::Value;

    const METHOD: Method = Method::GET;

    fn path_and_query(_request: &Self::Request) -> Result<String, std::io::Error> {
        Ok("/networks".to_owned())
    }
}

pub struct InspectNetwork;

impl ApiEndpoint for InspectNetwork {
    type Request = str;
    type Response = NetworkInspect;
    type Error = serde_json::Value;

    const METHOD: Method = Method::GET;

    fn path_and_query(request: &Self::Request) -> Result<String, std::io::Error> {
        Ok(format!("/networks/{}", request))
    }
}
