//! Server-side request deadline for every daemon HTTP route (#8476).
//!
//! Why: no route carried a server-side bound, so a stalled handler (the
//! merged-PR worktree survey under host load, #7884) held its connection and
//! task open until the CALLER gave up. Behind trusty-console that surfaced as
//! the proxy's opaque 30 s `502` instead of a daemon-side error, and a caller
//! with no client timeout of its own waited forever.
//! What: `enforce` is an axum `from_fn` middleware layered router-wide in
//! `super::router`. It races the handler future against the deadline
//! `deadline_for` picks from the matched route template. On expiry it drops
//! the handler future and answers `504 Gateway Timeout` with a JSON `error`
//! body. Every route gets `STANDARD_DEADLINE` unless `LONG_ROUTES` names
//! it with a longer class, each class sized just under the client bound its
//! callers already use so the caller reads the daemon's error, not its own
//! transport timeout.
//!
//! The deadline bounds the handler future only — the time to produce the
//! response head. A streamed body is not bounded, so the SSE routes
//! (`GET /events`, `GET /sessions/{id}/events`,
//! `GET /api/v1/bus/subscribe/{instance_id}`) keep the standard deadline: each
//! returns its `Sse` head at once and then streams indefinitely.
//!
//! Limits: dropping the future cancels its async work, but work already handed
//! to `spawn_blocking` or a child process runs on to that work's own ceiling
//! (e.g. `GIT_CALL_TIMEOUT`), and a handler that blocks its worker thread
//! synchronously cannot be preempted until it yields.
//! Test: `stalled_route_answers_504_at_the_server_deadline`,
//! `sse_stream_outlives_the_request_deadline` (the real router, in
//! `api_tests.rs`); the 504 mapping, dropped future, table and route
//! registration in `request_deadline_tests.rs`.

use std::time::Duration;

use axum::Json;
use axum::extract::{MatchedPath, Request};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use super::coordinator_routes::COORDINATOR_CHAT_PATH;

/// Deadline for every route [`LONG_ROUTES`] does not name.
///
/// Why: 2.5x the CLI's 10 s default client bound, and under trusty-console's
/// 30 s proxy bound, so a proxied stall reaches the caller as this daemon's
/// `504` rather than the console's `502`.
pub(crate) const STANDARD_DEADLINE: Duration = Duration::from_secs(25);

/// Routes that run git subprocesses, stop or start a harness process, or call
/// GitHub.
///
/// Why: one git call may legitimately run to `GIT_CALL_TIMEOUT` (60 s) and a
/// `git worktree remove` to 30 s; the standard deadline would cut them short.
pub(crate) const LIFECYCLE_DEADLINE: Duration = Duration::from_secs(90);

/// `GET /api/v1/doctor`: 5 s under the client's 120 s `DOCTOR_REQUEST_TIMEOUT`.
pub(crate) const DOCTOR_DEADLINE: Duration = Duration::from_secs(115);

/// LLM-backed routes: above the 120 s upstream OpenRouter bound, 5 s under the
/// client's 130 s `CHAT_REQUEST_TIMEOUT`.
pub(crate) const LLM_DEADLINE: Duration = Duration::from_secs(125);

/// Synchronous provisioning (clone + worktree + deploy + spawn): 5 s under the
/// client's 180 s `PROVISION_REQUEST_TIMEOUT`.
pub(crate) const PROVISION_DEADLINE: Duration = Duration::from_secs(175);

/// The whole-store worktree survey: 5 s under the client's 1800 s
/// `RECLAIM_SURVEY_REQUEST_TIMEOUT`, which is ~3x the measured worst case.
pub(crate) const SURVEY_DEADLINE: Duration = Duration::from_secs(1795);

/// One route that needs more than [`STANDARD_DEADLINE`]: HTTP method, matched
/// route template, deadline.
pub(crate) type LongRoute = (&'static str, &'static str, Duration);

