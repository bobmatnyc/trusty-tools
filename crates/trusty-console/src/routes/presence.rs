//! The browser leg of passkey enrolment for trusty-mpm presence devices
//! (#8878 PR2b-2).
//!
//! Why: a passkey is created by the browser on the device, and WebAuthn binds
//! it to the page's origin, so the page must be served on the console's
//! tailnet origin (`[presence] rp_origin`). `tm presence devices add` prints a
//! one-time link to it (Architect Q3). The page is off until owner decision
//! D24 flips `[presence] browser_enrol` (Architect Q4).
//! What: [`router`] serves the page at [`ENROL_PAGE_PATH`] and two POST
//! routes that relay to the tm daemon's `mpm.presence.register.start` and
//! `.finish` over its Unix socket. Before anything else, every request asks
//! the daemon's `mpm.presence.status` for the enrol origin: none (the switch
//! is off) is `404`, an unreachable daemon `503`. The request must then name
//! that origin's host in `Host`, and a POST must carry exactly that `Origin`,
//! or it is `403` — this check does not rely on the listener's peer gate.
//! The ticket is required by the body type, so a request without one never
//! reaches the daemon; the daemon checks its seal, purpose, dates and single
//! use. Every response is no-store and never framed.
//! Test: `presence_tests.rs` beside this file.

use std::path::PathBuf;
use std::time::Duration;

use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use reqwest::Url;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use trusty_common::uds::server::RpcResponse;

/// The page; trusty-mpm's `presence_enrol::ENROL_PAGE_PATH` must match.
pub const ENROL_PAGE_PATH: &str = "/tools/presence/enrol/";

/// The page's call that opens the registration.
pub const ENROL_START_PATH: &str = "/api/console/presence/enrol/start";

/// The page's call that finishes it.
pub const ENROL_FINISH_PATH: &str = "/api/console/presence/enrol/finish";

/// The enrol page, embedded (no bundle, no build step).
pub const ENROL_HTML: &str = include_str!("presence_enrol.html");

/// The daemon whose socket carries `mpm.presence.*`.
const MPM_SERVICE: &str = "trusty-mpm";

/// The bound on one daemon call.
const CALL_TIMEOUT: Duration = Duration::from_secs(15);

/// The largest daemon answer read (registration options are a few KiB).
const MAX_FRAME_BYTES: u64 = 1 << 20;

/// Where the tm daemon's socket is, or `None` when it cannot be resolved.
#[derive(Clone, Debug)]
pub struct PresenceBridge {
    socket: Option<PathBuf>,
}

impl PresenceBridge {
    /// The live daemon socket, resolved the way `tm` resolves it.
    pub fn live() -> Self {
        Self {
            socket: trusty_common::daemon_socket_path(MPM_SERVICE).ok(),
        }
    }

    /// A bridge to the daemon at `socket` (tests).
    pub fn with_socket(socket: PathBuf) -> Self {
        Self {
            socket: Some(socket),
        }
    }
}

/// Why an enrol request was not relayed.
#[derive(Debug, PartialEq, Eq)]
enum Refusal {
    /// `[presence] browser_enrol` is off: the page does not exist.
    NotEnabled,
    /// The daemon could not be asked.
    Unavailable(String),
    /// `Host` or `Origin` is not the enrol origin.
    WrongTarget(&'static str),
    /// The daemon refused the step; its message is shown on the page.
    Refused(String),
}

impl IntoResponse for Refusal {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            Self::NotEnabled => (
                StatusCode::NOT_FOUND,
                "enrolment via browser is not enabled".to_owned(),
            ),
            Self::Unavailable(m) => (StatusCode::SERVICE_UNAVAILABLE, m),
            Self::WrongTarget(why) => {
                tracing::warn!(why, "presence enrol refused a request not addressed to it");
                (StatusCode::FORBIDDEN, format!("refused: {why}"))
            }
            Self::Refused(m) => (StatusCode::UNPROCESSABLE_ENTITY, m),
        };
        (status, Json(json!({ "error": message }))).into_response()
    }
}

/// One `mpm.presence.*` call; an error frame is [`Refusal::Refused`].
async fn call(bridge: &PresenceBridge, method: &str, params: Value) -> Result<Value, Refusal> {
    let socket = bridge
        .socket
        .as_deref()
        .ok_or_else(|| Refusal::Unavailable("the tm daemon socket is unknown".to_owned()))?;
    let frame = json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params });
    let reply: RpcResponse = trusty_common::uds::send_framed_request_capped(
        socket,
        &frame,
        CALL_TIMEOUT,
        MAX_FRAME_BYTES,
    )
    .await
    .map_err(|e| Refusal::Unavailable(format!("the tm daemon did not answer {method}: {e}")))?;
    match (reply.result, reply.error) {
        (_, Some(e)) => Err(Refusal::Refused(e.message)),
        (Some(v), None) => Ok(v),
        (None, None) => Err(Refusal::Unavailable(format!("{method}: empty reply"))),
    }
}

