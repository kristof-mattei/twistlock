use http::{Method, StatusCode};
use serde::de::DeserializeOwned;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum TransportError {
    #[error("Failed to send the request")]
    Send(#[source] reqwest::Error),
    #[error("Failed to read the response body")]
    Body(#[source] reqwest::Error),
}

/// Error type for endpoint calls.
#[derive(Debug, Error)]
pub enum ApiEndpointCallError {
    /// Unhandled status, body is the daemon's `ErrorResponse`.
    #[error("Daemon error: {}, message: {}", .status, .message)]
    Daemon { status: StatusCode, message: String },
    /// Unhandled status, body is not an `ErrorResponse`.
    #[error("HTTP error: {}, body: {}", .status, .body)]
    HttpError { status: StatusCode, body: String },
    #[error("Failed to serialize the request parameters")]
    Query(#[source] std::io::Error),
    #[error(transparent)]
    Transport(#[from] TransportError),
    #[error("Failed to deserialize the response")]
    Deserialize(#[source] serde_json::Error),
}

pub(crate) fn is_success(status: StatusCode) -> bool {
    status.is_success()
}

/// Failure of [`ApiEndpoint::parse_response`].
pub(crate) enum ParseError {
    /// The endpoint does not handle the status.
    Status,
    /// The body does not deserialize.
    Body(serde_json::Error),
}

/// Parses a `2xx` response with a JSON body.
pub(crate) fn parse_json<T: DeserializeOwned>(
    status: StatusCode,
    bytes: &[u8],
) -> Result<T, ParseError> {
    if is_success(status) {
        serde_json::from_slice(bytes).map_err(ParseError::Body)
    } else {
        Err(ParseError::Status)
    }
}

/// A typed Docker API endpoint.
pub(crate) trait ApiEndpoint {
    /// Request.
    type Request<'r>: ?Sized;
    /// Response for a handled status.
    type Response;

    /// HTTP method for this endpoint.
    const METHOD: Method;

    /// The path and query string for this request.
    ///
    /// # Errors
    ///
    /// Returns an error if request parameters cannot be serialized.
    fn path_and_query(request: &Self::Request<'_>) -> Result<String, std::io::Error>;

    /// Parse a response into the response type.
    ///
    /// # Errors
    ///
    /// * [`ParseError::Status`] if the endpoint does not handle `status`
    /// * [`ParseError::Body`] if the body does not deserialize
    fn parse_response(status: StatusCode, bytes: &[u8]) -> Result<Self::Response, ParseError>;
}
