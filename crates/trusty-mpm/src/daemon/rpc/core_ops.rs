//! Transport-neutral bodies for the daemon's core request/response routes.
//!
//! Why (#6288 slice 2): every route in the health/doctor/errors/report-bug,
//! breakers/optimizer/overseer/llm-chat and tmux families is now reachable two
//! ways — `daemon::api`'s axum handler and [`super::core`]'s JSON-RPC method.
//! Two transports over one route must not become two implementations of it: the
//! moment a fix lands in one copy and not the other, a caller's answer depends
//! on which socket it happened to dial. So the body lives here exactly once and
//! both transports call in.
//!
//! What: one function per route, taking `&Arc<DaemonState>` plus the route's own
//! decoded arguments and returning the response type the HTTP handler used to
//! build. Nothing here names an HTTP type — no `Json`, no `StatusCode`, no
//! extractor — and nothing names a JSON-RPC type either. Failures are
//! [`DaemonError`], which both transports already know how to render
//! (`IntoResponse` for HTTP, `From<DaemonError> for RpcError` for the socket).
//!
//! Why these bodies MOVED rather than staying in `api.rs` behind a sibling
//! wrapper: `api.rs` sits on a frozen 1176-SLOC ratchet budget with 15 lines of
//! headroom, and eleven extra wrappers would have pushed it over. The split
//! ships in the PR that forces it, per the SLOC-cap rule.
//!
//! Test: `super::core_tests` — the `rpc_*` and `parity_*` cases drive these
//! through BOTH transports and compare the answers.

use std::sync::Arc;

use crate::daemon::api::types::{
    AdoptResponse, BreakerEntry, BreakersResponse, BugReportPreview, ErrorSummary, ErrorsResponse,
    HealthResponse, LlmChatRequest, LlmChatResponse, OptimizerResponse, OverseerResponse,
    OverseerStatus, ReportBugHttpResponse, ScrubChangeSummary, TmuxSessionsResponse,
    TmuxSnapshotResponse,
};
use crate::daemon::api::{AdoptRequest, DoctorQuery, ErrorsQuery, ReportBugApiRequest};
use crate::daemon::bug_report;
use crate::daemon::error::DaemonError;
use crate::daemon::services::TmuxService;
use crate::daemon::state::DaemonState;

