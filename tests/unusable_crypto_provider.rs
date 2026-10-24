#![expect(clippy::tests_outside_test_module, reason = "Integration tests")]
use http::Uri;
use rustls::crypto::{CryptoProvider, aws_lc_rs};
use twistlock::client::{BuildError, Client};
use twistlock::config::Endpoint;

#[test]
fn https_endpoint_with_an_unusable_crypto_provider_is_a_provider_error() {
    CryptoProvider {
        cipher_suites: Vec::new(),
        ..aws_lc_rs::default_provider()
    }
    .install_default()
    .unwrap();

    let endpoint = Endpoint::Direct(Uri::from_static("https://localhost:2376"));

    let error = Client::build(endpoint, None, None).unwrap_err();

    assert!(matches!(error, BuildError::Provider(_)));
}
