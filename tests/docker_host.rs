#![expect(clippy::tests_outside_test_module, reason = "Integration tests")]
use pretty_assertions::assert_eq;
use twistlock::config::Endpoint;

#[test]
fn from_env_parses_docker_host() {
    // SAFETY: the only test in this binary, no other thread reads the environment
    unsafe {
        std::env::set_var("DOCKER_HOST", "tcp://example.com:2375");
    }

    let endpoint = Endpoint::from_env().unwrap();

    assert_eq!(endpoint.to_string(), "tcp://example.com:2375");
}
