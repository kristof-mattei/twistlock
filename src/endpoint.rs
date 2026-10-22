use http::StatusCode;
use hyper::Method;
use serde::de::DeserializeOwned;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum TransportError {
    #[error("Failed to build the request")]
    Build(#[source] http::Error),
    #[error("Failed to send the request")]
    Send(#[source] hyper_util::client::legacy::Error),
    #[error("Failed to read the response body")]
    Body(#[source] hyper::Error),
}

/// Error type for endpoint calls.
#[derive(Debug, Error)]
pub enum ApiEndpointCallError<TError>
where
    TError: std::fmt::Debug,
{
    /// Unaccepted status and body parsed as the endpoint's typed error.
    #[error("Error: {:?}", .0)]
    Typed(TError),
    /// Unaccepted status, body is valid JSON but didn't match `TError`.
    #[error("Error: {}", .0)]
    Generic(serde_json::Value),
    /// Unaccepted status, body was not valid JSON.
    #[error("HTTP error: {status}, body: {body}")]
    HttpError { status: StatusCode, body: String },
    #[error("Failed to serialize the request parameters")]
    Query(#[source] std::io::Error),
    #[error(transparent)]
    Transport(#[from] TransportError),
    #[error("Failed to deserialize the response")]
    Deserialize(#[source] serde_json::Error),
}

/// A typed Docker API endpoint.
pub(crate) trait ApiEndpoint {
    /// Request.
    type Request<'r>: ?Sized;
    /// Accepted response.
    type Response;
    /// Error response.
    type Error: DeserializeOwned + std::fmt::Debug;

    /// HTTP method for this endpoint.
    const METHOD: Method;

    /// The path and query string for this request.
    ///
    /// # Errors
    ///
    /// Returns an error if request parameters cannot be serialized.
    fn path_and_query(request: &Self::Request<'_>) -> Result<String, std::io::Error>;

    /// Whether a response with `status` goes to [`ApiEndpoint::parse_response`] instead of becoming an error.
    #[must_use]
    fn accepts(status: StatusCode) -> bool {
        status.is_success()
    }

    /// Parse an accepted response into the response type.
    ///
    /// # Errors
    ///
    /// Returns an error if the response body cannot be parsed.
    fn parse_response(
        status: StatusCode,
        bytes: &[u8],
    ) -> Result<Self::Response, serde_json::Error>;
}
