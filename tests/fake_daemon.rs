#![expect(clippy::tests_outside_test_module, reason = "Integration tests")]
use std::io::{Read, Write};
use std::net::TcpListener;
#[cfg(unix)]
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::str::FromStr as _;
use std::sync::Arc;
use std::time::Duration;

use pretty_assertions::assert_eq;
use rcgen::{BasicConstraints, CertificateParams, CertifiedIssuer, IsCa, KeyPair};
use rustls::crypto::aws_lc_rs;
use rustls::server::WebPkiClientVerifier;
use rustls::{
    ConfigBuilder, RootCertStore, ServerConfig, ServerConnection, StreamOwned, WantsVerifier,
};
use tempfile::TempDir;
#[cfg(windows)]
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
#[cfg(windows)]
use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeServer, ServerOptions};
use twistlock::client::{Client, EventStreamError};
use twistlock::config::{ClientCredentialPaths, Endpoint, Tls};
use twistlock::endpoint::{ApiEndpointCallError, TransportError};
use twistlock::models::container::{StopOutcome, StopTimeout};
use twistlock::models::events::{Event, EventDecodeError};
use twistlock::models::id::ContainerRef;

fn line(action: &str) -> String {
    let mut line = format!(
        r#"{{"Type":"container","Action":"{}","Actor":{{"ID":"0f9fc026ac74","Attributes":{{}}}},"scope":"local","time":1,"timeNano":2}}"#,
        action
    );

    line.push('\n');

    line
}

fn container_action(next: Option<Result<Event, EventDecodeError>>) -> Box<str> {
    let Some(Ok(Event::Container(body))) = next else {
        panic!("not a container event");
    };

    body.action
}

/// A dropped sender closes the connection.
fn spawn_daemon<A, S>(accept: A, head: &'static str) -> std::sync::mpsc::Sender<String>
where
    A: FnOnce() -> S + Send + 'static,
    S: Read + Write,
{
    let (body_sender, body_receiver) = std::sync::mpsc::channel::<String>();

    std::thread::spawn(move || {
        let mut stream = accept();

        let mut request = Vec::new();
        let mut byte = [0_u8; 1];

        while !request.ends_with(b"\r\n\r\n") {
            stream.read_exact(&mut byte).unwrap();
            request.push(byte[0]);
        }

        stream.write_all(head.as_bytes()).unwrap();

        for part in body_receiver {
            stream.write_all(part.as_bytes()).unwrap();
        }
    });

    body_sender
}

fn serve(head: &'static str) -> (Client, std::sync::mpsc::Sender<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let body = spawn_daemon(move || listener.accept().unwrap().0, head);

    let endpoint = Endpoint::from_str(&format!("tcp://{}", address)).unwrap();
    let client = Client::build(endpoint).unwrap();

    (client, body)
}

fn write(directory: &Path, name: &str, contents: &str) -> PathBuf {
    let path = directory.join(name);

    std::fs::write(&path, contents).unwrap();

    path
}

fn certificate_authority() -> CertifiedIssuer<'static, KeyPair> {
    let mut params = CertificateParams::default();
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);

    CertifiedIssuer::self_signed(params, KeyPair::generate().unwrap()).unwrap()
}

fn daemon_config() -> ConfigBuilder<ServerConfig, WantsVerifier> {
    ServerConfig::builder_with_provider(Arc::new(aws_lc_rs::default_provider()))
        .with_safe_default_protocol_versions()
        .unwrap()
}

/// Returns the daemon's `DOCKER_HOST`.
fn serve_tls(
    config: ServerConfig,
    head: &'static str,
) -> (String, std::sync::mpsc::Sender<String>) {
    let config = Arc::new(config);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let body = spawn_daemon(
        move || {
            StreamOwned::new(
                ServerConnection::new(config).unwrap(),
                listener.accept().unwrap().0,
            )
        },
        head,
    );

    (format!("tcp://{}", address), body)
}

fn tls_client(host: &str, tls: Tls) -> Client {
    Client::build(Endpoint::from_str(host).unwrap().with_tls(tls)).unwrap()
}

#[tokio::test]
async fn response_head_returns_the_subscription() {
    let (client, body) = serve("HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\n\r\n");

    let mut subscription = client.subscribe_events().send().await.unwrap();

    let start = line("start");

    body.send(format!("{:x}\r\n{}\r\n0\r\n\r\n", start.len(), start))
        .unwrap();
    drop(body);

    let Ok(Ok(Event::Container(event))) = subscription.next().await else {
        panic!("not a container event");
    };

    assert_eq!(&*event.action, "start");

    assert!(matches!(
        subscription.next().await,
        Err(EventStreamError::Ended)
    ));
}

