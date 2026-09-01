//! HTTP route handlers for the managed session-manager API.
//!
//! Why: the managed session API (POST/GET /api/v1/sessions/managed/…) is a new
//! cohesive cluster of routes that extends the existing axum router; keeping it
//! in a separate file mirrors the existing coordinator_routes pattern and keeps
//! api.rs focused on the core session/hook surface.
//! What: defines request/response shapes and axum handlers for the managed
//! session endpoints listed in the spec. All handlers receive Arc<DaemonState>
//! via the axum State extractor and delegate to the lazily-initialized
//! [`crate::session_manager::SessionManager`].
//! Test: handler behavior is exercised via the in-process router in
//! `tests/session_manager_mvp.rs`.

use std::sync::Arc;

use axum::{
    Json,
    extract::{Path as AxumPath, State},
    response::IntoResponse,
};
use serde::{Deserialize, Serialize};

use crate::daemon::state::DaemonState;
use crate::runtime::RuntimeKind;
use crate::session_manager::ManagedSessionId;

pub mod activity;
pub(crate) mod cores;
pub mod delete;
mod deliverable_link;
mod deployment_check;
pub mod fleet;
mod foreign_harness;
pub mod front_gate;
pub mod inproject;
pub mod inproject_cold_start;
pub mod inproject_hygiene;
mod inproject_start_point;
mod launch_on_main;
mod lifecycle;
pub mod managed_checkout;
mod mcp_spawn_gate;
// #6288: `pub` so `rpc::registry::projects` can call the shared `*_op` bodies.
pub mod project_registry_routes;
// #6288: `pub` so `rpc::registry::projects` can call `project_status_op`.
pub mod project_status;
pub mod provision_status;
pub mod proxy;
pub mod prune;
pub mod reactivate;
pub mod reconcile;
// #6497: the explicit ownership transfer for a dead owner's worktree.
pub mod adopt_worktree;
pub mod rename;
mod resume_error;
pub(crate) mod route_outcome_http;
mod session_prep;
mod session_summary;
pub(crate) mod summary;
pub mod sync_assets;
pub use activity::{ActivityResponse, get_session_activity};
pub use delete::{delete_managed_session, stop_managed_session};
pub use fleet::{FleetByProjectResponse, FleetProjectGroup, fleet_by_project_route};
pub use front_gate::{
    ConformanceGate, FrontGateOutcome, HeadlessApproval, IsrConformanceGate, run_front_gate,
};
pub use lifecycle::{
    SpawnParams, is_local_workdir, resume_managed, spawn_managed, spawn_runtime_for, write_task_md,
};
pub use project_registry_routes::{
    PatchProjectBody, ProjectsListResponse, RegisterProjectBody, get_project_registry_route,
    list_projects_registry_route, patch_project_registry_route, register_project_registry_route,
};
pub use project_status::{
    DeliverableStatusCounts, MilestoneStatusCounts, ProjectConfigFlags, ProjectStatusResponse,
    SessionStateCounts, aggregate_project_status, project_status_route,
};
pub use proxy::{
    DirectManagedBackend, ProxyFocusRequest, ProxyFocusResponse, ProxyMessageRequest,
    ProxyMessageResponse, ProxySummaryResponse, ProxyTargetWire, ProxyUnfocusRequest,
    ProxyUnfocusResponse, proxy_focus, proxy_get_focus, proxy_message, proxy_router, proxy_summary,
    proxy_unfocus,
};
pub use prune::{
    EphemeralQuery, PruneRequest, decommission_ephemeral_route, prune_managed_route,
    prune_worktrees_route,
};
pub use reactivate::{ReactivateQuery, reactivate_managed_session};
pub use rename::{RenameRequest, rename_managed_session};
pub use resume_error::ResumeManagedError;
pub use session_summary::SessionSummary;
pub use summary::record_to_json;
use summary::{
    attach_cmd_for, checked_summaries, numbered_summaries, parse_id, reconcile_against_tmux,
    record_to_summary, record_to_summary_checked,
};

