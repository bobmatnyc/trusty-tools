//! Typed HTTP API response bodies.
//!
//! Why: handlers previously returned `Json<serde_json::Value>` built by
//! `json!` macros, so the response shape was only checked at runtime by a
//! large suite of string-indexing contract tests. Naming each response as a
//! `#[derive(Serialize, Deserialize)]` struct moves that contract to the type
//! system — a misnamed or missing field is now a compile error.
//! What: one struct per HTTP endpoint that returns a JSON object, mirroring the
//! exact field names the `json!` macros produced so the wire format is
//! unchanged.
//! Test: `cargo test -p trusty-mpm-daemon` drives the handlers and reads typed
//! fields directly; `cargo check` proves the structs match the handler bodies.

use serde::{Deserialize, Serialize};

use crate::core::circuit::CircuitBreaker;
use crate::core::claude_config::{ClaudeConfig, ConfigRecommendation, DeploymentProfile};
use crate::core::external_session::ExternalSession;
use crate::core::hook::{HookEvent, HookEventRecord};
use crate::core::session::Session;

use crate::daemon::optimizer::OptimizerConfig;
use crate::daemon::tmux::{AdoptedSession, SessionSnapshot};

/// Response of `GET /sessions`.
#[derive(Debug, Serialize, Deserialize)]
pub struct SessionsResponse {
    /// Snapshot of managed sessions.
    pub sessions: Vec<Session>,
}

/// Response of `GET /events` and `GET /sessions/{id}/events`.
#[derive(Debug, Serialize, Deserialize)]
pub struct EventsResponse {
    /// Recent hook events.
    pub events: Vec<HookEventRecord>,
}

/// Response of `POST /sessions`.
#[derive(Debug, Serialize, Deserialize)]
pub struct RegisterSessionResponse {
    /// The new session's id.
    pub id: crate::core::session::SessionId,
    /// The session's friendly tmux name.
    pub name: String,
}

/// Response of `DELETE /sessions/{id}`.
#[derive(Debug, Serialize, Deserialize)]
pub struct RemoveSessionResponse {
    /// The id of the removed session.
    pub removed: String,
}

/// Response of `DELETE /sessions/dead`.
#[derive(Debug, Serialize, Deserialize)]
pub struct ReapResponse {
    /// Number of dead sessions reaped (tmux session gone, entry removed).
    pub removed: usize,
    /// Number of alive tmux sessions marked `Stopped` because their `claude`
    /// process exited.
    #[serde(default)]
    pub stopped: usize,
}

/// Response of `PATCH /sessions/{id}/pid`.
#[derive(Debug, Serialize, Deserialize)]
pub struct SetPidResponse {
    /// The session id the PID was recorded on.
    pub session_id: String,
    /// The OS-level `claude` process PID now tracked for the session.
    pub pid: u32,
}

/// Response of `POST /sessions/discover`.
///
/// Why: the auto-discovery endpoint reports how many tmux sessions running
/// Claude Code it newly registered, so a UI can tell the operator what changed.
/// What: the count plus the friendly names of the discovered sessions.
/// Test: `discover_sessions_returns_count` in `api_tests.rs`.
#[derive(Debug, Serialize, Deserialize)]
pub struct DiscoverResponse {
    /// Number of tmux sessions newly registered by the scan.
    pub discovered: usize,
    /// Friendly tmux names of the newly-registered sessions.
    pub sessions: Vec<String>,
}

/// Response of `POST /pair/reset`.
///
/// Why: clearing the pairing should give the caller an explicit acknowledgement.
/// What: a `reset` flag, always `true` on a successful call.
/// Test: `pair_reset_clears_pairing` in `api_tests.rs`.
#[derive(Debug, Serialize, Deserialize)]
pub struct PairResetResponse {
    /// Always `true` — the pairing was cleared.
    pub reset: bool,
}

/// Response of `POST /sessions/{id}/pause`.
#[derive(Debug, Serialize, Deserialize)]
pub struct PauseResponse {
    /// Always `true` — the session is now paused.
    pub paused: bool,
    /// The resolved session id.
    pub session_id: String,
    /// The pause summary (operator note or auto-derived).
    pub summary: String,
}

/// Response of `POST /sessions/{id}/resume`.
#[derive(Debug, Serialize, Deserialize)]
pub struct ResumeResponse {
    /// Always `true` — the session is now active.
    pub resumed: bool,
}