#[tokio::test]
async fn broken_chunk_is_a_frame_error() {
    let (client, body) = serve("HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\n\r\n");

    let mut subscription = client.subscribe_events().send().await.unwrap();

    // the chunk size is 5, and the connection closes after 2 bytes
    body.send("5\r\nab".to_owned()).unwrap();
    drop(body);

    let Err(error) = subscription.next().await else {
        panic!("not an error");
    };

    assert!(matches!(error, EventStreamError::Frame(_)));
    assert_eq!(error.to_string(), "Failed to read frame");
}

#[tokio::test]
async fn truncated_body_is_a_body_error() {
    let (client, body) = serve("HTTP/1.1 200 OK\r\ncontent-length: 10\r\n\r\n");

    // `content-length` is 10, and the connection closes after 2 bytes
    body.send("[]".to_owned()).unwrap();
    drop(body);

    assert!(matches!(
        client.list_networks().send().await,
        Err(ApiEndpointCallError::Transport(TransportError::Body(_)))
    ));
}

/// Fails the test instead of hanging when the time limit under test does not apply.
async fn bounded<F: Future>(future: F) -> F::Output {
    tokio::time::timeout(Duration::from_secs(5), future)
        .await
        .unwrap()
}

#[tokio::test]
async fn list_without_a_response_is_a_timeout() {
    let (client, _body) = serve("");

    let result = bounded(
        client
            .list_networks()
            .timeout(Duration::from_millis(100))
            .send(),
    )
    .await;

    assert!(matches!(
        result,
        Err(ApiEndpointCallError::Transport(TransportError::Timeout(limit)))
            if limit == Duration::from_millis(100)
    ));
}

#[tokio::test]
async fn list_with_a_stalled_body_is_a_timeout() {
    let (client, _body) = serve("HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\n");

    let result = bounded(
        client
            .list_networks()
            .timeout(Duration::from_millis(100))
            .send(),
    )
    .await;

    assert!(matches!(
        result,
        Err(ApiEndpointCallError::Transport(TransportError::Timeout(limit)))
            if limit == Duration::from_millis(100)
    ));
}

#[tokio::test]
async fn subscription_without_a_head_is_a_timeout() {
    let (client, _body) = serve("");

    let result = bounded(
        client
            .subscribe_events()
            .timeout(Duration::from_millis(100))
            .send(),
    )
    .await;

    assert!(matches!(
        result,
        Err(ApiEndpointCallError::Transport(TransportError::Timeout(limit)))
            if limit == Duration::from_millis(100)
    ));
}

#[tokio::test]
async fn subscription_outlives_the_timeout() {
    let (client, body) = serve("HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\n\r\n");

    let mut subscription = client
        .subscribe_events()
        .timeout(Duration::from_millis(100))
        .send()
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(200)).await;

    let start = line("start");

    body.send(format!("{:x}\r\n{}\r\n", start.len(), start))
        .unwrap();

    assert_eq!(
        &*container_action(Some(subscription.next().await.unwrap())),
        "start"
    );
}

#[tokio::test]
async fn stop_with_no_content_is_stopped() {
    let (client, _body) = serve("HTTP/1.1 204 No Content\r\n\r\n");

    let outcome = client
        .stop_container(
            ContainerRef::try_from("ubuntu").unwrap(),
            StopTimeout::Default,
        )
        .await
        .unwrap();

    assert!(matches!(outcome, StopOutcome::Stopped));
}

#[cfg(unix)]
#[tokio::test]
async fn stop_over_a_socket_is_stopped() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("docker.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let _body = spawn_daemon(
        move || listener.accept().unwrap().0,
        "HTTP/1.1 204 No Content\r\n\r\n",
    );

    let endpoint = Endpoint::from_str(&format!("unix://{}", path.display())).unwrap();
    let client = Client::build(endpoint).unwrap();

    let outcome = client
        .stop_container(
            ContainerRef::try_from("ubuntu").unwrap(),
            StopTimeout::Default,
        )
        .await
        .unwrap();

    assert!(matches!(outcome, StopOutcome::Stopped));
}