// ── Request / Response shapes ─────────────────────────────────────────────────

/// Request body for POST /api/v1/sessions/managed (spawn).
///
/// Why: the calling agentic process must supply the repo, ref, and task;
/// an optional name hint overrides the auto-generated tmux session name, and an
/// optional runtime selector picks the backend (Claude Code vs trusty-code).
/// What: deserializable JSON body with repo_url, ref, task, optional name_hint,
/// and optional `runtime` (`"claude-code"` | `"tcode"`; defaults to claude-code).
/// Test: spawn handler test in session_manager_mvp.rs; `spawn_request_runtime_*`.
#[derive(Debug, Deserialize)]
pub struct SpawnRequest {
    /// Repository URL to provision the session workspace from.
    pub repo_url: String,
    /// Git branch or ref to check out.
    #[serde(rename = "ref")]
    pub git_ref: String,
    /// Human-readable task description for the session.
    pub task: String,
    /// Optional name hint overriding the auto-generated tmux session name.
    pub name_hint: Option<String>,
    /// Optional runtime selector (`"claude-code"` | `"tcode"`).
    ///
    /// Absent or null → the default Claude Code path, so existing callers are
    /// unaffected. Parsed via `crate::runtime::RuntimeKind::from_str`; an
    /// unrecognized value yields a `400 Bad Request`.
    pub runtime: Option<String>,
    /// Optional ephemeral marker (#1508): `true` tags this as a test/throwaway
    /// session eligible for bulk teardown + age-based auto-reap. Absent/null/false
    /// → a normal durable session the automatic paths never touch.
    #[serde(default)]
    pub ephemeral: Option<bool>,
    /// Optional turnkey-injection control (#1903/#1299): absent/null/`true` →
    /// auto-inject `task` into the pane once the runtime is ready (the default,
    /// so `tm session new`/`tm ticket` are turnkey); `false` → metadata-only
    /// (the caller delivers the task via `POST .../{id}/send`).
    #[serde(default)]
    pub inject_task: Option<bool>,
    /// Optional Deliverable id to bind this session to (DOC-35 §10.6, #2379).
    ///
    /// Why: `tm sessions new --deliverable <id>` links a fresh session to an
    /// existing Deliverable so its ledger accumulates the sessions that
    /// worked on it. Validated at spawn time (must exist AND belong to this
    /// project) BEFORE any provisioning side effect; an invalid id is a 404,
    /// never a silently-dropped link. Absent/null → no link (the common case).
    #[serde(default)]
    pub deliverable_id: Option<String>,
    /// Optional force-new control (#2450): `true` SKIPS the in-project reconnect
    /// pre-flight so an explicit "launch new session" surface (the `tm` picker's
    /// "launch new session" choice, `tm session new`) always spawns a FRESH
    /// session + worktree instead of adopting an existing live one for the same
    /// project. Absent or `false` → the reconnect default (#1707), so
    /// programmatic/idempotent callers are unaffected. Unlike `inject_task`
    /// above, this is a plain `bool` (not `Option<bool>`): `#[serde(default)]`
    /// on a bool only supplies the default when the KEY IS ABSENT — an
    /// explicit `"force_new": null` in the request body is a type mismatch and
    /// fails the whole request with a 400, it is not tolerated as `false`.
    #[serde(default)]
    pub force_new: bool,
    /// Optional asynchronous-spawn control (#2605): `true` provisions on a
    /// detached background task and returns `202 Accepted` with
    /// `{ id, state: "provisioning" }` IMMEDIATELY, instead of holding the
    /// connection open for the whole clone/deploy. The caller then polls
    /// `GET /api/v1/sessions/managed/{id}/provision-status` for live progress
    /// and the terminal outcome. Absent/`false` → the legacy synchronous `201`
    /// behaviour (unchanged for every existing programmatic/MCP caller), so
    /// this is a purely additive, opt-in flag. Like `force_new`, it is a plain
    /// `#[serde(default)]` bool: an explicit `"background": null` is a type
    /// mismatch (400), not tolerated as `false`.
    #[serde(default)]
    pub background: bool,
    /// Optional EXPLICIT worktree request (#5274): `true` provisions the session
    /// its own per-session git worktree; absent/`false` runs it in the project's
    /// main checkout.
    ///
    /// Why: this is the wire form of the only input allowed to decide session
    /// placement — see [`lifecycle::SpawnParams::worktree`] for why the project's
    /// `worktree` flag deliberately cannot. `tm launch --worktree` sets it; every
    /// other client omits it and gets the main checkout. Like `force_new` and
    /// `background` it is a plain `#[serde(default)]` bool, so an explicit
    /// `"worktree": null` is a 400 rather than a silently-tolerated `false`.
    #[serde(default)]
    pub worktree: bool,
}

