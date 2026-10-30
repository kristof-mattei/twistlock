#![expect(clippy::tests_outside_test_module, reason = "Integration tests")]
use std::str::FromStr as _;

use mockito::{Mock, Server, ServerGuard};
use pretty_assertions::assert_eq;
use twistlock::client::Client;
use twistlock::config::Endpoint;
use twistlock::endpoint::ApiEndpointCallError;
use twistlock::filters::{Filters, Status};
use twistlock::models::id::{ContainerRef, NetworkId, NetworkRef};

/// Serves `200` with `body` for one `GET` of `path_and_query`.
async fn daemon(path_and_query: &str, body: &str) -> (Client, ServerGuard, Mock) {
    let mut server = Server::new_async().await;

    let mock = server
        .mock("GET", path_and_query)
        .with_status(200)
        .with_body(body)
        .create_async()
        .await;

    let endpoint = Endpoint::from_str(&format!("tcp://{}", server.host_with_port())).unwrap();

    (Client::build(endpoint).unwrap(), server, mock)
}

#[tokio::test]
async fn list_containers_decodes_the_summaries() {
    let (client, _server, mock) = daemon(
        "/containers/json?all=false&filters=%7B%7D",
        r#"[{"Id":"582036c7a5e8","Names":["/ubuntu"],"Labels":{},"State":"running","NetworkSettings":{"Networks":{}}}]"#,
    )
    .await;

    let containers = client.list_containers(&Filters::default()).await.unwrap();

    mock.assert_async().await;

    assert_eq!(&*containers[0].names[0], "ubuntu");
    assert_eq!(containers[0].state, Status::Running);
}

#[tokio::test]
async fn inspect_container_decodes_the_inspect() {
    let (client, _server, mock) = daemon(
        "/containers/ubuntu/json",
        r#"{"Name":"/ubuntu","Id":"582036c7a5e8","Config":{"Hostname":"582036c7a5e8","Labels":{}},"State":{"Running":true,"Status":"running"},"NetworkSettings":{"Networks":{}}}"#,
    )
    .await;

    let container = client
        .inspect_container(ContainerRef::try_from("ubuntu").unwrap())
        .await
        .unwrap();

    mock.assert_async().await;

    assert_eq!(&*container.name, "/ubuntu");
    assert_eq!(container.state.status, Status::Running);
}

#[tokio::test]
async fn list_networks_decodes_the_summaries() {
    let (client, _server, mock) = daemon("/networks", r#"[{"Id":"88cad55e9ed7"}]"#).await;

    let networks = client.list_networks().await.unwrap();

    mock.assert_async().await;

    assert_eq!(networks[0].id, NetworkId::new("88cad55e9ed7"));
}

#[tokio::test]
async fn inspect_network_decodes_the_inspect() {
    let (client, _server, mock) = daemon(
        "/networks/bridge",
        r#"{"Id":"88cad55e9ed7","Name":"bridge","IPAM":{"Config":[{"Subnet":"172.17.0.0/16"}]}}"#,
    )
    .await;

    let network = client
        .inspect_network(NetworkRef::try_from("bridge").unwrap())
        .await
        .unwrap();

    mock.assert_async().await;

    assert_eq!(&*network.name, "bridge");
}

#[tokio::test]
async fn undecodable_body_is_a_deserialize_error() {
    let (client, _server, mock) = daemon("/networks", "{").await;

    let result = client.list_networks().await;

    mock.assert_async().await;

    assert!(matches!(result, Err(ApiEndpointCallError::Deserialize(_))));
}