#[cfg(windows)]
async fn serve_pipe(mut server: NamedPipeServer) {
    server.connect().await.unwrap();

    let mut request = Vec::new();
    let mut byte = [0_u8; 1];

    while !request.ends_with(b"\r\n\r\n") {
        server.read_exact(&mut byte).await.unwrap();
        request.push(byte[0]);
    }

    server
        .write_all(b"HTTP/1.1 204 No Content\r\n\r\n")
        .await
        .unwrap();

    // keeps the pipe open until the client closes it
    let _closed = server.read(&mut byte).await;
}

#[cfg(windows)]
#[tokio::test]
async fn stop_over_a_named_pipe_is_stopped() {
    let name = format!("twistlock-{}", std::process::id());
    let server = ServerOptions::new()
        .first_pipe_instance(true)
        .create(format!(r"\\.\pipe\{}", name))
        .unwrap();

    tokio::spawn(serve_pipe(server));

    let endpoint = Endpoint::from_str(&format!("npipe:////./pipe/{}", name)).unwrap();
    let client = Client::build(endpoint).unwrap();

    let outcome = client
        .stop_container(
            ContainerRef::try_from("ubuntu").unwrap(),
            StopTimeout::Default,
        )
        .await
        .unwrap();

    assert!(matches!(outcome, StopOutcome::Stopped));
}

#[cfg(windows)]
#[tokio::test]
async fn stop_over_a_busy_named_pipe_is_stopped() {
    let name = format!("twistlock-busy-{}", std::process::id());
    let path = format!(r"\\.\pipe\{}", name);

    // the only instance, connected to another client
    let _server = ServerOptions::new()
        .first_pipe_instance(true)
        .create(&path)
        .unwrap();
    let _client = ClientOptions::new().open(&path).unwrap();

    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;

        serve_pipe(ServerOptions::new().create(&path).unwrap()).await;
    });

    let endpoint = Endpoint::from_str(&format!("npipe:////./pipe/{}", name)).unwrap();
    let client = Client::build(endpoint).unwrap();

    let outcome = client
        .stop_container(
            ContainerRef::try_from("ubuntu").unwrap(),
            StopTimeout::Default,
        )
        .await
        .unwrap();

    assert!(matches!(outcome, StopOutcome::Stopped));
}

#[tokio::test]
async fn stop_with_not_modified_is_already_stopped() {
    let (client, _body) = serve("HTTP/1.1 304 Not Modified\r\n\r\n");

    let outcome = client
        .stop_container(
            ContainerRef::try_from("ubuntu").unwrap(),
            StopTimeout::Default,
        )
        .await
        .unwrap();

    assert!(matches!(outcome, StopOutcome::AlreadyStopped));
}

#[tokio::test]
async fn restart_with_not_modified_is_an_error() {
    let (client, _body) = serve("HTTP/1.1 304 Not Modified\r\n\r\n");

    let Err(ApiEndpointCallError::HttpError { status, .. }) = client
        .restart_container(
            ContainerRef::try_from("ubuntu").unwrap(),
            StopTimeout::Default,
        )
        .await
    else {
        panic!("not an HTTP error");
    };

    assert_eq!(status, 304);
}

#[tokio::test]
async fn stop_with_not_found_is_the_daemon_error() {
    let (client, _body) = serve(
        "HTTP/1.1 404 Not Found\r\ncontent-length: 39\r\n\r\n{\"message\":\"No such container: ubuntu\"}",
    );

    let Err(ApiEndpointCallError::Daemon { status, message }) = client
        .stop_container(
            ContainerRef::try_from("ubuntu").unwrap(),
            StopTimeout::Default,
        )
        .await
    else {
        panic!("not the daemon's error");
    };

    assert_eq!(status, 404);
    assert_eq!(message, "No such container: ubuntu");
}

#[tokio::test]
async fn unsuccessful_subscription_returns_the_daemon_error() {
    let (client, _body) = serve(
        "HTTP/1.1 500 Internal Server Error\r\ncontent-length: 18\r\n\r\n{\"message\":\"boom\"}",
    );

    let Err(ApiEndpointCallError::Daemon { status, message }) =
        client.subscribe_events().send().await
    else {
        panic!("not the daemon's error");
    };

    assert_eq!(status, 500);
    assert_eq!(message, "boom");
}