/// Response body for POST /api/v1/sessions/managed (spawn, 201 Created).
///
/// Why: the calling agentic process needs the session id, tmux name, workspace path,
/// state, and attach command immediately after spawn.
/// What: serializable JSON with all fields from the spec response shape.
/// Test: spawn handler test.
#[derive(Debug, Serialize)]
pub struct SpawnResponse {
    /// Managed session id (UUID string).
    pub id: String,
    /// tmux session name.
    pub name: String,
    /// Provisioned workspace path, if any.
    pub workspace_path: Option<String>,
    /// Repository URL the session was provisioned from.
    pub repo_url: Option<String>,
    /// Git branch or ref checked out.
    pub branch: Option<String>,
    /// Current lifecycle state.
    pub state: String,
    /// Creation timestamp (RFC 3339).
    pub created_at: String,
    /// tmux attach command string.
    pub attach_cmd: String,
    /// Runtime backend that hosts the session (`"claude-code"` | `"tcode"`).
    pub runtime: String,
    /// The Deliverable this session is bound to, if `--deliverable` was passed
    /// (DOC-35 §10.6, #2379). `None` for the common ad-hoc-session case.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deliverable_id: Option<String>,
}

/// Request body for POST /api/v1/sessions/managed/adopt (#1433).
///
/// Why: adopting an EXISTING unmanaged tmux session connects the managed surface
/// to a pane the operator already has. Unlike the stateless snapshot adopt at
/// `POST /tmux/adopt` (which registers nothing), this REGISTERS a durable record.
/// Because the pane's provenance is unknown to the daemon, the operator must
/// supply the `cwd`; `task` is optional (empty → a generic description) and
/// `runtime` is optional (absent → the default managed runtime).
/// What: `tmux_name` (the live pane to adopt), the required `cwd`, an optional
/// `task`, and an optional `runtime` selector (`"claude-code"` | `"tcode"`).
/// Test: `adopt_existing_*` handler tests in tests/session_manager_mvp.rs.
#[derive(Debug, Deserialize)]
pub struct AdoptExistingRequest {
    /// The live tmux session name to adopt (any name; need not be `tm-`/`tmpm-`).
    pub tmux_name: String,
    /// Working directory the adopted session runs in (REQUIRED — provenance is
    /// unknown to the daemon, so the operator supplies it).
    pub cwd: String,
    /// Optional human-readable task description (empty/absent allowed).
    #[serde(default)]
    pub task: Option<String>,
    /// Optional runtime selector (`"claude-code"` | `"tcode"`); absent → default.
    #[serde(default)]
    pub runtime: Option<String>,
    /// Optional ephemeral marker (#1508): `true` tags this adoption as a
    /// test/throwaway session eligible for bulk teardown + age-based auto-reap.
    /// Absent/null/false → a durable operator adoption the automatic paths never
    /// touch.
    #[serde(default)]
    pub ephemeral: Option<bool>,
}

