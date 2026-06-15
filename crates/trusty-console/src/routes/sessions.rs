//! trusty-mpm session-manager HTTP routes — the single HTTP front door (#1222).
//!
//! Why: per the #1104 architecture principle, HTTP lives ONLY in trusty-console;
//! the session-manager daemon speaks stdio MCP. These handlers turn the
//! console's `/api/console/sessions/*` surface into the canonical operator
//! interface for the session REST API (P3), rendering fleet state and driving
//! lifecycle ops natively from the trusty-mpm MCP tools (P2) — never by proxying
//! to the daemon's HTTP port.
//! What: read routes (`list`, `get`, `activity`, `supervisor`) and write routes
//! (`new`, `stop`, `resume`, `decommission`, `auto_resume`) that each call a
//! trusty-mpm MCP tool through the shared `McpServiceHandle` via
//! `call_tool_checked` (capability-gated). [`map_tool_result`] converts a tool
//! result or `McpHandleError` into a clean HTTP response: 200 on success, 503
//! when the binary is absent / in backoff / degraded / the tool is missing
//! (with an actionable hint), 502 on any other transport error.
//! Test: the `tests` module drives each handler with an absent-binary handle
//! (CI has no trusty-mpm on PATH) and asserts no handler ever 500s.

use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::mcp_handle::{McpHandleError, McpServiceHandle};
use crate::server::AppState;

/// Default trailing pane lines for the activity route (parity with the MCP tool).
const DEFAULT_ACTIVITY_LINES: u32 = 60;

/// Resolve the trusty-mpm MCP handle from app state, or return a 503 response.
///
/// Why: every session route needs the mpm handle; when it is unregistered (it
/// should always be present, but be defensive) the route must degrade to 503
/// rather than panic.
/// What: looks up `"trusty-mpm"` in the handle map; `Some(handle)` or `None`
/// (the caller maps `None` to a 503). Returning `Option` rather than
/// `Result<_, Response>` avoids carrying a large axum `Response` in the error
/// variant (`clippy::result_large_err`).
/// Test: indirectly via the route tests (handle is always registered in tests).
fn mpm_handle(state: &AppState) -> Option<Arc<McpServiceHandle>> {
    let handle = state.mcp_handles().get("trusty-mpm").cloned();
    if handle.is_none() {
        tracing::error!("sessions route: no MCP handle registered for trusty-mpm");
    }
    handle
}

/// Map a capability-gated MCP tool call result into an HTTP response.
///
/// Why: all session routes share the same error taxonomy — a missing tool means
/// a stale daemon (503 + hint), an absent binary / backoff / degraded handle
/// means the service is not reachable (503), and any other error is a transport
/// failure (502). Centralising it keeps every handler a one-liner and the
/// behaviour uniform (mirrors the analyze routes in `server.rs`).
/// What: `Ok(val)` → 200 JSON; `ToolUnavailable` → 503 `{status, hint}`;
/// `Absent|Backoff|Degraded` → bare 503; `Other` → 502.
/// Test: `map_tool_result_*` unit tests below.
pub fn map_tool_result(result: Result<Value, McpHandleError>) -> axum::response::Response {
    match result {
        Ok(val) => axum::Json(val).into_response(),
        Err(McpHandleError::ToolUnavailable { tool, hint }) => {
            tracing::warn!(tool = %tool, hint = %hint, "sessions route: tool unavailable");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                axum::Json(json!({ "status": "degraded", "hint": hint })),
            )
                .into_response()
        }
        Err(
            McpHandleError::Absent
            | McpHandleError::Backoff { .. }
            | McpHandleError::Degraded { .. },
        ) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
        Err(e) => {
            tracing::warn!("sessions route error: {e:#}");
            StatusCode::BAD_GATEWAY.into_response()
        }
    }
}

/// Call a trusty-mpm tool through the handle and map the result to a response.
///
/// Why: every handler resolves the handle then calls one tool; this collapses
/// both steps so each route body is a single expression.
/// What: resolves the handle (503 on absence), calls `call_tool_checked`, and
/// passes the result through [`map_tool_result`].
/// Test: exercised by every route test below.
async fn call(state: &AppState, tool: &str, args: Value) -> axum::response::Response {
    let Some(handle) = mpm_handle(state) else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    map_tool_result(handle.call_tool_checked(tool, args).await)
}

// ─── read routes ──────────────────────────────────────────────────────────────

