#![expect(clippy::tests_outside_test_module, reason = "Integration tests")]
use std::process::Command;
use std::str::FromStr as _;
use std::time::Duration;

use hashbrown::HashMap;
#[cfg(windows)]
use hashbrown::HashSet;
use pretty_assertions::assert_eq;
use twistlock::client::Client;
use twistlock::config::Endpoint;
use twistlock::endpoint::ApiEndpointCallError;
#[cfg(windows)]
use twistlock::filters::Isolation;
use twistlock::filters::{Filters, Status};
use twistlock::models::container::{StopOutcome, StopTimeout};
use twistlock::models::events::Event;
use twistlock::models::id::{ContainerId, ContainerRef, NetworkRef};

const LABEL: &str = "twistlock.test";

fn docker(args: &[&str]) -> String {
    let output = Command::new("docker").args(args).output().unwrap();

    assert!(
        output.status.success(),
        "docker {}: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );

    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

enum DaemonOs {
    Linux,
    Windows,
}

impl DaemonOs {
    fn detect() -> DaemonOs {
        match &*docker(&["version", "--format", "{{.Server.Os}}"]) {
            "linux" => DaemonOs::Linux,
            "windows" => DaemonOs::Windows,
            other => panic!("unknown daemon OS: {}", other),
        }
    }

    fn idle_container(&self) -> &'static [&'static str] {
        match *self {
            DaemonOs::Linux => &["ubuntu", "sleep", "infinity"],
            DaemonOs::Windows => &[
                "--isolation",
                "process",
                "mcr.microsoft.com/windows/nanoserver:ltsc2025",
                "ping",
                "-t",
                "localhost",
            ],
        }
    }

    fn default_network(&self) -> &'static str {
        match *self {
            DaemonOs::Linux => "bridge",
            DaemonOs::Windows => "nat",
        }
    }
}

struct Fixture {
    id: ContainerId,
    name: String,
}

impl Fixture {
    fn run(test: &str) -> Fixture {
        let name = format!("twistlock-{}-{}", test, std::process::id());
        let label = format!("{}={}", LABEL, name);

        let mut args = vec!["run", "--detach", "--name", &name, "--label", &label];
        args.extend_from_slice(DaemonOs::detect().idle_container());

        let id = docker(&args);

        Fixture {
            id: ContainerId::new(id),
            name,
        }
    }

    fn reference(&self) -> ContainerRef<'_> {
        ContainerRef::try_from(self.name.as_str()).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // if the removal fails, there is nothing to do
        let _output = Command::new("docker")
            .args(["rm", "--force", &self.name])
            .output();
    }
}

/// Builds a client for the current `docker` context, whose daemon runs the fixtures.
fn client() -> Client {
    let host = docker(&[
        "context",
        "inspect",
        "--format",
        "{{.Endpoints.docker.Host}}",
    ]);

    Client::build(Endpoint::from_str(&host).unwrap()).unwrap()
}

#[tokio::test]
#[ignore = "needs a Docker daemon"]
async fn list_containers_returns_the_fixture() {
    let fixture = Fixture::run("list");

    let filters = Filters {
        label: Some(HashMap::from([(
            LABEL.into(),
            Some(fixture.name.as_str().into()),
        )])),
        ..Filters::default()
    };

    let containers = client().list_containers(&filters).send().await.unwrap();

    assert_eq!(containers.len(), 1);
    assert_eq!(containers[0].id, fixture.id);
    assert_eq!(&*containers[0].names, [fixture.name.as_str().into()]);
    assert_eq!(containers[0].state, Status::Running);
    assert_eq!(
        containers[0].labels.get(LABEL).map(|value| &**value),
        Some(fixture.name.as_str())
    );
}

