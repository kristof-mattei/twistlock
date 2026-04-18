use std::time::Duration;

use hyper::Method;

use crate::client::url_encode;
use crate::endpoint::ApiEndpoint;
use crate::filters::Filters;
use crate::models::container::Container;
use crate::models::container_inspect::ContainerInspect;

pub struct ListContainers;

impl ApiEndpoint for ListContainers {
    type Request = Filters;
    type Response = Vec<Container>;
    type Error = serde_json::Value;

    const METHOD: Method = Method::GET;

    fn path_and_query(request: &Self::Request) -> Result<String, std::io::Error> {
        Ok(format!("/containers/json?filters={}", url_encode(request)?))
    }
}

pub struct InspectContainer;

impl ApiEndpoint for InspectContainer {
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

impl ApiEndpoint for RestartContainer {
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

#[cfg(test)]
mod tests {
    use hashbrown::{HashMap, HashSet};
    use pretty_assertions::assert_eq;

    use crate::client::url_encode;
    use crate::filters::{Filters, Health};

    fn build(mode: &str) -> Filters {
        Filters {
            health: Some(HashSet::from_iter([Health::Unhealthy])),
            label: Some(HashMap::from_iter([(
                Box::from(mode),
                Some(Box::from("true")),
            )])),
            ..Filters::default()
        }
    }

    #[test]
    fn build_decode_autoheal() {
        let something_and_unhealthy = build("something");

        let something_and_unhealthy_encoded = url_encode(&something_and_unhealthy).unwrap();

        assert_eq!(
            &*something_and_unhealthy_encoded,
            "%7B%22label%22%3A%5B%22something%3Dtrue%22%5D%2C%22health%22%3A%5B%22unhealthy%22%5D%7D"
        );
    }

    #[test]
    fn build_decode_custom() {
        let custom_and_unhealthy = build("custom");

        let custom_and_unhealthy_encoded = url_encode(&custom_and_unhealthy).unwrap();

        assert_eq!(
            &*custom_and_unhealthy_encoded,
            "%7B%22label%22%3A%5B%22custom%3Dtrue%22%5D%2C%22health%22%3A%5B%22unhealthy%22%5D%7D"
        );
    }
}
