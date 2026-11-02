#![expect(clippy::tests_outside_test_module, reason = "Integration tests")]
use std::io::{Read as _, Write as _};
use std::net::TcpListener;
use std::str::FromStr as _;

use pretty_assertions::assert_eq;
use twistlock::client::Client;
use twistlock::config::Endpoint;

#[tokio::test]
async fn remote_daemon_is_reached_through_the_environment_proxy() {
    let proxy = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = proxy.local_addr().unwrap();

    // SAFETY: the only test in this binary, no other thread reads the environment
    unsafe {
        std::env::set_var("HTTP_PROXY", format!("http://{}", address));
    }
    // SAFETY: the only test in this binary, no other thread reads the environment
    unsafe {
        std::env::remove_var("NO_PROXY");
    }
    // SAFETY: the only test in this binary, no other thread reads the environment
    unsafe {
        std::env::remove_var("no_proxy");
    }

    let request_line = std::thread::spawn(move || {
        let (mut stream, _peer) = proxy.accept().unwrap();

        let mut request = Vec::new();
        let mut byte = [0_u8; 1];

        while !request.ends_with(b"\r\n\r\n") {
            stream.read_exact(&mut byte).unwrap();
            request.push(byte[0]);
        }

        stream
            .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\n[]")
            .unwrap();

        String::from_utf8(request)
            .unwrap()
            .lines()
            .next()
            .unwrap()
            .to_owned()
    });

    // a non-loopback address reserved for documentation (RFC 5737)
    let endpoint = Endpoint::from_str("tcp://192.0.2.1:2375").unwrap();

    let networks = Client::build(endpoint)
        .unwrap()
        .list_networks()
        .send()
        .await
        .unwrap();

    assert!(networks.is_empty());
    assert_eq!(
        request_line.join().unwrap(),
        "GET http://192.0.2.1:2375/networks HTTP/1.1"
    );
}