#[tokio::test]
#[ignore = "needs a Docker daemon"]
async fn inspect_container_returns_the_running_fixture() {
    let fixture = Fixture::run("inspect");

    let container = client()
        .inspect_container(fixture.reference())
        .send()
        .await
        .unwrap();

    assert_eq!(container.id, fixture.id);
    assert_eq!(&*container.name, format!("/{}", fixture.name));
    assert!(container.state.running);
    assert_eq!(container.state.status, Status::Running);
    assert!(container.state.health.is_none());
    assert_eq!(
        container.config.labels.get(LABEL).map(|value| &**value),
        Some(fixture.name.as_str())
    );
}

#[tokio::test]
#[ignore = "needs a Docker daemon"]
async fn inspect_of_a_missing_container_is_the_daemon_error() {
    let name = format!("twistlock-missing-{}", std::process::id());

    let Err(ApiEndpointCallError::Daemon { status, .. }) = client()
        .inspect_container(ContainerRef::try_from(name.as_str()).unwrap())
        .send()
        .await
    else {
        panic!("not the daemon's error");
    };

    assert_eq!(status, 404);
}

#[tokio::test]
#[ignore = "needs a Docker daemon"]
async fn second_stop_is_already_stopped() {
    let fixture = Fixture::run("stop");
    let client = client();

    let first = client
        .stop_container(fixture.reference(), StopTimeout::After(Duration::ZERO))
        .await
        .unwrap();

    let second = client
        .stop_container(fixture.reference(), StopTimeout::After(Duration::ZERO))
        .await
        .unwrap();

    assert!(matches!(first, StopOutcome::Stopped));
    assert!(matches!(second, StopOutcome::AlreadyStopped));

    let container = client
        .inspect_container(fixture.reference())
        .send()
        .await
        .unwrap();

    assert_eq!(container.state.status, Status::Exited);
}

#[tokio::test]
#[ignore = "needs a Docker daemon"]
async fn restart_is_observed_by_the_subscription() {
    let fixture = Fixture::run("restart");
    let client = client();

    let mut subscription = client.subscribe_events().send().await.unwrap();

    client
        .restart_container(fixture.reference(), StopTimeout::After(Duration::ZERO))
        .await
        .unwrap();

    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            match subscription.next().await.unwrap() {
                Ok(Event::Container(event))
                    if event.actor.id == fixture.id && &*event.action == "restart" =>
                {
                    break;
                },
                Ok(_) => {},
                Err(error) => panic!(
                    "undecodable event: {}",
                    String::from_utf8_lossy(&error.line)
                ),
            }
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
#[ignore = "needs a Docker daemon"]
async fn inspect_network_returns_the_default_network() {
    let name = DaemonOs::detect().default_network();

    let network = client()
        .inspect_network(NetworkRef::try_from(name).unwrap())
        .send()
        .await
        .unwrap();

    assert_eq!(&*network.name, name);
    assert!(
        network
            .ipam
            .config
            .iter()
            .any(|config| config.subnet.is_some())
    );
}

#[tokio::test]
#[ignore = "needs a Docker daemon"]
async fn list_networks_includes_the_default_network() {
    let client = client();

    let network = client
        .inspect_network(NetworkRef::try_from(DaemonOs::detect().default_network()).unwrap())
        .send()
        .await
        .unwrap();

    let networks = client.list_networks().send().await.unwrap();

    assert!(networks.iter().any(|summary| summary.id == network.id));
}

#[cfg(windows)]
#[tokio::test]
#[ignore = "needs a Docker daemon"]
async fn isolation_filter_matches_the_fixture() {
    let fixture = Fixture::run("isolation");

    let filters = Filters {
        isolation: Some(HashSet::from([Isolation::Process])),
        label: Some(HashMap::from([(
            LABEL.into(),
            Some(fixture.name.as_str().into()),
        )])),
        ..Filters::default()
    };

    let containers = client().list_containers(&filters).send().await.unwrap();

    assert_eq!(containers.len(), 1);
    assert_eq!(containers[0].id, fixture.id);
}
