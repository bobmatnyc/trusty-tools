//! The `tcode-bridge://` custom URI scheme — unary REST calls with no socket
//! (#10166).
//!
//! Why: the webview used to `fetch()` the bridge on a loopback TCP port, the
//! last listener on the trusty-code side. A Tauri custom scheme hands each
//! webview request to this process as a value, so the same guarded router can
//! answer it in-process and nothing has to listen. The webview's ~30 `fetch()`
//! sites keep their `${base}/path` shape; only the base changes.
//!
//! What a scheme cannot do is stream: Tauri's `UriSchemeResponder` takes one
//! whole response. The two event streams therefore go over an IPC `Channel`
//! instead ([`crate::bridge::channel`]), and this handler refuses a response
//! that turns out to be `text/event-stream` rather than waiting on it forever.
//!
//! **macOS only (owner ruling Q5, 2026-10-23).** WebKit on macOS and Linux
//! addresses the scheme as `tcode-bridge://localhost`; Windows and Android use
//! `http://tcode-bridge.localhost`, which this crate does not support.
//!
//! What: [`dispatch`] turns a scheme request into a router request, runs the
//! router under a deadline in its own task, and always returns a response —
//! a hung or panicking route becomes `504` or `500`, never an unanswered
//! responder, which would hang the webview's `fetch()`.
//!
//! Test: `tests` below, and `tests/bridge_uds.rs`, which sends every unary case
//! through [`dispatch`].

use std::time::Duration;

use axum::body::Body;
use axum::http::{self, HeaderValue, StatusCode, Uri, header};
use serde_json::json;
use tower::ServiceExt as _;
use tracing::warn;
use trusty_common::server::{SelfOrigins, same_origin_cors_allows};

use super::{CALL_TIMEOUT, SOCKET_APP_NAME};

/// The scheme name registered with Tauri.
pub const SCHEME: &str = "tcode-bridge";

/// The origin the webview `fetch()`es — the macOS/Linux WebKit form.
pub const SCHEME_ORIGIN: &str = "tcode-bridge://localhost";

/// The longest one scheme request may take before it is answered `504`.
///
/// Why this figure: the slowest unary route, the Markdown transcript, makes two
/// daemon calls of up to [`CALL_TIMEOUT`] each. Three budgets covers both with
/// room to render, and still bounds a route that never answers.
pub const SCHEME_RESPONSE_TIMEOUT: Duration = Duration::from_secs(CALL_TIMEOUT.as_secs() * 3);

/// Largest response body the scheme hands back, in bytes.
///
/// Why: a response is buffered whole before it reaches the webview. Four
/// daemon frames covers a transcript rendered to Markdown; anything larger is
/// a bug worth a `502`, not an unbounded allocation.
const MAX_RESPONSE_BYTES: usize = 4 * trusty_common::uds::MAX_FRAME_BYTES as usize;

/// Answer one scheme request through `router`, within [`SCHEME_RESPONSE_TIMEOUT`].
///
/// Why: see the module docs — the responder must always be answered.
/// What: delegates to [`dispatch_within`] with the production deadline.
/// Test: `tests/bridge_uds.rs` routes every unary case through here.
pub async fn dispatch(
    router: axum::Router,
    request: http::Request<Vec<u8>>,
) -> http::Response<Vec<u8>> {
    dispatch_within(router, request, SCHEME_RESPONSE_TIMEOUT).await
}

