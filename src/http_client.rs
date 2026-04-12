use std::convert::Into;
use std::str::FromStr as _;

use color_eyre::eyre;
use hashbrown::HashMap;
use http_body_util::Empty;
use hyper::body::{Body, Bytes};
use hyper::header::{HeaderName, IntoHeaderName};
use hyper::http::HeaderValue;
use hyper::http::uri::PathAndQuery;
use hyper::{Method, Request, Response, Uri};
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::Connect;
use hyper_util::rt::TokioExecutor;

pub(crate) fn build_request<B>(
    base: Uri,
    path_and_query: &str,
    method: Method,
) -> Result<Request<B>, eyre::Report>
where
    B: Body + Send + 'static + Default,
    B::Data: Send,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    build_request_with_headers_and_body::<_, HeaderName>(
        base,
        path_and_query,
        HashMap::default(),
        method,
        B::default(),
    )
}

#[expect(unused, reason = "WIP")]
pub(crate) fn build_request_with_body<B>(
    base: Uri,
    path_and_query: &str,
    method: Method,
    body: B,
) -> Result<Request<B>, eyre::Report>
where
    B: Body + Send + 'static,
    B::Data: Send,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    build_request_with_headers_and_body::<B, HeaderName>(
        base,
        path_and_query,
        HashMap::default(),
        method,
        body,
    )
}

#[expect(unused, reason = "WIP")]
pub(crate) fn build_request_with_headers<K>(
    base: Uri,
    path_and_query: &str,
    headers: HashMap<K, HeaderValue>,
    method: Method,
) -> Result<Request<Empty<Bytes>>, eyre::Report>
where
    K: IntoHeaderName,
{
    build_request_with_headers_and_body(
        base,
        path_and_query,
        headers,
        method,
        Empty::<Bytes>::new(),
    )
}

pub(crate) fn build_request_with_headers_and_body<B, K>(
    base: Uri,
    path_and_query: &str,
    headers: HashMap<K, HeaderValue>,
    method: Method,
    body: B,
) -> Result<Request<B>, eyre::Report>
where
    B: Body + Send + 'static,
    B::Data: Send,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    K: IntoHeaderName,
{
    let full_url = build_uri(base, path_and_query)?;

    let mut request = Request::builder()
        .uri(full_url)
        .method(method)
        .body::<B>(body)?;

    let request_headers = request.headers_mut();

    for (k, v) in headers {
        request_headers.insert(k, v);
    }

    Ok(request)
}

pub fn build_client<C, B>(connector: C) -> Client<C, B>
where
    C: Connect + Clone + Send + Sync + 'static,
    B: Body + Send,
    B::Data: Send,
{
    Client::builder(TokioExecutor::new()).build::<_, B>(connector)
}

/// Executes a request on a client.
///
/// # Errors
///
/// When the request errors.
pub async fn execute_request<C, B>(
    client: &Client<C, B>,
    request: Request<B>,
) -> Result<Response<hyper::body::Incoming>, hyper_util::client::legacy::Error>
where
    C: Connect + Clone + Send + Sync + 'static,
    B: Body + Send + 'static + Unpin,
    B::Data: Send,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    let response = client.request(request).await?;

    Ok(response)
}

fn build_uri(base_url: Uri, path_and_query: &str) -> Result<Uri, eyre::Report> {
    let mut parts = base_url.into_parts();

    parts.path_and_query = Some(PathAndQuery::from_str(path_and_query)?);

    Uri::from_parts(parts).map_err(Into::into)
}
