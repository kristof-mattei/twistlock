use std::time::Duration;

use hyper::Method;

use crate::client::url_encode;
use crate::endpoint::ApiEndpoint;
use crate::filters::Filters;
use crate::models::container::ContainerSummary;
use crate::models::container_inspect::ContainerInspect;
use crate::models::id::ContainerRef;

pub struct ListContainers;

impl ApiEndpoint for ListContainers {
    type Request<'r> = Filters;
    type Response = Vec<ContainerSummary>;
    type Error = serde_json::Value;

    const METHOD: Method = Method::GET;

    fn path_and_query(request: &Self::Request<'_>) -> Result<String, std::io::Error> {
        Ok(format!("/containers/json?filters={}", url_encode(request)?))
    }
}

pub struct InspectContainer;

impl ApiEndpoint for InspectContainer {
    type Request<'r> = ContainerRef<'r>;
    type Response = ContainerInspect;
    type Error = serde_json::Value;

    const METHOD: Method = Method::GET;

    fn path_and_query(request: &Self::Request<'_>) -> Result<String, std::io::Error> {
        Ok(format!("/containers/{}/json", request.as_str()))
    }
}

pub struct RestartContainerRequest<'r> {
    pub container: ContainerRef<'r>,
    pub timeout: Duration,
}

pub struct RestartContainer;

impl ApiEndpoint for RestartContainer {
    type Request<'r> = RestartContainerRequest<'r>;
    type Response = ();
    type Error = serde_json::Value;

    const METHOD: Method = Method::POST;

    fn path_and_query(request: &Self::Request<'_>) -> Result<String, std::io::Error> {
        Ok(format!(
            "/containers/{}/restart?t={}",
            request.container.as_str(),
            request.timeout.as_secs()
        ))
    }

    fn parse_response(_bytes: &[u8]) -> Result<Self::Response, serde_json::Error> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use hashbrown::{HashMap, HashSet};
    use pretty_assertions::assert_eq;

    use crate::client::url_encode;
    use crate::endpoint::ApiEndpoint as _;
    use crate::endpoints::containers::{
        InspectContainer, RestartContainer, RestartContainerRequest,
    };
    use crate::filters::{Filters, Health};
    use crate::models::id::{ContainerId, ContainerRef};

    #[test]
    fn inspect_container_path_from_id() {
        let id = ContainerId::new("0f9fc026ac74");

        assert_eq!(
            InspectContainer::path_and_query(&ContainerRef::Id(&id)).unwrap(),
            "/containers/0f9fc026ac74/json"
        );
    }

    #[test]
    fn inspect_container_path_from_name() {
        assert_eq!(
            InspectContainer::path_and_query(&ContainerRef::IdOrName("photoprism")).unwrap(),
            "/containers/photoprism/json"
        );
    }

    #[test]
    fn restart_container_path_carries_the_timeout() {
        let request = RestartContainerRequest {
            container: ContainerRef::IdOrName("photoprism"),
            timeout: Duration::from_secs(12),
        };

        assert_eq!(
            RestartContainer::path_and_query(&request).unwrap(),
            "/containers/photoprism/restart?t=12"
        );
    }

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