/// Response body for POST /api/v1/sessions/managed/adopt (201 Created).
///
/// Why: the caller needs the new managed record's identity + the attach command
/// to immediately drive or attach to the now-managed session.
/// What: the same flat summary fields as [`SessionSummary`] plus the derived
/// `attach_cmd`, mirroring [`SpawnResponse`].
/// Test: `adopt_existing_registers_record` in tests/session_manager_mvp.rs.
#[derive(Debug, Serialize)]
pub struct AdoptExistingResponse {
    /// Managed session id (UUID string).
    pub id: String,
    /// tmux session name that was adopted.
    pub name: String,
    /// Current lifecycle state (`active` immediately after adoption).
    pub state: String,
    /// Working directory the adopted session runs in.
    pub cwd: String,
    /// Runtime backend hosting the session (`"claude-code"` | `"tcode"`).
    pub runtime: String,
    /// tmux attach command string.
    pub attach_cmd: String,
}

/// List response for GET /api/v1/sessions/managed.
///
/// Why: the calling agentic process needs the full session list in a consistent
/// JSON shape.
/// What: wraps a vec of SessionSummary.
/// Test: list handler test.
#[derive(Debug, Serialize)]
pub struct ListSessionsResponse {
    /// All managed sessions as summaries.
    pub sessions: Vec<SessionSummary>,
    /// Present ONLY when the daemon served this list from its last-known
    /// in-memory set because it could not read `sessions.json` (#5007).
    ///
    /// Why: the fallback that produced this list is the resilience feature that
    /// hid a totally wedged store — `tm ls` kept printing a healthy fleet while
    /// every write failed. Disclosing the degradation in the same response the
    /// fallback produced is what makes it impossible to read the list without
    /// also seeing that it is stale.
    /// What: `None` (and omitted from the JSON entirely, so a healthy response
    /// is byte-identical to before) whenever the last store read succeeded.
    /// Test: `list_response_omits_store_health_when_healthy`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub store_health: Option<StoreHealthPayload>,
}

/// A store read failure disclosed alongside a degraded list response (#5007).
///
/// Why: see [`ListSessionsResponse::store_health`].
/// What: the rendered error, whether the file is corrupt (permanent) rather
/// than merely unreadable (possibly transient), and when it was observed.
/// Test: `list_response_omits_store_health_when_healthy`.
#[derive(Debug, Serialize)]
pub struct StoreHealthPayload {
    /// The rendered store error — names the file, the byte offset, and the
    /// repair command when the failure is corruption.
    pub message: String,
    /// Whether the file is corrupt rather than transiently unreadable.
    pub corrupt: bool,
    /// RFC3339 timestamp of the observation.
    pub observed_at: String,
}

// `SessionSummary` lives in its own file (issue #2444) — see
// `session_summary.rs`'s module doc for why, and `stale_assets`'s field doc
// for the new drift marker.

/// Response body for POST /api/v1/sessions/managed/{id}/decommission.
///
/// Why: the decommission operation may or may not physically remove the workspace
/// directory (it is skipped for un-owned workspaces such as local-path or adopted
/// sessions). The CLI must surface an honest message, so the daemon reports what
/// actually happened via `workspace_removed`. `workspace_path_was` lets the CLI
/// locate the base git repo for `git worktree prune` without requiring a second
/// lookup — the path is gone from disk but the parent dir still exists.
/// What: extends the flat session summary with a `workspace_removed` bool that is
/// `true` only when `remove_dir_all` succeeded (the workspace was owned and got
/// deleted from disk), and `workspace_path_was` (the pre-tombstone path) when the
/// session had an owned workspace.
/// Test: `decommission_workspace_removed_reflects_ownership` in managed_routes tests.
#[derive(Debug, Serialize)]
pub struct DecommissionResponse {
    /// Flat session summary (post-tombstone: state=decommissioned, workspace_path=None).
    #[serde(flatten)]
    pub summary: SessionSummary,
    /// Whether the workspace directory was actually removed from disk.
    ///
    /// `true`  → SM-owned workspace was deleted by this call.
    /// `false` → workspace was not owned (adopt/local-path) or was already absent.
    pub workspace_removed: bool,
    /// Pre-decommission workspace path string, when the session was owned (`None`
    /// for adopted/local-path sessions where the workspace was never SM-owned).
    /// Used by the CLI to locate the base git repo and run `git worktree prune`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_path_was: Option<String>,
}