#[tokio::test]
async fn unsuccessful_subscription_without_an_error_response_returns_the_body() {
    let (client, _body) = serve(
        "HTTP/1.1 500 Internal Server Error\r\ncontent-length: 16\r\n\r\n{\"error\":\"boom\"}",
    );

    let Err(ApiEndpointCallError::HttpError { status, body }) =
        client.subscribe_events().send().await
    else {
        panic!("not an HTTP error");
    };

    assert_eq!(status, 500);
    assert_eq!(body, r#"{"error":"boom"}"#);
}

#[tokio::test]
async fn stop_over_mutual_tls_is_stopped() {
    let ca = certificate_authority();

    let daemon_key = KeyPair::generate().unwrap();
    let daemon_cert = CertificateParams::new(["127.0.0.1".to_owned()])
        .unwrap()
        .signed_by(&daemon_key, &ca)
        .unwrap();

    let client_key = KeyPair::generate().unwrap();
    let client_cert = CertificateParams::default()
        .signed_by(&client_key, &ca)
        .unwrap();

    let mut roots = RootCertStore::empty();
    roots.add(ca.der().clone()).unwrap();

    let config = daemon_config()
        .with_client_cert_verifier(
            WebPkiClientVerifier::builder_with_provider(
                Arc::new(roots),
                Arc::new(aws_lc_rs::default_provider()),
            )
            .build()
            .unwrap(),
        )
        .with_single_cert(vec![daemon_cert.der().clone()], daemon_key.into())
        .unwrap();

    let (host, _body) = serve_tls(config, "HTTP/1.1 204 No Content\r\n\r\n");

    let directory = TempDir::new().unwrap();

    let client = tls_client(
        &host,
        Tls {
            verify: true,
            ca_cert: Some(write(directory.path(), "ca.pem", &ca.pem())),
            client_credentials: Some(ClientCredentialPaths {
                cert: write(directory.path(), "cert.pem", &client_cert.pem()),
                key: write(directory.path(), "key.pem", &client_key.serialize_pem()),
            }),
        },
    );

    let outcome = client
        .stop_container(
            ContainerRef::try_from("ubuntu").unwrap(),
            StopTimeout::Default,
        )
        .await
        .unwrap();

    assert!(matches!(outcome, StopOutcome::Stopped));
}

#[tokio::test]
async fn stop_over_unverified_tls_is_stopped() {
    let daemon_key = KeyPair::generate().unwrap();
    let daemon_cert = CertificateParams::new(["127.0.0.1".to_owned()])
        .unwrap()
        .self_signed(&daemon_key)
        .unwrap();

    let config = daemon_config()
        .with_no_client_auth()
        .with_single_cert(vec![daemon_cert.der().clone()], daemon_key.into())
        .unwrap();

    let (host, _body) = serve_tls(config, "HTTP/1.1 204 No Content\r\n\r\n");

    let client = tls_client(
        &host,
        Tls {
            verify: false,
            ca_cert: None,
            client_credentials: None,
        },
    );

    let outcome = client
        .stop_container(
            ContainerRef::try_from("ubuntu").unwrap(),
            StopTimeout::Default,
        )
        .await
        .unwrap();

    assert!(matches!(outcome, StopOutcome::Stopped));
}

#[tokio::test]
async fn daemon_certificate_from_another_ca_is_a_send_error() {
    let daemon_key = KeyPair::generate().unwrap();
    let daemon_cert = CertificateParams::new(["127.0.0.1".to_owned()])
        .unwrap()
        .self_signed(&daemon_key)
        .unwrap();

    let config = Arc::new(
        daemon_config()
            .with_no_client_auth()
            .with_single_cert(vec![daemon_cert.der().clone()], daemon_key.into())
            .unwrap(),
    );

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();

    std::thread::spawn(move || {
        let mut stream = StreamOwned::new(
            ServerConnection::new(config).unwrap(),
            listener.accept().unwrap().0,
        );

        stream.read(&mut [0_u8; 1]).unwrap_err();
    });

    let directory = TempDir::new().unwrap();

    let client = tls_client(
        &format!("tcp://{}", address),
        Tls {
            verify: true,
            ca_cert: Some(write(
                directory.path(),
                "ca.pem",
                &certificate_authority().pem(),
            )),
            client_credentials: None,
        },
    );

    let result = client
        .stop_container(
            ContainerRef::try_from("ubuntu").unwrap(),
            StopTimeout::Default,
        )
        .await;

    assert!(matches!(
        result,
        Err(ApiEndpointCallError::Transport(TransportError::Send(_)))
    ));
}
