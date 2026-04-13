use std::time::Duration;

use hyper::Method;

use crate::docker::client::url_encode;
use crate::docker::endpoint::Endpoint;
use crate::filters::Filters;
use crate::models::container::Container;
use crate::models::container_inspect::ContainerInspect;

pub struct ListContainers;

impl Endpoint for ListContainers {
    type Request = Filters;
    type Response = Vec<Container>;
    type Error = serde_json::Value;

    const METHOD: Method = Method::GET;

    fn path_and_query(request: &Self::Request) -> Result<String, std::io::Error> {
        Ok(format!("/containers/json?filters={}", url_encode(request)?))
    }
}

pub struct InspectContainer;

impl Endpoint for InspectContainer {
    type Request = str;
    type Response = ContainerInspect;
    type Error = serde_json::Value;

    const METHOD: Method = Method::GET;

    fn path_and_query(request: &Self::Request) -> Result<String, std::io::Error> {
        Ok(format!("/containers/{}/json", request))
    }
}

pub struct RestartContainerRequest {
    pub id: String,
    pub timeout: Duration,
}

pub struct RestartContainer;

impl Endpoint for RestartContainer {
    type Request = RestartContainerRequest;
    type Response = ();
    type Error = serde_json::Value;

    const METHOD: Method = Method::POST;

    fn path_and_query(request: &Self::Request) -> Result<String, std::io::Error> {
        Ok(format!(
            "/containers/{}/restart?t={}",
            request.id,
            request.timeout.as_secs()
        ))
    }

    fn parse_response(_bytes: &[u8]) -> Result<Self::Response, serde_json::Error> {
        Ok(())
    }
}