/// Query parameters for POST /api/v1/sessions/managed/{id}/delete (#2012).
///
/// Why: hard-delete needs exactly one caller-controlled knob — whether to
/// bypass the running-session safety guard. A query flag (mirroring
/// `?source_id=` on the list route) keeps the call a plain, bodyless POST for
/// the common (non-running) case, while still letting `--force` opt in.
/// What: `force` (default `false`, the fail-closed default) — when absent or
/// `false`, [`crate::session_manager::SessionManager::delete_record`] refuses
/// to delete a RUNNING (`Active`/`Provisioning`) session.
/// Test: `delete_route_*` in managed_routes tests.
#[derive(Debug, Deserialize)]
pub struct DeleteQuery {
    /// Bypass the running-session guard and hard-delete the record anyway.
    #[serde(default)]
    pub force: bool,
}

/// Response body for POST /api/v1/sessions/managed/{id}/delete (#2012).
///
/// Why: delete SOFT-deletes — it marks the record `Deleted` (rendered
/// `--deleted--`) and keeps it in the store. The response carries the
/// PRE-deletion snapshot (id, name, the state it was in BEFORE deletion, …) so
/// the caller/CLI can render an honest `[was <state>]` confirmation.
/// What: the pre-deletion [`SessionSummary`] plus `deleted: true`. Distinct
/// from [`DecommissionResponse`] — delete never mutates the workspace, so
/// there is no `workspace_removed` field here.
/// Test: `delete_route_*` in managed_routes tests.
#[derive(Debug, Serialize)]
pub struct DeleteResponse {
    /// Snapshot of the record as it was immediately BEFORE deletion.
    #[serde(flatten)]
    pub summary: SessionSummary,
    /// Always `true` on success — the record was marked `--deleted--`.
    pub deleted: bool,
}

/// Request body for POST /api/v1/sessions/managed/{id}/send.
///
/// Why: the calling agentic process or operator injects text into the pane.
/// What: a single `text` field.
/// Test: send handler test.
#[derive(Debug, Deserialize)]
pub struct SendInputRequest {
    /// Text to inject into the session's tmux pane.
    pub text: String,
}

/// Response body for POST /api/v1/sessions/managed/{id}/send.
///
/// Why: confirms the inject succeeded without echoing the full session record.
/// What: sent flag and tmux_name for logging.
/// Test: send handler test.
#[derive(Debug, Serialize)]
pub struct SendInputResponse {
    /// True when the text was injected.
    pub sent: bool,
    /// tmux session name the text was sent to.
    pub tmux_name: String,
}

/// Request body for POST /api/v1/sessions/managed/{id}/answer.
///
/// Why: the calling agentic process injects an answer to a pending decision.
/// What: a single `answer` field.
/// Test: answer handler test.
#[derive(Debug, Deserialize)]
pub struct AnswerRequest {
    /// The answer text to inject for the pending decision.
    pub answer: String,
}

/// Response body for POST /api/v1/sessions/managed/{id}/answer.
///
/// Why: confirms the answer was injected.
/// What: injected flag and tmux_name.
/// Test: answer handler test.
#[derive(Debug, Serialize)]
pub struct AnswerResponse {
    /// True when the answer was injected.
    pub injected: bool,
    /// tmux session name the answer was sent to.
    pub tmux_name: String,
}

/// Response body for GET /api/v1/sessions/managed/{id}/attach-cmd.
///
/// Why: the calling agentic process or operator needs the exact tmux command
/// string to attach to the session.
/// What: a single `attach_cmd` field.
/// Test: attach-cmd handler test.
#[derive(Debug, Serialize)]
pub struct AttachCmdResponse {
    /// tmux attach command string.
    pub attach_cmd: String,
}

// ── Handlers ──────────────────────────────────────────────────────────────────