/// Every route with a deadline longer than [`STANDARD_DEADLINE`].
///
/// Why: a too-short deadline on a legitimately long call is itself a
/// regression, so each exemption is listed with its reason.
/// What: keyed by method AND template — `GET …/managed/{id}` is a cheap read
/// while `DELETE …/managed/{id}` stops a harness.
/// Test: `every_long_route_is_a_registered_route` proves each row names a real
/// route and method; `long_routes_resolve_to_their_class_deadline`.
pub(crate) const LONG_ROUTES: &[LongRoute] = &[
    // Whole-store worktree survey: classifies and byte-walks every registered
    // worktree (#5830 measured ~92 s to classify, >600 s to walk 46).
    (
        "POST",
        "/api/v1/sessions/managed/prune-worktrees",
        SURVEY_DEADLINE,
    ),
    (
        "GET",
        "/api/v1/sessions/managed/reconcile-worktrees",
        SURVEY_DEADLINE,
    ),
    // Synchronous provisioning, or a dispatcher that can reach it: `/rpc`
    // carries the `session_new` MCP tool, and its other long tool, the disk
    // survey, is clamped at 55 s.
    ("POST", "/api/v1/sessions/managed", PROVISION_DEADLINE),
    ("POST", "/api/v1/control/sessions/run", PROVISION_DEADLINE),
    ("POST", "/api/v1/manager/act", PROVISION_DEADLINE),
    ("POST", "/rpc", PROVISION_DEADLINE),
    // LLM round trips.
    ("POST", "/llm/chat", LLM_DEADLINE),
    ("POST", COORDINATOR_CHAT_PATH, LLM_DEADLINE),
    ("POST", "/api/v1/session-manager/chat", LLM_DEADLINE),
    ("POST", "/api/v1/manager/chat", LLM_DEADLINE),
    ("GET", "/api/v1/manager/digest", LLM_DEADLINE),
    ("POST", "/api/v1/manager/route-task", LLM_DEADLINE),
    ("POST", "/api/v1/sessions/proxy/message", LLM_DEADLINE),
    (
        "GET",
        "/api/v1/sessions/proxy/summary/{conversation_key}",
        LLM_DEADLINE,
    ),
    // The whole doctor battery runs inside the request (#5111).
    ("GET", "/api/v1/doctor", DOCTOR_DEADLINE),
    // Session lifecycle: git worktree work, harness stop/start, tmux adoption.
    (
        "DELETE",
        "/api/v1/sessions/managed/{id}",
        LIFECYCLE_DEADLINE,
    ),
    (
        "POST",
        "/api/v1/sessions/managed/{id}/runtime-stop",
        LIFECYCLE_DEADLINE,
    ),
    (
        "POST",
        "/api/v1/sessions/managed/{id}/resume",
        LIFECYCLE_DEADLINE,
    ),
    (
        "POST",
        "/api/v1/sessions/managed/{id}/reactivate",
        LIFECYCLE_DEADLINE,
    ),
    (
        "POST",
        "/api/v1/sessions/managed/{id}/decommission",
        LIFECYCLE_DEADLINE,
    ),
    (
        "POST",
        "/api/v1/sessions/managed/{id}/delete",
        LIFECYCLE_DEADLINE,
    ),
    (
        "POST",
        "/api/v1/sessions/managed/{id}/sync-assets",
        LIFECYCLE_DEADLINE,
    ),
    (
        "POST",
        "/api/v1/sessions/managed/sync-assets",
        LIFECYCLE_DEADLINE,
    ),
    ("POST", "/api/v1/sessions/managed/adopt", LIFECYCLE_DEADLINE),
    (
        "POST",
        "/api/v1/sessions/managed/adopt-worktree",
        LIFECYCLE_DEADLINE,
    ),
    ("POST", "/api/v1/sessions/managed/prune", LIFECYCLE_DEADLINE),
    (
        "POST",
        "/api/v1/sessions/managed/decommission-ephemeral",
        LIFECYCLE_DEADLINE,
    ),
    (
        "POST",
        "/api/v1/control/sessions/{id}/connect",
        LIFECYCLE_DEADLINE,
    ),
    (
        "POST",
        "/api/v1/control/sessions/{id}/stop",
        LIFECYCLE_DEADLINE,
    ),
    ("DELETE", "/sessions/{id}", LIFECYCLE_DEADLINE),
    ("DELETE", "/sessions/dead", LIFECYCLE_DEADLINE),
    ("POST", "/sessions/discover", LIFECYCLE_DEADLINE),
    ("POST", "/sessions/{id}/pause", LIFECYCLE_DEADLINE),
    ("POST", "/sessions/{id}/resume", LIFECYCLE_DEADLINE),
    ("POST", "/tmux/adopt", LIFECYCLE_DEADLINE),
    // Config writes that restart Claude Code or redeploy a profile.
    ("POST", "/claude-config/apply", LIFECYCLE_DEADLINE),
    ("POST", "/claude-config/restart", LIFECYCLE_DEADLINE),
    ("POST", "/claude-config/deploy", LIFECYCLE_DEADLINE),
    ("POST", "/claude-config/restore", LIFECYCLE_DEADLINE),
    // GitHub API round trips.
    ("POST", "/api/v1/report-bug", LIFECYCLE_DEADLINE),
    // Delegation repair touches the recorded worktree.
    (
        "POST",
        "/api/v1/delegations/{agent_id}/repair",
        LIFECYCLE_DEADLINE,
    ),
    (
        "POST",
        "/api/v1/delegations/by-id/{delegation_id}/repair",
        LIFECYCLE_DEADLINE,
    ),
    // Filesystem walk for project discovery, and registration that may probe
    // the repository.
    ("GET", "/projects/discover", LIFECYCLE_DEADLINE),
    ("POST", "/api/v1/projects", LIFECYCLE_DEADLINE),
];

