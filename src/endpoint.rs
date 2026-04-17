use color_eyre::eyre;
use http::StatusCode;
use hyper::Method;
use serde::de::DeserializeOwned;
use thiserror::Error;

/// Error type for endpoint calls.
#[derive(Debug, Error)]
pub enum EndpointCallError<TError>
where
    TError: std::fmt::Debug,
{
    /// Non-success status and body parsed as the endpoint's typed error.
    #[error("Error: {:?}", .0)]
    Typed(TError),
    /// Non-success status, body is valid JSON but didn't match `TError`.
    #[error("Error: {}", .0)]
    Generic(serde_json::Value),
    /// Non-success status, body was not valid JSON.
    #[error("HTTP error: {status}, body: {body}")]
    HttpError { status: StatusCode, body: String },
    /// Transport or serialization failure.
    #[error("{0}")]
    Transport(eyre::Report),
}

/// A typed Docker API endpoint.
pub trait Endpoint {
    /// Request.
    type Request: ?Sized;
    /// Successful response.
    type Response: DeserializeOwned;
    /// Error response.
    type Error: DeserializeOwned + std::fmt::Debug;

    /// HTTP method for this endpoint.
    const METHOD: Method;

    /// The path and query string for this request.
    ///
    /// # Errors
    ///
    /// Returns an error if request parameters cannot be serialized.
    fn path_and_query(request: &Self::Request) -> Result<String, std::io::Error>;

    /// Parse the response body into the response type.
    ///
    /// # Errors
    ///
    /// Returns an error if the response body cannot be parsed.
    fn parse_response(bytes: &[u8]) -> Result<Self::Response, serde_json::Error> {
        serde_json::from_slice(bytes)
    }
}
