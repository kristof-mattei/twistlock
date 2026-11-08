//! A client for the Docker Engine API.
//!
//! [`Client`](client::Client) connects to the daemon over:
//!
//! - a Unix socket (Unix only)
//! - a named pipe (Windows only)
//! - TCP
//! - TCP with TLS, see [`Endpoint::with_tls`](config::Endpoint::with_tls)
//!
//! Requires Docker Engine 25.0 or later.
//!
//! # Example
//!
//! ```no_run
//! use twistlock::client::Client;
//! use twistlock::config::Endpoint;
//! use twistlock::filters::Filters;
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     // an empty `DOCKER_HOST` selects the platform's default socket
//!     let endpoint = std::env::var("DOCKER_HOST")
//!         .unwrap_or_default()
//!         .parse::<Endpoint>()?;
//!
//!     let client = Client::build(endpoint)?;
//!
//!     for container in client.list_containers(&Filters::default()).send().await? {
//!         println!("{} {}", container.id, container.names.join(", "));
//!     }
//!
//!     Ok(())
//! }
//! ```
pub mod client;
pub mod config;
pub mod endpoint;
pub(crate) mod endpoints;
pub mod filters;
pub mod models;