/// `GET /api/console/sessions` — list the managed-session fleet via `session_list`.
///
/// Why: the Sessions tab renders the fleet from this; native MCP, not a proxy.
/// What: calls `session_list` (no args) and returns the JSON array.
/// Test: `list_absent_binary_does_not_500`.
pub async fn list_handler(State(state): State<AppState>) -> axum::response::Response {
    call(&state, "session_list", json!({})).await
}

/// `GET /api/console/sessions/{id}` — detailed status via `session_status`.
///
/// Why: the Sessions tab's per-session detail view needs the full record.
/// What: calls `session_status` with the path `session_id`.
/// Test: `get_absent_binary_does_not_500`.
pub async fn get_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> axum::response::Response {
    call(&state, "session_status", json!({ "session_id": id })).await
}

/// Query params for the activity route.
#[derive(Deserialize)]
pub struct ActivityQuery {
    /// Optional trailing-line count (defaults to 60, matching the MCP tool).
    lines: Option<u32>,
}

/// `GET /api/console/sessions/{id}/activity` — recent pane via `session_activity`.
///
/// Why: the activity panel shows the last N pane lines so an operator can watch
/// an actively-failing/auto-resuming session at the configured poll cadence.
/// What: calls `session_activity` with `session_id` + `lines` (capped default).
/// Test: `activity_absent_binary_does_not_500`.
pub async fn activity_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(params): Query<ActivityQuery>,
) -> axum::response::Response {
    let lines = params.lines.unwrap_or(DEFAULT_ACTIVITY_LINES);
    call(
        &state,
        "session_activity",
        json!({ "session_id": id, "lines": lines }),
    )
    .await
}

/// `GET /api/console/sessions/supervisor` — fleet + auto-resume via `supervisor_status`.
///
/// Why: the supervisor widget needs fleet counts and the auto-resume control
/// state in one call (RFC §4 P3).
/// What: calls `supervisor_status` (no args); returns `{ fleet, auto_resume }`.
/// Test: `supervisor_absent_binary_does_not_500`.
pub async fn supervisor_handler(State(state): State<AppState>) -> axum::response::Response {
    call(&state, "supervisor_status", json!({})).await
}

// ─── write routes ─────────────────────────────────────────────────────────────

/// Body for the spawn route — mirrors the `session_new` MCP tool arguments.
#[derive(Deserialize)]
pub struct NewSessionBody {
    repo_url: String,
    #[serde(rename = "ref")]
    git_ref: String,
    task: String,
    #[serde(default)]
    name_hint: Option<String>,
    #[serde(default)]
    runtime: Option<String>,
}

/// `POST /api/console/sessions` — spawn a new session via `session_new`.
///
/// Why: the Sessions tab's "spawn" control creates a managed session end-to-end
/// through the console → MCP bridge → daemon (no direct daemon HTTP).
/// What: forwards the body fields to `session_new`; required fields are enforced
/// by serde (a missing field yields a 422 from axum's JSON extractor).
/// Test: `new_absent_binary_does_not_500`.
pub async fn new_handler(
    State(state): State<AppState>,
    axum::Json(body): axum::Json<NewSessionBody>,
) -> axum::response::Response {
    let mut args = json!({
        "repo_url": body.repo_url,
        "ref": body.git_ref,
        "task": body.task,
    });
    if let Some(obj) = args.as_object_mut() {
        if let Some(hint) = body.name_hint {
            obj.insert("name_hint".to_string(), json!(hint));
        }
        if let Some(rt) = body.runtime {
            obj.insert("runtime".to_string(), json!(rt));
        }
    }
    call(&state, "session_new", args).await
}

/// `POST /api/console/sessions/{id}/stop` — stop a session via `session_stop`.
///
/// Why: the per-session Stop control.
/// What: calls `session_stop` with the path id.
/// Test: `stop_absent_binary_does_not_500`.
pub async fn stop_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> axum::response::Response {
    call(&state, "session_stop", json!({ "session_id": id })).await
}

/// `POST /api/console/sessions/{id}/resume` — resume via `session_resume`.
///
/// Why: the per-session Resume control.
/// What: calls `session_resume` with the path id.
/// Test: `resume_absent_binary_does_not_500`.
pub async fn resume_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> axum::response::Response {
    call(&state, "session_resume", json!({ "session_id": id })).await
}

/// `DELETE /api/console/sessions/{id}` — full teardown via `session_decommission`.
///
/// Why: the per-session Decommission control (terminal — removes the workspace).
/// What: calls `session_decommission` with the path id.
/// Test: `decommission_absent_binary_does_not_500`.
pub async fn decommission_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> axum::response::Response {
    call(&state, "session_decommission", json!({ "session_id": id })).await
}

