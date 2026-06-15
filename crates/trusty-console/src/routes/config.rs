//! trusty-mpm config-convention HTTP routes — the Config tab front door (#1220).
//!
//! Why: #1220 adds a trusty-console Config UI for editing the
//! `~/.trusty-tools/trusty-mpm/config.yaml` convention (workspace-root template,
//! auto-resume default, default model). Per the #1104 principle, HTTP lives ONLY
//! in the console: these handlers expose `/api/console/config/mpm` as the operator
//! surface and drive the edit natively through the trusty-mpm `config_read` /
//! `config_write` MCP tools via the shared `McpServiceHandle` — never by touching
//! the config file from the console process or proxying to the daemon's HTTP port.
//! What: a read route (`get_handler` → `config_read`) and a write route
//! (`post_handler` → `config_write`) that map tool results to HTTP via the shared
//! [`crate::routes::sessions::map_tool_result`] taxonomy (200 / 503+hint / 502).
//! The write body's fields are all optional so a partial update leaves omitted
//! settings unchanged (the MCP tool merges).
//! Test: the `tests` module drives both handlers with an absent-binary handle
//! (CI has no trusty-mpm on PATH) and asserts neither ever 500s.

use axum::{extract::State, http::StatusCode, response::IntoResponse};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::routes::sessions::map_tool_result;
use crate::server::AppState;

/// Resolve the trusty-mpm MCP handle and call a tool, mapping the result.
///
/// Why: both config routes resolve the same handle then call one tool; collapsing
/// the two steps keeps each handler a single expression and reuses the session
/// routes' error taxonomy so config and session surfaces behave identically.
/// What: looks up the `"trusty-mpm"` handle (503 when unregistered), calls
/// `call_tool_checked`, and passes the result through [`map_tool_result`].
/// Test: exercised by both route tests below.
async fn call(state: &AppState, tool: &str, args: Value) -> axum::response::Response {
    let Some(handle) = state.mcp_handles().get("trusty-mpm").cloned() else {
        tracing::error!("config route: no MCP handle registered for trusty-mpm");
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    map_tool_result(handle.call_tool_checked(tool, args).await)
}

/// `GET /api/console/config/mpm` — read trusty-mpm config via `config_read`.
///
/// Why: the Config tab renders its form from the current settings (#1220).
/// What: calls `config_read` (no args); returns
/// `{ workspace_root_template, auto_resume, default_model, workspace_root }`.
/// Test: `config_get_absent_binary_does_not_500`.
pub async fn get_handler(State(state): State<AppState>) -> axum::response::Response {
    call(&state, "config_read", json!({})).await
}

/// Request body for the config-write route — all fields optional (partial update).
///
/// Why: the Config tab may save one field at a time; omitted fields must leave the
/// stored setting unchanged (the `config_write` MCP tool merges).
/// What: optional `workspace_root_template` / `default_model` strings and an
/// optional `auto_resume` boolean. `additionalProperties` are ignored by serde.
/// Test: `config_post_absent_binary_does_not_500`.
#[derive(Deserialize)]
pub struct ConfigBody {
    /// New workspace-root template (a leading `~` is expanded by the daemon).
    #[serde(default)]
    workspace_root_template: Option<String>,
    /// New supervisor auto-resume default.
    #[serde(default)]
    auto_resume: Option<bool>,
    /// New default model id or tier alias for launched sessions.
    #[serde(default)]
    default_model: Option<String>,
}

/// `POST /api/console/config/mpm` — persist config edits via `config_write`.
///
/// Why: the Config tab's save action durably records the operator's choices
/// (#1220) — the console's non-CLI control over the config convention.
/// What: forwards only the present body fields to `config_write` (omitted fields
/// are not sent, so the tool leaves them unchanged); returns the merged config.
/// Test: `config_post_absent_binary_does_not_500`.
pub async fn post_handler(
    State(state): State<AppState>,
    axum::Json(body): axum::Json<ConfigBody>,
) -> axum::response::Response {
    let mut args = serde_json::Map::new();
    if let Some(t) = body.workspace_root_template {
        args.insert("workspace_root_template".to_string(), json!(t));
    }
    if let Some(a) = body.auto_resume {
        args.insert("auto_resume".to_string(), json!(a));
    }
    if let Some(m) = body.default_model {
        args.insert("default_model".to_string(), json!(m));
    }
    call(&state, "config_write", Value::Object(args)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    use crate::server::{AppState, build_router};

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

    /// Why: the read route must degrade gracefully (never 500) when trusty-mpm is
    /// absent from PATH (the CI condition).
    /// Test: this test.
    #[tokio::test]
    async fn config_get_absent_binary_does_not_500() {
        assert_not_500("GET", "/api/console/config/mpm", Body::empty()).await;
    }

    /// Why: the write route must also degrade gracefully and must accept a partial
    /// body (only one field present).
    /// Test: this test.
    #[tokio::test]
    async fn config_post_absent_binary_does_not_500() {
        let body = Body::from(json!({ "auto_resume": true }).to_string());
        assert_not_500("POST", "/api/console/config/mpm", body).await;
    }
}