/// POST /api/v1/sessions/managed — spawn a new managed session.
///
/// Why: the primary entry point for the calling agentic process to create a new
/// isolated agent workspace and start a harness in it.
/// What: in order —
///   (a) pre-generates a `ManagedSessionId` so the workspace path can embed it;
///   (b) provisions an isolated workspace via `WorkspaceProvisioner::provision`
///       (clone + prepare_session deploy of agents/skills);
///   (c) creates the tmux session via `SessionManager::create_with_id` with
///       `cwd = workspace_path` so `tmux new-session -c <workspace>` is issued
///       and claude opens IN the provisioned directory, not $HOME;
///   (d) launches Claude Code in the pane via `ClaudeCodeAdapter::spawn`
///       (`env -u ANTHROPIC_API_KEY claude`).
/// On any step failing the record is marked `errored` (or the error is returned
/// before the record is created). No panics, no `unwrap` on the critical path.
/// Test: `handler_spawn_creates_tmux_at_workspace_cwd` in
/// tests/session_manager_mvp.rs asserts the tmux session was created with
/// `cwd == workspace_path`, never `$HOME`; `handler_spawn_wires_provision_and_spawn`
/// asserts provision was called (workspace_path is non-null under workspaces root)
/// and the spawn command was sent to tmux.
pub async fn spawn_session(
    State(state): State<Arc<DaemonState>>,
    Json(req): Json<SpawnRequest>,
) -> impl IntoResponse {
    // #6288: the body lives in `cores::spawn_core` so the socket serves it too.
    cores::spawn_core(&state, req).await
}

/// POST /api/v1/sessions/managed/adopt — adopt an EXISTING tmux session (#1433).
///
/// Why: the operator already has a live, unmanaged tmux pane (a hand-started
/// Claude Code, an externally-created session, or one whose record was lost) and
/// wants to drive it through the full managed surface. This REGISTERS a durable
/// `Active` record for that pane. It is deliberately distinct from the stateless
/// `POST /tmux/adopt` snapshot endpoint, which captures a session's shape but
/// registers NOTHING — that endpoint's semantics are unchanged.
/// What: validates an optional runtime selector up front (400 on a bad value),
/// then delegates to [`crate::session_manager::SessionManager::adopt_existing`].
/// Maps its typed errors: `TmuxSessionMissing` → 404 (no such pane),
/// `AlreadyAdopted` → 409 (already tracked), any other → 500. On success returns
/// 201 Created with the new record + attach command.
/// Test: `adopt_existing_registers_record`, `adopt_existing_missing_is_404`,
/// `adopt_existing_double_is_409` in tests/session_manager_mvp.rs.
pub async fn adopt_existing_session(
    State(state): State<Arc<DaemonState>>,
    Json(req): Json<AdoptExistingRequest>,
) -> impl IntoResponse {
    cores::adopt_core(&state, req).await // #6288
}