/// Liveness plus the HR-3 catalog-staleness signal (`GET /health`, `mpm.health`).
///
/// Test: `health_reports_ok_status`, `parity_health_agrees_across_transports`.
pub async fn health(state: &Arc<DaemonState>) -> HealthResponse {
    let fw = crate::core::paths::FrameworkPaths::from_root(state.framework_root());
    // The daemon-wide baseline uses the framework root as the "project" so only
    // the user/catalog/default manifest layers apply (no per-project override).
    let report = crate::core::update_check::detect_for_framework(&fw, state.framework_root());
    HealthResponse {
        status: "ok".to_owned(),
        catalog_stale: report.stale,
        catalog_unknown: report.unknown,
        catalog_changes: report.summary_lines(),
        supervised: state.supervised(),
        // #4469: the three-state answer the bool above collapses. Published so
        // `tm doctor` can distinguish "launchd says no" from "could not ask".
        launchd_supervision: state.launchd_supervision(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
        // #4230: identify WHICH process answered, so `tm doctor` can compare it
        // against the PID launchd owns instead of trusting `supervised` alone.
        pid: std::process::id(),
        unsupervised_forced: state.unsupervised_forced(),
    }
}

/// The full stack diagnostic (`GET /api/v1/doctor`, `mpm.doctor`).
///
/// Test: `doctor_endpoint_returns_report`, `parity_doctor_agrees_across_transports`.
pub async fn doctor(
    state: &Arc<DaemonState>,
    query: DoctorQuery,
) -> crate::core::doctor::DoctorReport {
    // #6336: the workspace paths, the managed root, and the reconciled worktree
    // counts are all derived by `run_doctor_for_manager`, so this route and the
    // daemonless `tm doctor` CLI cannot drift on how the fleet is read.
    let mgr = state.session_manager().await;
    crate::daemon::doctor::run_doctor_for_manager(&mgr, query.project.as_deref()).await
}

/// Recently captured errors from every daemon store (`GET /api/v1/errors`,
/// `mpm.errors.list`).
///
/// Test: `parity_errors_agrees_across_transports`.
pub fn list_errors(_state: &Arc<DaemonState>, query: ErrorsQuery) -> ErrorsResponse {
    let limit = query.limit.unwrap_or(20).min(100) as usize;
    let errors = bug_report::aggregate_errors(limit);
    let summaries: Vec<ErrorSummary> = errors
        .iter()
        .map(|e| ErrorSummary {
            fingerprint: e.record.fingerprint.clone(),
            crate_target: e.record.crate_target.clone(),
            crate_version: e.record.crate_version.clone(),
            summary: e.record.summary(),
            occurrences: e.occurrences,
            timestamp_secs: e.record.timestamp_secs,
        })
        .collect();
    let total = summaries.len();
    ErrorsResponse {
        errors: summaries,
        total,
        limit,
    }
}

/// Build a [`BugReportPreview`] from a scrubbed [`bug_report::IssuePreview`].
///
/// Why: both the `confirm:false` path and the rate-limited path must return the
///      same preview shape so callers can inspect before (or after a blocked)
///      filing.
/// Test: exercised transitively by `report_bug_no_confirm_includes_preview`.
pub fn to_wire_preview(p: &bug_report::IssuePreview) -> BugReportPreview {
    BugReportPreview {
        title: p.title.clone(),
        body: p.body.clone(),
        labels: p.labels.clone(),
        scrub_changes: p
            .scrub_changes
            .iter()
            .map(|c| ScrubChangeSummary {
                pattern: c.pattern.to_string(),
                hint: c.hint.to_string(),
            })
            .collect(),
    }
}

/// File or preview a bug report (`POST /api/v1/report-bug`, `mpm.report_bug`).
///
/// Why this never returns `Err`: filing is best-effort, and every failure mode —
/// an unknown fingerprint, a missing token, a rate-limit block, a GitHub
/// rejection — is reported IN the response so the caller learns which one it
/// hit. The HTTP route is documented as always `200` for exactly that reason,
/// and the socket method inherits it: `mpm.report_bug` answers with a RESULT
/// frame whose `filed` is `false`, never a coded error frame.
///
/// The consent gate is `confirm`, and the transport does not change it: nothing
/// is filed unless the caller sets it. [`super::core`]'s module doc records why
/// the socket's peer-uid check is the stronger half of the guard pair here.
///
/// Test: `report_bug_no_confirm_includes_preview`,
/// `report_bug_not_found_fingerprint_is_graceful`,
/// `report_bug_rate_limit_guard_blocks_correctly`,
/// `parity_report_bug_preview_agrees_across_transports`.
pub async fn report_bug(
    _state: &Arc<DaemonState>,
    body: ReportBugApiRequest,
) -> ReportBugHttpResponse {
    // Load errors and find the requested fingerprint.
    let errors = bug_report::aggregate_errors(500);
    let found = errors
        .into_iter()
        .find(|e| e.record.fingerprint == body.fingerprint);

    let Some(agg) = found else {
        return ReportBugHttpResponse {
            filed: false,
            deduped: None,
            issue_url: None,
            issue_number: None,
            note: Some(format!(
                "fingerprint `{}` not found in local error stores; \
                 run GET /api/v1/errors to see available fingerprints",
                body.fingerprint
            )),
            preview: None,
            rate_limited: None,
        };
    };

    let preview = bug_report::build_preview(&agg);

    // Fix 2 (P1): include scrubbed preview in confirm:false response.
    if !body.confirm {
        return ReportBugHttpResponse {
            filed: false,
            deduped: None,
            issue_url: None,
            issue_number: None,
            note: Some("confirm:false — preview only. POST with confirm:true to file.".to_string()),
            preview: Some(to_wire_preview(&preview)),
            rate_limited: None,
        };
    }

    // Fix 3 (P2): check the rate-limit guard before calling GitHub.
    let guard = bug_report::RateLimitGuard::production();
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let rl_decision = guard.check(&body.fingerprint, now_secs);
    if !rl_decision.is_allowed() {
        return ReportBugHttpResponse {
            filed: false,
            deduped: None,
            issue_url: None,
            issue_number: None,
            note: Some(rl_decision.block_reason()),
            preview: None,
            rate_limited: Some(true),
        };
    }

    // Fix 1 (P0): use the full resolution chain — PAT → file → GitHub App → NoToken.
    let fingerprint = body.fingerprint.clone();
    let provider = bug_report::ResolvedProvider;
    let result =
        tokio::task::spawn_blocking(move || bug_report::file_issue(&preview, &provider)).await;

    match result {
        Ok(Ok(filing)) => {
            // Fix 3 (P2): record the successful filing in the rate-limit store.
            // State-file failures are non-fatal — log and allow.
            guard.record_filed(&fingerprint, now_secs);
            ReportBugHttpResponse {
                filed: true,
                deduped: Some(filing.deduped),
                issue_url: Some(filing.issue_url),
                issue_number: Some(filing.issue_number),
                note: None,
                preview: None,
                rate_limited: None,
            }
        }
        Ok(Err(bug_report::GithubFilingError::NoToken)) => ReportBugHttpResponse {
            filed: false,
            deduped: None,
            issue_url: None,
            issue_number: None,
            note: Some(bug_report::GithubFilingError::NoToken.to_string()),
            preview: None,
            rate_limited: None,
        },
        Ok(Err(e)) => ReportBugHttpResponse {
            filed: false,
            deduped: None,
            issue_url: None,
            issue_number: None,
            note: Some(format!("GitHub filing failed: {e}")),
            preview: None,
            rate_limited: None,
        },
        Err(e) => ReportBugHttpResponse {
            filed: false,
            deduped: None,
            issue_url: None,
            issue_number: None,
            note: Some(format!("internal error: {e}")),
            preview: None,
            rate_limited: None,
        },
    }
}

/// Every agent's circuit-breaker state (`GET /breakers`, `mpm.breakers`).
///
/// Test: `parity_breakers_agrees_across_transports`.
pub fn breakers(state: &Arc<DaemonState>) -> BreakersResponse {
    let breakers = state
        .all_breakers()
        .into_iter()
        .map(|(agent, breaker)| BreakerEntry { agent, breaker })
        .collect();
    BreakersResponse { breakers }
}

/// The overseer's enabled flag and active handler (`GET /overseer`,
/// `mpm.overseer`).
///
/// Test: `parity_overseer_agrees_across_transports`.
pub fn overseer(state: &Arc<DaemonState>) -> OverseerResponse {
    OverseerResponse {
        overseer: OverseerStatus {
            enabled: state.overseer().is_enabled(),
            handler: state.overseer_handler().to_string(),
        },
    }
}

/// The token-use optimizer configuration (`GET /optimizer`, `mpm.optimizer`).
///
/// Test: `parity_optimizer_agrees_across_transports`.
pub fn optimizer(state: &Arc<DaemonState>) -> OptimizerResponse {
    OptimizerResponse {
        optimizer: state.optimizer_config(),
        scope: crate::daemon::optimizer::OPTIMIZER_SCOPE_NOTE.to_string(),
    }
}

/// One turn of the LLM chat assistant (`POST /llm/chat`, `mpm.llm.chat`).
///
/// # Errors
///
/// [`DaemonError::ServiceUnavailable`] when no LLM overseer is configured (HTTP
/// 503 / `CODE_UNAVAILABLE` on the socket), or [`DaemonError::Internal`] when
/// the provider call fails.
///
/// Test: `llm_chat_without_overseer_is_503`,
/// `rpc_llm_chat_without_overseer_reports_unavailable`.
pub async fn llm_chat(
    state: &Arc<DaemonState>,
    body: LlmChatRequest,
) -> Result<LlmChatResponse, DaemonError> {
    let overseer = state.llm_overseer().ok_or_else(|| {
        DaemonError::ServiceUnavailable(
            "LLM chat is not configured (no OpenRouter API key)".to_string(),
        )
    })?;
    let mut history = body.history;
    let reply = overseer
        .chat(&mut history, &body.message)
        .await
        .map_err(|e| DaemonError::Internal(e.to_string()))?;
    Ok(LlmChatResponse { reply, history })
}

/// Every tmux session with its origin label (`GET /tmux/sessions`,
/// `mpm.tmux.sessions`).
///
/// Test: `parity_tmux_sessions_agrees_across_transports`.
pub fn list_tmux_sessions(_state: &Arc<DaemonState>) -> TmuxSessionsResponse {
    TmuxSessionsResponse {
        sessions: TmuxService::list_all(),
    }
}

/// One session's last 100 pane lines (`GET /tmux/sessions/{name}/snapshot`,
/// `mpm.tmux.snapshot`).
///
/// # Errors
///
/// [`DaemonError`] when the session is missing or tmux is unavailable.
///
/// Test: `tmux_snapshot_unknown_session_is_404`,
/// `rpc_tmux_snapshot_unknown_session_reports_a_coded_error`.
pub fn tmux_snapshot(
    _state: &Arc<DaemonState>,
    name: &str,
) -> Result<TmuxSnapshotResponse, DaemonError> {
    let snapshot = TmuxService::snapshot(name, 100)?;
    Ok(TmuxSnapshotResponse { snapshot })
}

/// Bring an external tmux session under oversight (`POST /tmux/adopt`,
/// `mpm.tmux.adopt`).
///
/// # Errors
///
/// [`DaemonError`] when the session is missing or tmux is unavailable.
///
/// Test: `adopt_tmux_session_handles_missing`,
/// `parity_tmux_adopt_unknown_session_agrees_across_transports`.
pub fn adopt_tmux(
    _state: &Arc<DaemonState>,
    body: AdoptRequest,
) -> Result<AdoptResponse, DaemonError> {
    let adopted = TmuxService::adopt(&body.session)?;
    Ok(AdoptResponse { adopted })
}
