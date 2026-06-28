//! Activity-inspection route for managed sessions.
//!
//! Why: extracted from `managed_routes/mod.rs` to keep that file under the 500-SLOC
//! production cap while the route stays cohesive in its own submodule.
//! What: defines [`ActivityResponse`] and the [`get_session_activity`] handler
//! (`GET /api/v1/sessions/managed/{id}/activity`).
//! Test: `activity_no_key_returns_raw_pane` in tests/session_manager_mvp.rs;
//! `handler_activity_cache_hit`.

use std::sync::Arc;

use axum::{
    Json,
    extract::{Path as AxumPath, State},
    http::StatusCode,
    response::IntoResponse,
};
use serde::Serialize;
use tracing::warn;

use crate::daemon::state::DaemonState;

use super::parse_id;

/// Response body for GET /api/v1/sessions/managed/{id}/activity.
///
/// Why: the calling agentic process needs the full activity picture without
/// requiring an LLM key — raw pane content and structured lifecycle fields are
/// always available; the LLM classification is an optional overlay when
/// OpenRouter is configured. This lets the calling agentic process do its own
/// inference over the raw pane content.
/// What: always-present fields: `raw_pane` (last 60 lines), `runtime_active`
/// (tmux session alive or not), `pending_decision`, `proposed_default`. LLM
/// fields (`state`, `summary`, `confidence`, `classification`) are populated
/// when the classifier ran successfully; `classification` is `null` when the
/// key is absent or the classifier was not invoked.
/// Test: activity route handler test; `activity_no_key_returns_raw_pane` test.
#[derive(Debug, Serialize)]
pub struct ActivityResponse {
    /// Raw pane content (last 60 lines). Always present so the calling agentic
    /// process can reason over the raw terminal output directly.
    pub raw_pane: String,
    /// Whether the tmux runtime session is currently alive.
    pub runtime_active: bool,
    /// Activity state from LLM classification: working, idle, blocked_on_permission,
    /// errored, done, unknown. Populated from classifier verdict or "unknown".
    pub state: String,
    /// Human-readable summary of what the session is doing (from LLM or fallback).
    pub summary: String,
    /// Confidence of the classification (0.0–1.0). 0.0 when no classifier ran.
    pub confidence: f32,
    /// True when the verdict was served from the content-hash cache.
    pub cache_hit: bool,
    /// Input token count for this check (0 on cache hit or no classifier).
    pub input_tokens: u32,
    /// Output token count for this check (0 on cache hit or no classifier).
    pub output_tokens: u32,
    /// Latency in milliseconds for this check.
    pub latency_ms: u64,
    /// Cumulative input tokens across all checks for this session.
    pub total_input_tokens: u64,
    /// Cumulative output tokens across all checks for this session.
    pub total_output_tokens: u64,
    /// LLM classification result. `null` when no OpenRouter key or classifier
    /// not configured; string state name when classifier ran.
    pub classification: Option<String>,
    /// A pending decision question, if surfaced by a previous activity check.
    pub pending_decision: Option<String>,
    /// Proposed default answer to the pending decision.
    pub proposed_default: Option<String>,
}

/// GET /api/v1/sessions/managed/{id}/activity — inspect session activity.
///
/// Why: the calling agentic process needs to know whether the session is
/// working, idle, blocked, errored, or done WITHOUT requiring an LLM key.
/// The raw pane content is ALWAYS returned so the calling agentic process can
/// perform its own inference. The OpenRouter LLM classifier is invoked ONLY
/// when configured (i.e. when OPENROUTER_API_KEY is set).
/// What: captures the pane via the session's tmux driver (last 60 lines);
/// determines `runtime_active` from tmux presence; calls `ActivityMonitor::check`
/// — which already converts `MissingApiKey` to an Unknown verdict non-erroring —
/// and returns the verdict alongside `raw_pane` and `classification` (null when
/// no classifier ran or key absent).
/// The hash-skip cache and cost instrumentation remain active for the
/// optional-classifier path.
/// Test: `activity_no_key_returns_raw_pane` in tests/session_manager_mvp.rs;
/// `handler_activity_cache_hit`.
pub async fn get_session_activity(
    State(state): State<Arc<DaemonState>>,
    AxumPath(id_str): AxumPath<String>,
) -> impl IntoResponse {
    let id = match parse_id(&id_str) {
        Ok(id) => id,
        Err((code, msg)) => return (code, msg).into_response(),
    };
    let mgr = state.session_manager().await;
    let record = match mgr.get(&id).await {
        Ok(r) => r,
        Err(_) => {
            return (StatusCode::NOT_FOUND, format!("session {id_str} not found")).into_response();
        }
    };

    let pane_text = mgr
        .capture_pane(&id, 60)
        .await
        .unwrap_or_else(|_| String::new());

    let runtime_active = mgr.tmux_driver().session_exists(&record.tmux_name);

    let monitor = state.activity_monitor();
    let result = match monitor.check(&id_str, &pane_text).await {
        Ok(r) => r,
        Err(e) => {
            warn!(session = %id_str, "activity check error (non-key): {e}");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("activity check failed: {e}"),
            )
                .into_response();
        }
    };

    let api_key_present = std::env::var("OPENROUTER_API_KEY").is_ok();
    let classification = if api_key_present {
        Some(format!("{:?}", result.verdict.state).to_lowercase())
    } else {
        None
    };

    Json(ActivityResponse {
        raw_pane: pane_text,
        runtime_active,
        state: format!("{:?}", result.verdict.state).to_lowercase(),
        summary: result.verdict.summary,
        confidence: result.verdict.confidence,
        cache_hit: result.cache_hit,
        input_tokens: result.cost.input_tokens,
        output_tokens: result.cost.output_tokens,
        latency_ms: result.cost.latency_ms,
        total_input_tokens: result.tally.total_input_tokens,
        total_output_tokens: result.tally.total_output_tokens,
        classification,
        pending_decision: record.pending_decision,
        proposed_default: record.proposed_default,
    })
    .into_response()
}