/// GET /api/v1/sessions/managed — list all managed sessions.
///
/// Why: the calling agentic process polls this to see all running sessions,
/// their state, and pending decisions. Since issue #3034, this is also the
/// SOLE source of the stable `tm ls` slot numbers every by-number CLI surface
/// (the picker, `d<N>` delete) resolves against — see
/// `SessionManager::numbered_snapshot`'s doc for why the assignment must live
/// here rather than being recomputed client-side per fetch.
/// What: returns one summary per stable slot (1..=highest ever assigned),
/// live or tombstoned. When the optional `?source_id=<id>` query parameter is
/// present, only rows whose LAST-KNOWN `source_id` matches exactly are
/// returned (a tombstoned slot still carries its last-known `source_id`, so
/// deleting a project's session does not make its slot vanish from that
/// project's filtered view). Each summary's `unresumable` flag (#2595) is
/// computed here — via `session_manager::resume_workdir::is_unresumable`,
/// which short-circuits to `false` without any I/O for every state other than
/// `Stopped`/`Errored` — so every picker/list surface reading this endpoint
/// sees dead sessions flagged up front rather than discovering them via a
/// failed resume. The per-session probes run CONCURRENTLY via
/// [`summary::checked_summaries`] (#2595 review, MEDIUM finding 4) rather than
/// one-at-a-time, so a large fleet's response latency does not scale with its
/// count of stopped/errored sessions. Numbering is observed against the FULL,
/// unfiltered record set BEFORE the `source_id` filter is applied — otherwise
/// a session outside the current filter would go unobserved and receive a
/// fresh number the next time it IS listed. The optional `?slim=true` (or
/// `slim=1`) query parameter (#4335, folds into #4322) skips the expensive
/// per-session `stale_assets` probe for callers that never read that flag —
/// every `stale_assets` then reports its `false` default, meaning "not
/// computed", so only such a caller may pass it. Anything else (absent,
/// `slim=false`, malformed) keeps the full probe.
/// Test: list handler test; list-with-source-id-filter test;
/// `checked_summaries_slim_skips_stale_assets_probe`;
/// `list_marks_dead_stopped_session_unresumable`,
/// `list_leaves_live_and_healthy_stopped_sessions_unmarked`,
/// `list_assigns_stable_slot_numbers_and_tombstones_deleted_one` (integration
/// test, `tests/session_manager_slots.rs`).
pub async fn list_managed_sessions(
    State(state): State<Arc<DaemonState>>,
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    // #4335: `?slim=true` opts OUT of the expensive per-session `stale_assets`
    // probe. Absent/malformed keeps the full probe, so no existing client
    // changes shape. #6288: the body itself lives in `cores::list_core`.
    let slim = q.get("slim").is_some_and(|v| v == "true" || v == "1");
    cores::list_core(&state, q.get("source_id").map(String::as_str), slim).await
}

/// GET /api/v1/sessions/managed/{id} — get one session record.
///
/// Why: the calling agentic process needs the full record for a specific session
/// including workspace_path, repo_url, branch, and pending decision fields.
/// What: looks up the session by id and returns its summary, with `unresumable`
/// (#2595) computed the same way [`list_managed_sessions`] does — kept
/// consistent so `tm session info <id>` never disagrees with the list/picker
/// about whether a session is a dead pick.
/// Test: get handler test.
pub async fn get_managed_session(
    State(state): State<Arc<DaemonState>>,
    AxumPath(id_str): AxumPath<String>,
) -> impl IntoResponse {
    cores::get_core(&state, &id_str).await // #6288
}

/// POST /api/v1/sessions/managed/{id}/send — inject text into pane.
///
/// Why: the calling agentic process or human operator sends messages to the
/// harness without needing to attach to the tmux pane.
/// What: delegates to SessionManager::send_input.
/// Test: send handler test.
pub async fn send_to_session(
    State(state): State<Arc<DaemonState>>,
    AxumPath(id_str): AxumPath<String>,
    Json(req): Json<SendInputRequest>,
) -> impl IntoResponse {
    cores::send_core(&state, &id_str, req).await // #6288
}

/// POST /api/v1/sessions/managed/{id}/answer — inject answer to pending decision.
///
/// Why: the calling agentic process resolves a pending decision by posting the
/// accepted or overridden answer; the substrate clears pending_decision.
/// What: delegates to SessionManager::answer_decision.
/// Test: answer handler test.
pub async fn answer_session_decision(
    State(state): State<Arc<DaemonState>>,
    AxumPath(id_str): AxumPath<String>,
    Json(req): Json<AnswerRequest>,
) -> impl IntoResponse {
    cores::answer_core(&state, &id_str, req).await // #6288
}

/// GET /api/v1/sessions/managed/{id}/attach-cmd — return tmux attach command.
///
/// Why: the calling agentic process or operator needs the exact string to attach
/// without hardcoding the naming convention.
/// What: returns "tmux attach-session -t <tmux_name>".
/// Test: attach-cmd handler test.
pub async fn get_attach_cmd(
    State(state): State<Arc<DaemonState>>,
    AxumPath(id_str): AxumPath<String>,
) -> impl IntoResponse {
    cores::attach_cmd_core(&state, &id_str).await // #6288
}