/// Response of `POST /sessions/{id}/command`.
#[derive(Debug, Serialize, Deserialize)]
pub struct CommandResponse {
    /// Always `true` — the command was sent.
    pub sent: bool,
    /// Captured pane output (possibly compressed).
    pub output: String,
    /// Output size in bytes before compression.
    pub original_bytes: usize,
    /// Output size in bytes after compression.
    pub compressed_bytes: usize,
    /// Applied compression level label, or `null` when uncompressed.
    pub compress_level: Option<String>,
}

/// Response of `GET /sessions/{id}/output`.
#[derive(Debug, Serialize, Deserialize)]
pub struct OutputResponse {
    /// Captured pane output (possibly compressed).
    pub output: String,
    /// Number of trailing pane lines captured.
    pub lines: u32,
    /// Output size in bytes before compression.
    pub original_bytes: usize,
    /// Output size in bytes after compression.
    pub compressed_bytes: usize,
    /// Applied compression level label, or `null` when uncompressed.
    pub compress_level: Option<String>,
}

/// Response of `GET /projects`.
#[derive(Debug, Serialize, Deserialize)]
pub struct ProjectsResponse {
    /// Registered projects.
    pub projects: Vec<crate::core::project::ProjectInfo>,
}

/// One discovered Claude Code project in [`DiscoverProjectsResponse`].
///
/// Why: `GET /projects/discover` reports projects mined from
/// `~/.claude/projects/`; each row needs the decoded path, how many sessions
/// were recorded, and when the project was last used.
/// What: the absolute project path, the `.jsonl` transcript count, and the
/// most-recent session time as an ISO-8601 string (`None` when the project has
/// no transcripts).
/// Test: `cargo test -p trusty-mpm-daemon` drives `discover_projects`.
#[derive(Debug, Serialize, Deserialize)]
pub struct DiscoveredProjectInfo {
    /// Absolute path to the project's working directory.
    pub path: String,
    /// Number of `.jsonl` session transcripts recorded for the project.
    pub session_count: usize,
    /// ISO-8601 timestamp of the most recent session, or `null` when none.
    pub last_session: Option<String>,
}

/// Response of `GET /projects/discover`.
#[derive(Debug, Serialize, Deserialize)]
pub struct DiscoverProjectsResponse {
    /// Projects discovered under `~/.claude/projects/`, newest-session first.
    pub projects: Vec<DiscoveredProjectInfo>,
}

/// One agent's circuit-breaker row in [`BreakersResponse`].
#[derive(Debug, Serialize, Deserialize)]
pub struct BreakerEntry {
    /// Agent name the breaker guards.
    pub agent: String,
    /// The breaker's current state.
    pub breaker: CircuitBreaker,
}

/// Response of `GET /breakers`.
#[derive(Debug, Serialize, Deserialize)]
pub struct BreakersResponse {
    /// Per-agent circuit-breaker states.
    pub breakers: Vec<BreakerEntry>,
}

/// Response of `POST /hooks`.
#[derive(Debug, Serialize, Deserialize)]
pub struct HookAcceptedResponse {
    /// The hook event that was accepted.
    pub accepted: HookEvent,
}

/// The overseer status block in [`OverseerResponse`].
#[derive(Debug, Serialize, Deserialize)]
pub struct OverseerStatus {
    /// Whether the overseer is enabled.
    pub enabled: bool,
    /// The active overseer strategy name.
    pub handler: String,
}

/// Response of `GET /overseer`.
#[derive(Debug, Serialize, Deserialize)]
pub struct OverseerResponse {
    /// The overseer configuration and status.
    pub overseer: OverseerStatus,
}

/// Response of `GET /optimizer`.
#[derive(Debug, Serialize, Deserialize)]
pub struct OptimizerResponse {
    /// The current token-use optimizer configuration.
    pub optimizer: OptimizerConfig,
}

/// Response of `GET /tmux/sessions`.
#[derive(Debug, Serialize, Deserialize)]
pub struct TmuxSessionsResponse {
    /// All tmux sessions on the host with origin labels.
    pub sessions: Vec<ExternalSession>,
}

/// Response of `GET /tmux/sessions/{name}/snapshot`.
#[derive(Debug, Serialize, Deserialize)]
pub struct TmuxSnapshotResponse {
    /// The captured session snapshot.
    pub snapshot: SessionSnapshot,
}