/// The server-side deadline for one request.
///
/// What: the [`LONG_ROUTES`] deadline whose method and template both match,
/// else [`STANDARD_DEADLINE`] — including for an unmatched path (the 404
/// fallback), which carries no template.
/// Test: `long_routes_resolve_to_their_class_deadline`.
pub(crate) fn deadline_for(method: &str, template: Option<&str>) -> Duration {
    let Some(template) = template else {
        return STANDARD_DEADLINE;
    };
    LONG_ROUTES
        .iter()
        .find(|(m, t, _)| *m == method && *t == template)
        .map_or(STANDARD_DEADLINE, |(_, _, deadline)| *deadline)
}

/// Race the handler against its deadline (#8476).
///
/// Why: see the module doc.
/// What: reads the method and `MatchedPath` (set by the router before a
/// `Router::layer` middleware runs), awaits `next.run(req)` under
/// `tokio::time::timeout`, and on expiry drops the handler future and returns
/// [`deadline_exceeded`].
/// Test: `deadline_timeout_maps_to_504_with_json_error`,
/// `timed_out_handler_future_is_dropped`, `fast_route_passes_through_untouched`.
pub(crate) async fn enforce(req: Request, next: Next) -> Response {
    let method = req.method().clone();
    // `MatchedPath` is an `Arc<str>`; only an unmatched path pays an allocation.
    let template = req.extensions().get::<MatchedPath>().cloned();
    let unmatched = template.is_none().then(|| req.uri().path().to_owned());
    let deadline = deadline_for(method.as_str(), template.as_ref().map(MatchedPath::as_str));
    match tokio::time::timeout(deadline, next.run(req)).await {
        Ok(response) => response,
        Err(_elapsed) => {
            let route = template.as_ref().map_or(
                unmatched.as_deref().unwrap_or_default(),
                MatchedPath::as_str,
            );
            deadline_exceeded(method.as_str(), route, deadline)
        }
    }
}

/// The `504` a timed-out request receives.
///
/// Why: `504` names a server-side deadline and, unlike `408`, is not a status
/// HTTP clients retry on their own — a retried `POST` could provision twice.
/// What: `504 Gateway Timeout` with `{ "error", "route", "deadline_secs" }`,
/// matching the `{ "error": … }` shape `DaemonError` responses carry, and a
/// `warn!` naming the route.
/// Test: `deadline_timeout_maps_to_504_with_json_error`.
fn deadline_exceeded(method: &str, route: &str, deadline: Duration) -> Response {
    let secs = deadline.as_secs();
    tracing::warn!(
        method,
        route,
        deadline_secs = secs,
        "request exceeded its server-side deadline (#8476)"
    );
    let body = serde_json::json!({
        "error": format!(
            "{method} {route} exceeded the daemon's {secs} s server-side deadline; \
             the request was abandoned (#8476)"
        ),
        "route": route,
        "deadline_secs": secs,
    });
    (StatusCode::GATEWAY_TIMEOUT, Json(body)).into_response()
}

#[cfg(test)]
#[path = "request_deadline_tests.rs"]
mod tests;