/// Answer one scheme request through `router` within `deadline`.
///
/// Why the router runs in its own task: a panic inside a route must become a
/// `500` the webview can read, and only a `JoinHandle` turns a panic into a
/// value. The deadline aborts the task, which drops any daemon socket it held.
///
/// What: rewrites the URI to its path and query (the router matches on path,
/// and the scheme's authority is always this app), drops `Accept-Encoding` so
/// the buffered body is the router's bytes rather than a gzip stream, runs the
/// router, and buffers the body up to [`MAX_RESPONSE_BYTES`]. Every failure is
/// a JSON `{"error", "service"}` body with a non-2xx status:
///
/// - the request could not be rebuilt → `400`;
/// - the route answered `text/event-stream` → `400` naming the IPC stream
///   command, since a scheme response cannot stream;
/// - the body could not be read or is over the limit → `502`;
/// - the route panicked or was cancelled → `500`;
/// - the deadline passed → `504`.
///
/// A trusted request `Origin` is reflected on these errors so the webview can
/// read the status; the router's own responses carry its CORS layer's headers.
///
/// Test: `a_panicking_route_is_a_500_not_a_hung_responder`,
/// `a_route_that_never_answers_is_a_504`,
/// `an_event_stream_is_refused_rather_than_buffered_forever`,
/// `a_malformed_path_is_answered_not_dropped`.
pub async fn dispatch_within(
    router: axum::Router,
    request: http::Request<Vec<u8>>,
    deadline: Duration,
) -> http::Response<Vec<u8>> {
    let origin = request
        .headers()
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok())
        .filter(|o| same_origin_cors_allows(o, &SelfOrigins::default()))
        .and_then(|o| HeaderValue::from_str(o).ok());
    let fail = |status: StatusCode, reason: String| error_response(status, &reason, origin.clone());

    let request = match into_router_request(request) {
        Ok(r) => r,
        Err(reason) => return fail(StatusCode::BAD_REQUEST, reason),
    };

    let task = tokio::spawn(async move {
        // The router's error type is `Infallible`; the `Err` arm cannot run.
        let response = match router.oneshot(request).await {
            Ok(r) => r,
            Err(never) => match never {},
        };
        if is_event_stream(&response) {
            return Err((
                StatusCode::BAD_REQUEST,
                "an event stream cannot be fetched over tcode-bridge://; \
                 open it with the open_daemon_stream command"
                    .to_string(),
            ));
        }
        let (parts, body) = response.into_parts();
        let bytes = axum::body::to_bytes(body, MAX_RESPONSE_BYTES)
            .await
            .map_err(|e| {
                (
                    StatusCode::BAD_GATEWAY,
                    format!("the bridge could not read its own response: {e}"),
                )
            })?;
        Ok(http::Response::from_parts(parts, bytes.to_vec()))
    });
    let abort = task.abort_handle();

    match tokio::time::timeout(deadline, task).await {
        Ok(Ok(Ok(response))) => response,
        Ok(Ok(Err((status, reason)))) => fail(status, reason),
        Ok(Err(join)) => {
            warn!("bridge scheme: the route did not finish: {join}");
            fail(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("the bridge route failed: {join}"),
            )
        }
        Err(_) => {
            abort.abort();
            fail(
                StatusCode::GATEWAY_TIMEOUT,
                format!(
                    "the bridge did not answer within {}s",
                    deadline.as_secs_f32()
                ),
            )
        }
    }
}

/// Rebuild a scheme request as one the router can match.
///
/// What: same method and body; the URI cut to path and query (`/` when the
/// webview sent none); every header but `Accept-Encoding`.
fn into_router_request(request: http::Request<Vec<u8>>) -> Result<http::Request<Body>, String> {
    let (mut parts, body) = request.into_parts();
    let path_and_query = parts
        .uri
        .path_and_query()
        .map_or("/", http::uri::PathAndQuery::as_str);
    parts.uri = path_and_query
        .parse::<Uri>()
        .map_err(|e| format!("the request path is not a valid URI: {e}"))?;
    parts.headers.remove(header::ACCEPT_ENCODING);
    Ok(http::Request::from_parts(parts, Body::from(body)))
}

/// Does `response` declare an SSE body?
fn is_event_stream(response: &http::Response<Body>) -> bool {
    response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("text/event-stream"))
}