/// Response of `POST /tmux/adopt`.
#[derive(Debug, Serialize, Deserialize)]
pub struct AdoptResponse {
    /// The adopted session's captured state.
    pub adopted: AdoptedSession,
}

/// Response of `GET /claude-config`.
#[derive(Debug, Serialize, Deserialize)]
pub struct ClaudeConfigResponse {
    /// The merged Claude Code configuration.
    pub config: ClaudeConfig,
    /// Recommended configuration changes.
    pub recommendations: Vec<ConfigRecommendation>,
}

/// Response of `POST /claude-config/apply`.
#[derive(Debug, Serialize, Deserialize)]
pub struct ApplyConfigResponse {
    /// Always `true` — the recommendation was applied.
    pub applied: bool,
    /// The id of the applied recommendation.
    pub recommendation_id: String,
    /// Checkpoint id created before applying, for undo.
    pub checkpoint_id: String,
}

/// Response of `GET /claude-config/checkpoints`.
#[derive(Debug, Serialize, Deserialize)]
pub struct CheckpointsResponse {
    /// Config checkpoints, newest first.
    pub checkpoints: Vec<crate::core::claude_config::ConfigCheckpoint>,
}

/// Response of `POST /claude-config/checkpoints`.
#[derive(Debug, Serialize, Deserialize)]
pub struct CreateCheckpointResponse {
    /// The new checkpoint's id.
    pub id: String,
}

/// Response of `POST /claude-config/restore`.
#[derive(Debug, Serialize, Deserialize)]
pub struct RestoreResponse {
    /// Always `true` — the config was restored.
    pub restored: bool,
    /// The id of the restored checkpoint.
    pub checkpoint_id: String,
}

/// Response of `DELETE /claude-config/checkpoints/{id}`.
#[derive(Debug, Serialize, Deserialize)]
pub struct DeleteCheckpointResponse {
    /// The id of the deleted checkpoint.
    pub deleted: String,
}

/// Response of `GET /claude-config/profiles`.
#[derive(Debug, Serialize, Deserialize)]
pub struct ProfilesResponse {
    /// The built-in deployment profiles.
    pub profiles: Vec<DeploymentProfile>,
}

/// Response of `POST /claude-config/deploy`.
#[derive(Debug, Serialize, Deserialize)]
pub struct DeployProfileResponse {
    /// The name of the deployed profile.
    pub deployed: String,
    /// Checkpoint id created before deploying, for undo.
    pub checkpoint_id: String,
}

/// Response of `POST /claude-config/restart`.
#[derive(Debug, Serialize, Deserialize)]
pub struct RestartResponse {
    /// The tmux session Claude Code was restarted in.
    pub restarted: String,
}

/// Request body for `POST /llm/chat`.
///
/// Why: the Telegram bot and TUI hold conversation history client-side and send
/// it with each turn so the daemon stays stateless about chat sessions.
/// What: the new user `message` plus the prior conversation `history`.
/// Test: `llm_chat_without_overseer_is_503` covers the no-overseer path.
#[derive(Debug, Serialize, Deserialize, utoipa::ToSchema)]
pub struct LlmChatRequest {
    /// The user's message text.
    pub message: String,
    /// Prior conversation history (oldest first); empty starts a new chat.
    #[serde(default)]
    #[schema(value_type = Vec<Object>)]
    pub history: Vec<crate::daemon::llm_overseer::ChatMessage>,
}

/// Response of `POST /llm/chat`.
///
/// Why: the caller needs both the assistant's reply and the updated history
/// (with the user message and reply appended, capped to the rolling window) so
/// it can store the history for the next turn.
/// What: the assistant `reply` text and the updated `history`.
/// Test: `llm_chat_without_overseer_is_503`.
#[derive(Debug, Serialize, Deserialize)]
pub struct LlmChatResponse {
    /// The assistant's reply text.
    pub reply: String,
    /// The updated conversation history, ready for the next turn.
    pub history: Vec<crate::daemon::llm_overseer::ChatMessage>,
}