/// Body for the auto-resume toggle.
#[derive(Deserialize)]
pub struct AutoResumeBody {
    enabled: bool,
}

/// `POST /api/console/sessions/supervisor/auto-resume` — toggle auto-resume.
///
/// Why: the console SHALL provide controls to enable/disable auto-resume
/// (RFC §6 Q6) — not CLI-only. This persists the operator's desired flag.
/// What: calls `auto_resume_set` with `{ enabled }`; returns the resulting
/// control state (`desired`, `env`, `pending_restart`).
/// Test: `auto_resume_absent_binary_does_not_500`.
pub async fn auto_resume_handler(
    State(state): State<AppState>,
    axum::Json(body): axum::Json<AutoResumeBody>,
) -> axum::response::Response {
    call(
        &state,
        "auto_resume_set",
        json!({ "enabled": body.enabled }),
    )
    .await
}

// ─── tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use crate::server::build_router;

    /// Build a router whose AppState has the trusty-mpm handle registered but no
    /// binary on PATH (CI), so every session route should degrade to 503/502 and
    /// never 500.
    fn router() -> axum::Router {
        build_router(AppState::new(vec![]))
    }

    async fn assert_not_500(method: &str, uri: &str, body: Body) {
        let req = Request::builder()
            .method(method)
            .uri(uri)
            .header("content-type", "application/json")
            .body(body)
            .expect("request");
        let resp = router().oneshot(req).await.expect("response");
        assert_ne!(
            resp.status(),
            StatusCode::INTERNAL_SERVER_ERROR,
            "{method} {uri} must not 500 when binary absent (got {})",
            resp.status()
        );
    }

    #[tokio::test]
    async fn list_absent_binary_does_not_500() {
        assert_not_500("GET", "/api/console/sessions", Body::empty()).await;
    }

    #[tokio::test]
    async fn get_absent_binary_does_not_500() {
        assert_not_500("GET", "/api/console/sessions/abc", Body::empty()).await;
    }

    #[tokio::test]
    async fn activity_absent_binary_does_not_500() {
        assert_not_500(
            "GET",
            "/api/console/sessions/abc/activity?lines=20",
            Body::empty(),
        )
        .await;
    }

    #[tokio::test]
    async fn supervisor_absent_binary_does_not_500() {
        assert_not_500("GET", "/api/console/sessions/supervisor", Body::empty()).await;
    }

    #[tokio::test]
    async fn new_absent_binary_does_not_500() {
        let body = Body::from(
            json!({ "repo_url": "https://x/y", "ref": "main", "task": "t" }).to_string(),
        );
        assert_not_500("POST", "/api/console/sessions", body).await;
    }

    #[tokio::test]
    async fn stop_absent_binary_does_not_500() {
        assert_not_500("POST", "/api/console/sessions/abc/stop", Body::empty()).await;
    }

    #[tokio::test]
    async fn resume_absent_binary_does_not_500() {
        assert_not_500("POST", "/api/console/sessions/abc/resume", Body::empty()).await;
    }

    #[tokio::test]
    async fn decommission_absent_binary_does_not_500() {
        assert_not_500("DELETE", "/api/console/sessions/abc", Body::empty()).await;
    }

    #[tokio::test]
    async fn auto_resume_absent_binary_does_not_500() {
        let body = Body::from(json!({ "enabled": true }).to_string());
        assert_not_500("POST", "/api/console/sessions/supervisor/auto-resume", body).await;
    }

    /// Why: the shared mapper must convert a missing-tool error into a clean 503
    /// with a hint, never a 502 — the regression class from #1170.
    /// Test: this test.
    #[tokio::test]
    async fn map_tool_result_tool_unavailable_is_503_with_hint() {
        let resp = map_tool_result(Err(McpHandleError::ToolUnavailable {
            tool: "session_list".to_string(),
            hint: "upgrade trusty-mpm".to_string(),
        }));
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    /// Why: an absent binary must be a bare 503 (service not reachable).
    /// Test: this test.
    #[tokio::test]
    async fn map_tool_result_absent_is_503() {
        let resp = map_tool_result(Err(McpHandleError::Absent));
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    // ── route-shadowing verification (#1222 review finding #2) ────────────────
    //
    // The static `/api/console/sessions/supervisor` route shares a prefix with the
    // dynamic `/api/console/sessions/{id}` capture. axum 0.8 (matchit 0.8)
    // prioritises a literal/static segment over a `{param}` capture, so the static
    // routes SHOULD win — these tests prove it rather than trusting the router
    // ordering. They do so by priming the trusty-mpm handle to a `Connected` state
    // whose tool set deliberately excludes every session tool, so each session
    // route returns `503 { status: degraded, hint }` where the hint names the tool
    // the matched handler asked for. The tool name in the hint reveals which
    // handler axum dispatched to:
    //   - `supervisor` route → `supervisor_handler` → hint names `supervisor_status`
    //   - shadowed by `{id}`  → `get_handler`        → hint names `session_status`

    /// Build a router whose trusty-mpm handle is `Connected` but exposes no
    /// session tools, so each route's 503 hint reveals the dispatched handler.
    async fn router_primed_missing_tools() -> axum::Router {
        let state = AppState::new(vec![]);
        {
            let handles = state.mcp_handles();
            let mpm = handles.get("trusty-mpm").expect("mpm handle registered");
            // Any argument primes a Connected state whose tool set is
            // {console_metrics, …analyze tools} — none of the session tools — so
            // every session route trips the capability-gate with a tool-named hint.
            mpm.prime_connected_missing_tool_for_test("supervisor_status")
                .await;
        }
        build_router(state)
    }

    async fn hint_of(resp: axum::http::Response<Body>) -> String {
        let bytes = resp
            .into_body()
            .collect()
            .await
            .expect("collect body")
            .to_bytes()
            .to_vec();
        let body: serde_json::Value = serde_json::from_slice(&bytes).expect("parse json");
        body["hint"].as_str().unwrap_or("").to_string()
    }

    /// Why: prove `GET /api/console/sessions/supervisor` reaches
    /// `supervisor_handler` (calls `supervisor_status`) and is NOT shadowed by the
    /// `{id}` capture (which would call `session_status` with id="supervisor").
    /// Test: this test — the discriminator is the tool name in the 503 hint.
    #[tokio::test]
    async fn supervisor_route_is_not_shadowed_by_id_capture() {
        let router = router_primed_missing_tools().await;
        let req = Request::builder()
            .uri("/api/console/sessions/supervisor")
            .body(Body::empty())
            .expect("request");
        let resp = router.oneshot(req).await.expect("response");
        assert_eq!(
            resp.status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "primed-missing-tool supervisor route must be a capability-gated 503"
        );
        let hint = hint_of(resp).await;
        assert!(
            hint.contains("supervisor_status"),
            "supervisor route must reach supervisor_handler (hint should name \
             supervisor_status); got: {hint}"
        );
        assert!(
            !hint.contains("session_status"),
            "supervisor route must NOT be shadowed by the {{id}} capture \
             (session_status); got: {hint}"
        );
    }

    /// Why: prove `POST /api/console/sessions/supervisor/auto-resume` reaches
    /// `auto_resume_handler` (calls `auto_resume_set`), not any `{id}` capture.
    /// Test: this test — the 503 hint must name `auto_resume_set`.
    #[tokio::test]
    async fn auto_resume_route_is_not_shadowed() {
        let router = router_primed_missing_tools().await;
        let body = Body::from(json!({ "enabled": true }).to_string());
        let req = Request::builder()
            .method("POST")
            .uri("/api/console/sessions/supervisor/auto-resume")
            .header("content-type", "application/json")
            .body(body)
            .expect("request");
        let resp = router.oneshot(req).await.expect("response");
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        let hint = hint_of(resp).await;
        assert!(
            hint.contains("auto_resume_set"),
            "auto-resume route must reach auto_resume_handler (hint should name \
             auto_resume_set); got: {hint}"
        );
    }

    /// Why: the sanity counterpart — a genuine id capture (`/{id}`) must reach
    /// `get_handler` (calls `session_status`), confirming the discriminator works
    /// and the `{id}` route is still wired for non-`supervisor` ids.
    /// Test: this test.
    #[tokio::test]
    async fn ordinary_id_route_reaches_session_status() {
        let router = router_primed_missing_tools().await;
        let req = Request::builder()
            .uri("/api/console/sessions/sess-abc123")
            .body(Body::empty())
            .expect("request");
        let resp = router.oneshot(req).await.expect("response");
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        let hint = hint_of(resp).await;
        assert!(
            hint.contains("session_status"),
            "ordinary id route must reach get_handler (session_status); got: {hint}"
        );
    }
}