/// POST /api/v1/sessions/managed/{id}/runtime-stop — stop the runtime only (keep workspace).
///
/// Why: a session ENDURES beyond its running runtime; `runtime-stop` kills the
/// tmux session and claude process but preserves the workspace directory and
/// record so the session can be resumed later.
/// What: delegates to SessionManager::stop; returns the updated record summary.
/// Test: stop handler test; `manager_stop_keeps_workspace`.
pub async fn stop_managed_session_runtime(
    State(state): State<Arc<DaemonState>>,
    AxumPath(id_str): AxumPath<String>,
) -> impl IntoResponse {
    cores::runtime_stop_core(&state, &id_str).await // #6288
}

/// POST /api/v1/sessions/managed/{id}/resume — re-spawn the runtime in the existing workspace.
///
/// Why: after `stop`, the workspace is still on disk; `resume` brings back the
/// runtime without re-cloning by creating a fresh tmux session with
/// cwd = workspace_path and spawning claude inside it.
/// What: delegates to SessionManager::resume, then re-spawns the SAME runtime
/// backend the session was created with (via `build_adapter(record.runtime, …)`)
/// on the fresh tmux session — a tcode session resumes on tcode, not claude-code.
/// Test: `manager_resume_respawns_in_existing_workspace`.
pub async fn resume_managed_session(
    State(state): State<Arc<DaemonState>>,
    AxumPath(id_str): AxumPath<String>,
) -> impl IntoResponse {
    // #6288: `resume_http_response` re-attaches the `x-trusty-resume-reason`
    // header the 422 refusals carry; the socket reads the same distinction off
    // the outcome's rpc code.
    cores::resume_http_response(cores::resume_core(&state, &id_str).await)
}

/// Query parameters for POST /api/v1/sessions/managed/{id}/decommission
/// (owner request 2026-07-29, critic HIGH finding #1).
///
/// Why: the `tm ls`/bare `tm` auto-prune sweep must be structurally incapable
/// of removing filesystem content — a remount between its listing-time probe
/// and this call could otherwise reach a real `remove_dir_all`. Rather than
/// trust every future caller to re-check, the route itself routes to a
/// removal-free teardown when asked.
/// What: `record_only` (default `false`, `#[serde(default)]` keeps every
/// existing caller's plain POST unaffected) selects
/// [`SessionManager::decommission_record_only`](crate::session_manager::SessionManager::decommission_record_only) instead of
/// [`SessionManager::decommission`](crate::session_manager::decommission).
/// Test: `decommission_record_only_never_removes_existing_workspace`
/// (`session_manager::tests`).
#[derive(Debug, serde::Deserialize)]
pub struct DecommissionQuery {
    #[serde(default)]
    pub record_only: bool,
}

/// POST /api/v1/sessions/managed/{id}/decommission — full teardown.
///
/// Why: the ONLY operation that removes the workspace from disk. Unlike `stop`,
/// decommission is terminal — no further `resume` is possible.
/// What: delegates to SessionManager::decommission (kills runtime, removes
/// workspace dir when owned, marks record Decommissioned) — or, when
/// `?record_only=true`, to [`SessionManager::decommission_record_only`](crate::session_manager::SessionManager::decommission_record_only)
/// (never touches disk). Returns a [`DecommissionResponse`] that includes
/// `workspace_removed` so callers can display an honest message reflecting
/// whether the filesystem was actually mutated (e.g. adopted/local-path
/// workspaces are NEVER deleted).
/// Test: `manager_decommission_removes_workspace`;
/// `decommission_workspace_removed_reflects_ownership`;
/// `decommission_record_only_never_removes_existing_workspace`.
pub async fn decommission_managed_session(
    State(state): State<Arc<DaemonState>>,
    AxumPath(id_str): AxumPath<String>,
    axum::extract::Query(q): axum::extract::Query<DecommissionQuery>,
) -> impl IntoResponse {
    cores::decommission_core(&state, &id_str, q.record_only).await // #6288
}

#[cfg(test)]
mod staleness_bench_tests;
#[cfg(test)]
mod tests;