/// Response of `POST /pair/confirm`.
#[derive(Debug, Serialize, Deserialize)]
pub struct PairConfirmResponse {
    /// Whether the code was valid and the chat is now paired.
    pub success: bool,
    /// The registered chat id, when `success` is true.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_id: Option<i64>,
    /// Failure reason, when `success` is false.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

// ── Bug-reporting types (Phase 2 surface + Phase 3 filing) ───────────────────

/// Response of `GET /api/v1/errors`.
///
/// Why: the HTTP fallback for `list_recent_errors` lets sub-agents without
///      MCP connections read captured errors via plain HTTP.
/// What: a JSON array of error summaries with dedup fingerprints.
#[derive(Debug, Serialize, Deserialize)]
pub struct ErrorsResponse {
    /// Deduplicated error summaries, most-recent first.
    pub errors: Vec<ErrorSummary>,
    /// Total count in the response (after limit).
    pub total: usize,
    /// The limit applied to the query.
    pub limit: usize,
}

/// One entry in an [`ErrorsResponse`].
#[derive(Debug, Serialize, Deserialize)]
pub struct ErrorSummary {
    /// SHA-256 hex fingerprint (64 chars) for dedup.
    pub fingerprint: String,
    /// Crate target (tracing event target).
    pub crate_target: String,
    /// Version of the daemon that captured the error.
    pub crate_version: String,
    /// One-line human-readable summary.
    pub summary: String,
    /// Occurrence count across all daemon stores.
    pub occurrences: usize,
    /// Unix timestamp (secs) of the most-recent occurrence.
    pub timestamp_secs: u64,
}

/// Scrubbed-change entry embedded in bug-report HTTP responses.
///
/// Why: HTTP clients need the same scrub-summary the MCP preview tool returns
///      so they can surface the "what was redacted" summary before consenting.
/// What: carries the pattern name and human-readable hint from the scrubber.
/// Test: embedded in `ReportBugHttpResponse` and verified in
///       `report_bug_no_confirm_includes_preview`.
#[derive(Debug, Serialize, Deserialize)]
pub struct ScrubChangeSummary {
    /// Pattern name (e.g. `"env-secret"`, `"path"`, `"jwt"`).
    pub pattern: String,
    /// Human-readable hint describing what was redacted.
    pub hint: String,
}

/// Scrubbed issue preview embedded in HTTP `confirm:false` responses.
///
/// Why: Fix 2 (#P1) — the HTTP `POST /api/v1/report-bug` `confirm:false` path
///      was returning only a "gate note" and discarding the preview. Including
///      the full preview lets HTTP clients inspect the exact title/body/labels
///      and scrub summary before consenting.
/// What: the preview title, Markdown body, labels, and list of scrub changes
///       — identical shape to the MCP `preview_bug_report` response.
/// Test: `report_bug_no_confirm_includes_preview` in `api_tests.rs`.
#[derive(Debug, Serialize, Deserialize)]
pub struct BugReportPreview {
    /// Issue title (already scrubbed).
    pub title: String,
    /// Issue body in GitHub Markdown (already scrubbed).
    pub body: String,
    /// Labels that will be applied to the issue.
    pub labels: Vec<String>,
    /// List of redactions performed by the scrubber.
    pub scrub_changes: Vec<ScrubChangeSummary>,
}

/// Response of `POST /api/v1/report-bug`.
///
/// Why: mirrors the MCP `report_bug` result so HTTP-based sub-agents get the
///      same structure as MCP callers. Fixes 1–3 add `preview` (always present
///      on `confirm:false`) and `rate_limited` (set when the rate-limit guard
///      blocked the filing).
/// What: `filed` is `true` on a successful filing. `note` carries an
///       actionable string when `filed` is `false`. `preview` carries the
///       scrubbed preview on the `confirm:false` path. `rate_limited` is `true`
///       when the per-fingerprint or hourly cap blocked the call.
/// Test: `report_bug_no_confirm_includes_preview`,
///       `report_bug_rate_limited_returns_not_filed` in `api_tests.rs`.
#[derive(Debug, Serialize, Deserialize)]
pub struct ReportBugHttpResponse {
    /// `true` when a GitHub issue was created or incremented.
    pub filed: bool,
    /// `true` when an existing open issue was incremented instead of creating.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deduped: Option<bool>,
    /// HTML URL of the issue that was created or incremented.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issue_url: Option<String>,
    /// Issue number in `bobmatnyc/trusty-tools`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issue_number: Option<u64>,
    /// Actionable message when `filed` is `false`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Scrubbed preview returned on `confirm:false` calls so callers can
    /// inspect title/body/labels/scrub-summary before consenting. Absent on
    /// `confirm:true` responses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<BugReportPreview>,
    /// `true` when the rate-limit guard (per-fingerprint 24h window or hourly
    /// cap) blocked the filing. Only set to `true` when the guard fires.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_limited: Option<bool>,
}
