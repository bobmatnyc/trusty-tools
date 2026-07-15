//! `GET /api/v1/manager/digest` — LLM-authored portfolio narrative (WI-3, #2580).
//!
//! Why: DOC-36 §3.2 gives `tm manager` a digest endpoint that turns the
//! deterministic `/manager/status` rollup into a concise prose narrative — "what's
//! going on right now" — via the unified `trusty_common::inference` adapter (§3.3).
//! Two invariants make it safe and honest: (1) the LLM is fed the SAME
//! deterministic snapshot `aggregate_portfolio_status` produces (reused, never
//! re-derived — DOC-36 §5), and the response returns those deterministic totals
//! alongside the prose so a consumer NEVER depends on the model for numbers; (2)
//! when no provider is configured — or the single call fails — it degrades to a
//! clearly-marked deterministic templated narrative and a typed 503/502, never a
//! panic (DOC-16 D1 "one LLM call per operation, deterministic fallback on
//! failure"; §4 degrade bar). It never mutates a record (§2.1 read-only boundary).
//! What: [`DigestScope`]/[`parse_scope`], the [`deterministic_narrative`] fallback
//! templater, the LLM [`build_digest_messages`] prompt, [`DigestResponse`], and the
//! [`manager_digest_route`] handler.
//! Test: `parse_scope_variants`, `deterministic_narrative_marks_fallback`,
//! `build_digest_messages_grounds_on_snapshot` in `digest_tests.rs`; HTTP happy /
//! degrade / project-scope coverage in `tests/manager_inference.rs`.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde::{Deserialize, Serialize};
use trusty_common::inference::{ChatMessage, ChatRequest};

use super::inference::{MANAGER_MAX_TOKENS, MANAGER_TEMPERATURE};
use super::status::{PortfolioStatusResponse, load_portfolio_status, rollup_of};
use crate::daemon::state::DaemonState;

/// `generated_by` marker for a genuine LLM-authored narrative.
const GENERATED_BY_LLM: &str = "llm";
/// `generated_by` marker for the deterministic templated fallback.
const GENERATED_BY_FALLBACK: &str = "deterministic_fallback";

/// Query parameters for `GET /api/v1/manager/digest`.
///
/// Why: DOC-36 §3.2 scopes the digest to `portfolio` or `project:<name>`; a typed
/// query keeps the parse in one place.
/// What: the optional raw `scope` string, parsed by [`parse_scope`].
/// Test: `parse_scope_variants`.
#[derive(Debug, Deserialize)]
pub struct DigestQuery {
    /// Raw scope selector (`portfolio` | `project:<name>`); defaults to portfolio.
    pub scope: Option<String>,
}

/// The resolved digest scope.
///
/// Why: distinguishes the whole-portfolio narrative from a single-project one so
/// the handler can 404 an unknown project and narrow the snapshot accordingly.
/// What: [`Self::Portfolio`] or [`Self::Project`] carrying the project name.
/// Test: `parse_scope_variants`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DigestScope {
    /// The whole portfolio.
    Portfolio,
    /// One named project.
    Project(String),
}

impl DigestScope {
    /// A stable label for the response `scope` field.
    fn label(&self) -> String {
        match self {
            Self::Portfolio => "portfolio".to_string(),
            Self::Project(name) => format!("project:{name}"),
        }
    }
}

/// Parse the raw `scope` query value into a [`DigestScope`].
///
/// Why: the endpoint accepts exactly `portfolio` (or absent) and `project:<name>`;
/// anything else is a client error, surfaced as a 400 rather than a silent
/// portfolio fallback.
/// What: `None`/empty/`"portfolio"` → [`DigestScope::Portfolio`]; `"project:<name>"`
/// with a non-empty name → [`DigestScope::Project`]; otherwise `Err(message)`.
/// Test: `parse_scope_variants`.
pub fn parse_scope(raw: Option<&str>) -> Result<DigestScope, String> {
    match raw.map(str::trim) {
        None | Some("") | Some("portfolio") => Ok(DigestScope::Portfolio),
        Some(other) => {
            if let Some(name) = other.strip_prefix("project:") {
                let name = name.trim();
                if name.is_empty() {
                    return Err("scope 'project:' requires a project name".to_string());
                }
                Ok(DigestScope::Project(name.to_string()))
            } else {
                Err(format!(
                    "invalid scope '{other}'; expected 'portfolio' or 'project:<name>'"
                ))
            }
        }
    }
}