/// The JSON error body every bridge failure carries, built without a fallible
/// builder so this path cannot itself fail to answer.
fn error_response(
    status: StatusCode,
    reason: &str,
    origin: Option<HeaderValue>,
) -> http::Response<Vec<u8>> {
    let body = json!({ "error": reason, "service": SOCKET_APP_NAME }).to_string();
    let mut response = http::Response::new(body.into_bytes());
    *response.status_mut() = status;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    if let Some(origin) = origin {
        response
            .headers_mut()
            .insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, origin);
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::routing::get;

    fn get_request(uri: &str) -> http::Request<Vec<u8>> {
        http::Request::builder()
            .method("GET")
            .uri(uri)
            .header("origin", "tauri://localhost")
            .body(Vec::new())
            .expect("request")
    }

    fn error_of(response: &http::Response<Vec<u8>>) -> String {
        let body: serde_json::Value = serde_json::from_slice(response.body()).expect("json body");
        body["error"].as_str().unwrap_or_default().to_string()
    }

    /// Why (#10166): a never-answered responder hangs the webview's `fetch()`
    /// for good. A panic inside a route must come back as a status.
    /// Test: this is the test.
    #[tokio::test]
    async fn a_panicking_route_is_a_500_not_a_hung_responder() {
        async fn boom() -> &'static str {
            panic!("route panicked")
        }
        let router = axum::Router::new().route("/boom", get(boom));
        let response = dispatch(router, get_request("tcode-bridge://localhost/boom")).await;
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(
            response.headers().get(header::ACCESS_CONTROL_ALLOW_ORIGIN),
            Some(&HeaderValue::from_static("tauri://localhost")),
            "the webview must be able to read the failure"
        );
    }

    /// Why: a route that never answers must still be answered — late, and as a
    /// failure.
    /// Test: this is the test.
    #[tokio::test]
    async fn a_route_that_never_answers_is_a_504() {
        let router = axum::Router::new().route(
            "/hang",
            get(|| async {
                std::future::pending::<()>().await;
                "unreachable"
            }),
        );
        let response = dispatch_within(
            router,
            get_request("tcode-bridge://localhost/hang"),
            Duration::from_millis(200),
        )
        .await;
        assert_eq!(response.status(), StatusCode::GATEWAY_TIMEOUT);
    }

    /// Why: a scheme response is buffered whole, and an SSE body never ends.
    /// Buffering one would leave the responder unanswered for the stream's
    /// life.
    /// Test: this is the test.
    #[tokio::test]
    async fn an_event_stream_is_refused_rather_than_buffered_forever() {
        let router = axum::Router::new().route(
            "/sessions/s-1/events",
            get(|| async {
                let forever = futures_never();
                axum::response::Response::builder()
                    .header(header::CONTENT_TYPE, "text/event-stream")
                    .body(Body::from_stream(forever))
                    .expect("response")
            }),
        );
        let response = tokio::time::timeout(
            Duration::from_secs(5),
            dispatch(
                router,
                get_request("tcode-bridge://localhost/sessions/s-1/events"),
            ),
        )
        .await
        .expect("the scheme must answer an SSE route promptly");
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert!(error_of(&response).contains("open_daemon_stream"));
    }

    /// Why: a path the router cannot decode must be answered with a status,
    /// not dropped.
    /// Test: this is the test.
    #[tokio::test]
    async fn a_malformed_path_is_answered_not_dropped() {
        let router = axum::Router::new().route(
            "/{*path}",
            get(|axum::extract::Path(p): axum::extract::Path<String>| async move { p }),
        );
        // `%FF` is not UTF-8, so the `Path<String>` extractor refuses it.
        let response = dispatch(router, get_request("tcode-bridge://localhost/%FF")).await;
        assert!(response.status().is_client_error(), "{}", response.status());
    }

    /// A body stream that yields nothing, ever — an SSE tail on a quiet stream.
    fn futures_never()
    -> impl futures_util::Stream<Item = Result<axum::body::Bytes, std::convert::Infallible>> {
        futures_util::stream::pending()
    }
}
