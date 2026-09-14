//! Turning an [`HttpRequestFrame`] into an axum call and back (#6433 slice 2).
//!
//! Why: the router is the definition of what this daemon answers. Slice 2 moves
//! the TRANSPORT to a Unix socket and leaves the route table alone, so the only
//! new code is the translation at the edge — build a `http::Request`, hand it to
//! the same `Router` the TCP listener used to hand it to, read the response
//! back out.
//!
//! What: [`dispatch`] does exactly that, and is the whole body of the
//! `agents.request` method. It never panics on caller input: a bad method
//! token, a bad path, a bad header name and an oversized body each become a
//! coded `invalid_params` frame.
//!
//! Test: `dispatch_round_trips_a_get`, `dispatch_carries_a_json_post_body`,
//! `dispatch_rejects_an_unparsable_method`, `dispatch_reports_a_404_status`.

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderName, HeaderValue, Method, Request};
use tower::ServiceExt as _;
use trusty_common::uds::server::RpcError;

use super::wire::{HttpRequestFrame, HttpResponseFrame};

/// Largest response body one `agents.request` may carry back.
///
/// Why 6 MiB against `trusty_common::uds::MAX_FRAME_BYTES`' 8 MiB: the body is
/// base64 in the frame, so it inflates by 4/3, and the frame also carries the
/// JSON-RPC envelope and the header list. 6 MiB of body encodes to 8 MiB of
/// base64 — the ceiling — so this is the largest figure that cannot produce a
/// frame the peer refuses to read. A route that needs more is a route that
/// should stream (`RpcRouter::typed_stream`), not one that should raise this.
///
/// Test: `dispatch_refuses_an_oversized_response_body`.
pub(super) const MAX_BODY_BYTES: usize = 6 * 1024 * 1024;

/// Run one HTTP exchange against `router`.
///
/// Why a clone per call rather than a shared `&mut`: `tower::Service::oneshot`
/// consumes the service, and `Router` is cheap to clone (an `Arc` around the
/// route table). Cloning is what lets concurrent connections share one router
/// without a lock.
/// What: builds `http::Request` from `frame`, awaits the router, and reads the
/// response status, headers and body back into an [`HttpResponseFrame`].
/// # Errors
///
/// `invalid_params` when the frame does not describe a well-formed HTTP request
/// (bad method token, bad URI, bad header name or value, body that is not
/// base64); `internal` when the response body cannot be read or exceeds
/// [`MAX_BODY_BYTES`].
///
/// Test: see the module docs.
pub(super) async fn dispatch(
    router: Router,
    frame: HttpRequestFrame,
) -> Result<HttpResponseFrame, RpcError> {
    let method = Method::from_bytes(frame.method.as_bytes())
        .map_err(|e| RpcError::invalid_params(format!("bad HTTP method {:?}: {e}", frame.method)))?;
    let body = frame
        .body_bytes()
        .map_err(|e| RpcError::invalid_params(format!("body is not valid base64: {e}")))?;

    let mut builder = Request::builder().method(method).uri(&frame.path);
    for (name, value) in &frame.headers {
        let name = HeaderName::from_bytes(name.as_bytes())
            .map_err(|e| RpcError::invalid_params(format!("bad header name {name:?}: {e}")))?;
        let value = HeaderValue::from_str(value)
            .map_err(|e| RpcError::invalid_params(format!("bad value for header {name}: {e}")))?;
        builder = builder.header(name, value);
    }
    let request = builder
        .body(Body::from(body))
        .map_err(|e| RpcError::invalid_params(format!("bad request for {:?}: {e}", frame.path)))?;

    let response = router
        .oneshot(request)
        .await
        .map_err(|e| RpcError::internal(format!("router call failed: {e}")))?;

    let status = response.status().as_u16();
    let headers = response
        .headers()
        .iter()
        .map(|(name, value)| {
            (
                name.as_str().to_string(),
                value.to_str().unwrap_or_default().to_string(),
            )
        })
        .collect();
    let bytes = axum::body::to_bytes(response.into_body(), MAX_BODY_BYTES)
        .await
        .map_err(|e| {
            RpcError::internal(format!(
                "response body for {} exceeded {MAX_BODY_BYTES} bytes or could not be read: {e}",
                frame.path
            ))
        })?;

    Ok(HttpResponseFrame::new(status, headers, &bytes))
}