/// Response body for `GET /api/v1/manager/digest`.
///
/// Why: DOC-36 §3.2 + #2580 require the response to carry BOTH the narrative and
/// the deterministic totals it was derived from, and to mark clearly whether the
/// prose is LLM-authored or the deterministic fallback. Consumers read `status`
/// for authoritative numbers and never depend on the model for counts.
/// What: the resolved `scope`, a `generated_by` marker, the optional `model` (LLM
/// path only), the `narrative` prose, the deterministic `status` snapshot, and —
/// on degrade — a typed `error` code + actionable `message`.
/// Test: HTTP coverage in `tests/manager_inference.rs`.
#[derive(Debug, Serialize)]
pub struct DigestResponse {
    /// The resolved scope label (`portfolio` | `project:<name>`).
    pub scope: String,
    /// `"llm"` when model-authored, `"deterministic_fallback"` otherwise.
    pub generated_by: &'static str,
    /// The model slug that authored the narrative (LLM path only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// The narrative prose (LLM-authored or the deterministic templated fallback).
    pub narrative: String,
    /// The deterministic rollup the narrative was derived from — authoritative
    /// numbers, independent of the model.
    pub status: PortfolioStatusResponse,
    /// Typed error code on degrade (`inference_unavailable` | `inference_failed`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Actionable operator message on degrade.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// Build a clearly-marked deterministic narrative from the status snapshot.
///
/// Why: the DOC-16 D1 fallback — a templated bullet list from `/status`, used
/// verbatim when no provider is configured or the call fails. It must be
/// self-evidently NOT model prose (so a reader never mistakes it for reasoning)
/// while still conveying the headline portfolio state.
/// What: renders a fixed-format bullet list of the session / Deliverable /
/// Milestone histograms and the most-recent activity, prefixed with an explicit
/// "deterministic fallback" marker line.
/// Test: `deterministic_narrative_marks_fallback`.
pub fn deterministic_narrative(scope: &str, status: &PortfolioStatusResponse) -> String {
    let t = &status.totals;
    let last = status
        .totals
        .last_activity_at
        .map(|ts| ts.to_rfc3339())
        .unwrap_or_else(|| "none".to_string());
    format!(
        "[deterministic fallback — no inference provider configured] {scope} rollup:\n\
         - Projects: {projects}\n\
         - Sessions: {active} active, {provisioning} provisioning, {stopped} stopped, \
         {errored} errored ({sessions_total} total)\n\
         - Deliverables: {in_progress} in progress, {blocked} blocked, {complete} complete, \
         {delivered} delivered, {shipped} shipped ({deliverables_total} total)\n\
         - Milestones: {m_in_progress} in progress, {m_complete} complete, {m_shipped} shipped \
         ({milestones_total} total)\n\
         - Most recent activity: {last}",
        projects = status.project_count,
        active = t.sessions.active,
        provisioning = t.sessions.provisioning,
        stopped = t.sessions.stopped,
        errored = t.sessions.errored,
        sessions_total = t.sessions.total,
        in_progress = t.deliverables.in_progress,
        blocked = t.deliverables.blocked,
        complete = t.deliverables.complete,
        delivered = t.deliverables.delivered,
        shipped = t.deliverables.shipped,
        deliverables_total = t.deliverables.total,
        m_in_progress = t.milestones.in_progress,
        m_complete = t.milestones.complete,
        m_shipped = t.milestones.shipped,
        milestones_total = t.milestones.total,
    )
}

/// Build the LLM prompt for the digest: a grounded system persona + the snapshot.
///
/// Why: the narrative must be grounded strictly in the supplied deterministic
/// snapshot (no invented state), so the system message pins the read-only
/// portfolio-manager persona and the user message carries the snapshot as JSON.
/// Keeping this a pure function makes the prompt testable without a live model and
/// keeps the handler thin.
/// What: returns a two-message history — a system instruction and a user message
/// embedding the scope label plus the pretty-printed [`PortfolioStatusResponse`].
/// Test: `build_digest_messages_grounds_on_snapshot`.
pub fn build_digest_messages(scope: &str, status: &PortfolioStatusResponse) -> Vec<ChatMessage> {
    let snapshot = serde_json::to_string_pretty(status)
        .unwrap_or_else(|_| "{\"error\":\"snapshot serialization failed\"}".to_string());
    let system = "You are the read-only portfolio manager for a software developer running \
         many coding sessions across multiple projects. Given a deterministic status \
         snapshot, write a concise (3-6 sentence) plain-prose narrative of what is going \
         on right now: what has momentum, what is blocked or needs attention, and where \
         activity is concentrated. Use ONLY the numbers in the snapshot — never invent \
         projects, sessions, or counts. Do not use bullet lists or headings; write flowing \
         prose. Do not suggest mutating actions.";
    let user = format!("Scope: {scope}\n\nDeterministic status snapshot (JSON):\n{snapshot}");
    vec![ChatMessage::system(system), ChatMessage::user(user)]
}

/// `GET /api/v1/manager/digest?scope=portfolio|project:<name>` handler.
///
/// Why: the curl-first (§4) digest surface. It composes the deterministic rollup,
/// narrows it to scope, and asks the configured adapter for one grounded
/// narrative — degrading to the deterministic templated fallback (clearly marked)
/// with a typed status code whenever inference is unavailable or fails.
/// What: parses `scope` (400 on malformed), loads the snapshot via
/// [`load_portfolio_status`], narrows to a single project (404 if unknown) or the
/// whole portfolio, then resolves the inference seam: on success issues ONE
/// [`trusty_common::inference::InferenceAdapter::chat`] and returns 200 with the
/// prose + snapshot; on no-provider returns 503 + fallback; on an empty/failed
/// call returns 502 + fallback. Never logs prompt/response text (privacy).
/// Test: HTTP coverage in `tests/manager_inference.rs`.
pub async fn manager_digest_route(
    State(state): State<Arc<DaemonState>>,
    Query(query): Query<DigestQuery>,
) -> impl IntoResponse {
    let scope = match parse_scope(query.scope.as_deref()) {
        Ok(scope) => scope,
        Err(msg) => return (StatusCode::BAD_REQUEST, msg).into_response(),
    };

    let full = match load_portfolio_status(&state).await {
        Ok(full) => full,
        Err((code, msg)) => return (code, msg).into_response(),
    };

    // Narrow to scope, reusing the per-project rollups verbatim (never re-derived).
    let scoped = match &scope {
        DigestScope::Portfolio => full,
        DigestScope::Project(name) => {
            match full.projects.iter().find(|p| &p.project_name == name) {
                Some(p) => rollup_of(vec![p.clone()]),
                None => {
                    return (
                        StatusCode::NOT_FOUND,
                        format!("project '{name}' is not registered"),
                    )
                        .into_response();
                }
            }
        }
    };

    let scope_label = scope.label();

    // Resolve a live adapter; no provider → deterministic fallback (503).
    let (model, adapter) = match state.manager_state().inference().resolve() {
        Ok(pair) => pair,
        Err(unavailable) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(DigestResponse {
                    scope: scope_label.clone(),
                    generated_by: GENERATED_BY_FALLBACK,
                    model: None,
                    narrative: deterministic_narrative(&scope_label, &scoped),
                    status: scoped,
                    error: Some("inference_unavailable".to_string()),
                    message: Some(unavailable.to_string()),
                }),
            )
                .into_response();
        }
    };

    // Issue exactly ONE chat call; any failure degrades to the fallback (502).
    let mut request = ChatRequest::new(model.clone(), build_digest_messages(&scope_label, &scoped));
    request.max_tokens = Some(MANAGER_MAX_TOKENS);
    request.temperature = Some(MANAGER_TEMPERATURE);

    match adapter.chat(&request).await {
        Ok(response) => match response.first_text().filter(|t| !t.trim().is_empty()) {
            Some(narrative) => Json(DigestResponse {
                scope: scope_label,
                generated_by: GENERATED_BY_LLM,
                model: Some(model),
                narrative,
                status: scoped,
                error: None,
                message: None,
            })
            .into_response(),
            None => digest_call_failed(scope_label, scoped, "provider returned an empty narrative"),
        },
        Err(e) => {
            tracing::warn!("manager digest inference call failed: {e}");
            digest_call_failed(scope_label, scoped, "inference call failed")
        }
    }
}

/// Build the 502 degrade response when the adapter is present but the call fails.
///
/// Why: DOC-16 D1's "deterministic fallback on failure" — a configured provider
/// that errors (network, upstream 5xx, empty body) must still return the numbers
/// and a clearly-marked deterministic narrative, distinct (502) from the
/// no-provider 503 case.
/// What: renders a [`DigestResponse`] with the fallback narrative + an
/// `inference_failed` error code as a `502`.
/// Test: HTTP coverage in `tests/manager_inference.rs`.
fn digest_call_failed(
    scope_label: String,
    status: PortfolioStatusResponse,
    reason: &str,
) -> axum::response::Response {
    (
        StatusCode::BAD_GATEWAY,
        Json(DigestResponse {
            scope: scope_label.clone(),
            generated_by: GENERATED_BY_FALLBACK,
            model: None,
            narrative: deterministic_narrative(&scope_label, &status),
            status,
            error: Some("inference_failed".to_string()),
            message: Some(format!(
                "{reason}; returning the deterministic rollup — see GET /api/v1/manager/status"
            )),
        }),
    )
        .into_response()
}

#[cfg(test)]
#[path = "digest_tests.rs"]
mod tests;