/// The `host[:port]` a request to `origin` names in `Host`.
fn authority(origin: &Url) -> Option<String> {
    let host = origin.host_str()?;
    Some(match origin.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_owned(),
    })
}

/// Admit a request only while enrolment is on and addressed to its origin.
///
/// Why: #8878 plan §2 — the HTTP leg must not rest on the peer gate alone.
/// What: asks the daemon for the enrol origin (none → [`Refusal::NotEnabled`];
/// a refused or failed status → [`Refusal::Unavailable`]); `Host` must equal
/// that origin's authority, and when `need_origin` the `Origin` header must
/// equal the origin itself.
/// Test: `the_enrol_routes_are_absent_while_the_switch_is_off_8878`,
/// `a_request_not_addressed_to_the_enrol_origin_is_refused_8878`.
async fn gate(
    bridge: &PresenceBridge,
    headers: &HeaderMap,
    need_origin: bool,
) -> Result<(), Refusal> {
    let status = match call(bridge, "mpm.presence.status", json!({})).await {
        Err(Refusal::Refused(m)) => return Err(Refusal::Unavailable(m)),
        other => other?,
    };
    let origin = status["enrol_origin"].as_str().ok_or(Refusal::NotEnabled)?;
    let parsed = Url::parse(origin)
        .map_err(|e| Refusal::Unavailable(format!("the enrol origin is invalid: {e}")))?;
    let expected = authority(&parsed)
        .ok_or_else(|| Refusal::Unavailable("the enrol origin has no host".to_owned()))?;
    let header_str = |name: header::HeaderName| headers.get(name).and_then(|v| v.to_str().ok());
    if !header_str(header::HOST).is_some_and(|h| h.eq_ignore_ascii_case(&expected)) {
        return Err(Refusal::WrongTarget("Host is not the enrol origin"));
    }
    if need_origin && header_str(header::ORIGIN) != Some(origin.trim_end_matches('/')) {
        return Err(Refusal::WrongTarget("Origin is not the enrol origin"));
    }
    Ok(())
}

/// `GET /tools/presence/enrol/` — the page, when enrolment is on.
///
/// Test: `the_enrol_page_is_served_only_on_its_origin_8878`.
async fn page(State(bridge): State<PresenceBridge>, headers: HeaderMap) -> Response {
    match gate(&bridge, &headers, false).await {
        Ok(()) => Html(ENROL_HTML).into_response(),
        Err(refusal) => refusal.into_response(),
    }
}

/// The start body: the ticket from the link's fragment.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StartBody {
    ticket: Map<String, Value>,
}

/// The finish body: the ticket and the browser's credential.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FinishBody {
    ticket: Map<String, Value>,
    credential: Map<String, Value>,
}

/// `POST /api/console/presence/enrol/start` → `mpm.presence.register.start`.
///
/// Test: `start_and_finish_relay_to_the_daemon_8878`.
async fn start(
    State(bridge): State<PresenceBridge>,
    headers: HeaderMap,
    Json(body): Json<StartBody>,
) -> Response {
    relay(
        &bridge,
        &headers,
        "mpm.presence.register.start",
        json!({ "ticket": body.ticket }),
    )
    .await
}

/// `POST /api/console/presence/enrol/finish` → `mpm.presence.register.finish`.
///
/// Test: `start_and_finish_relay_to_the_daemon_8878`.
async fn finish(
    State(bridge): State<PresenceBridge>,
    headers: HeaderMap,
    Json(body): Json<FinishBody>,
) -> Response {
    let params = json!({ "ticket": body.ticket, "credential": body.credential });
    relay(&bridge, &headers, "mpm.presence.register.finish", params).await
}

/// Gate, then forward `params` to `method` and answer its result as JSON.
async fn relay(
    bridge: &PresenceBridge,
    headers: &HeaderMap,
    method: &str,
    params: Value,
) -> Response {
    if let Err(refusal) = gate(bridge, headers, true).await {
        return refusal.into_response();
    }
    match call(bridge, method, params).await {
        Ok(value) => Json(value).into_response(),
        Err(refusal) => refusal.into_response(),
    }
}

/// No caching, no framing, no referrer, on every enrol response.
async fn enrol_headers(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("frame-ancestors 'none'"),
    );
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    response
}

/// The enrol page and its two calls, over `bridge`.
///
/// Why/What: see the module doc.
/// Test: `presence_tests.rs`.
pub fn router(bridge: PresenceBridge) -> Router {
    Router::new()
        .route(ENROL_PAGE_PATH, get(page))
        .route(ENROL_START_PATH, post(start))
        .route(ENROL_FINISH_PATH, post(finish))
        .layer(axum::middleware::from_fn(enrol_headers))
        .with_state(bridge)
}

#[cfg(test)]
#[path = "presence_tests.rs"]
mod tests;
