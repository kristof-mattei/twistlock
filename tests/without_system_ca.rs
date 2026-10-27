#![expect(clippy::tests_outside_test_module, reason = "Integration tests")]
use std::str::FromStr as _;

use tempfile::TempDir;
use twistlock::client::Client;
use twistlock::config::Endpoint;

#[test]
fn http_endpoint_builds_without_system_ca_certificates() {
    let directory = TempDir::new().unwrap();

    // SAFETY: the only test in this binary, no other thread reads the environment
    unsafe {
        std::env::set_var("SSL_CERT_FILE", directory.path().join("missing.pem"));
    }
    // SAFETY: the only test in this binary, no other thread reads the environment
    unsafe {
        std::env::remove_var("SSL_CERT_DIR");
    }

    let endpoint = Endpoint::from_str("tcp://127.0.0.1:2375").unwrap();

    Client::build(endpoint).unwrap();
}
