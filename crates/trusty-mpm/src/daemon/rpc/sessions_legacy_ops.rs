//! Transport-neutral bodies for the legacy session registry, hook ingestion,
//! and the polled event feeds.
//!
//! Why (#6288 slice 3): every route in the `/sessions*` registry family, the
//! `/hooks` relay, and the two `/events/poll` legs is now reachable two ways —
//! `daemon::api`'s axum handler and [`super::sessions_legacy`]'s JSON-RPC
//! method. Two transports over one route must not become two implementations of
//! it: the moment a fix lands in one copy and not the other, a caller's answer
//! depends on which socket it happened to dial. So the body lives here exactly
//! once and both transports call in. Slice 2's [`super::core_ops`] made the same
//! move for the health/doctor/tmux families; this file copies that shape.
//!
//! What: one function per route, taking `&Arc<DaemonState>` plus the route's own
//! decoded arguments and returning the response type the HTTP handler used to
//! build. Nothing here names an HTTP type — no `Json`, no `StatusCode`, no
//! extractor — and nothing names a JSON-RPC type either. Failures are
//! [`DaemonError`], which both transports already know how to render
//! (`IntoResponse` for HTTP, `From<DaemonError> for RpcError` for the socket).
//!
//! Why these bodies MOVED rather than staying in `api.rs` behind a sibling
//! wrapper: `api.rs` sits on a frozen 1176-SLOC ratchet budget, and fourteen
//! extra wrappers would have pushed it over. The split ships in the PR that
//! forces it, per the SLOC-cap rule.
//!
//! ## Best-effort steps are best-effort on BOTH transports
//!
//! Three routes here do work that is deliberately allowed to fail without
//! failing the call: the tmux kill and managed-store reconcile behind
//! [`remove_session`], the pause-file write behind [`pause_session`], and the
//! tmux `send-keys` behind [`send_command`]. Each is logged and swallowed
//! exactly where it always was — inside this one body — so the socket inherits
//! the HTTP contract rather than a second, laxer one. The failure branches that
//! DO error (an unknown id, a stopped session, an overseer block) error on both
//! transports for the same reason, because there is one `?` to hit.
//!
//! Test: `parity_sessions_list_agrees_across_transports`,
//! `parity_sessions_delete_agrees_across_transports`,
//! `parity_sessions_pause_leaves_the_same_state_on_both_transports`,
//! `parity_hooks_ingest_leaves_the_same_event_log_on_both_transports` — and the
//! rest of the `parity_*` set in `sessions_legacy_tests.rs`, which drives every
//! body here through BOTH transports and compares the answers and the state
//! each left behind.

use std::sync::Arc;

use crate::core::compress::CompressionLevel;
use crate::core::hook::HookEvent;
use crate::core::session::{ControlModel, Session, SessionId, SessionStatus};
use crate::daemon::api::session_start_correlation::{correlate_session_start, handle_session_end};
use crate::daemon::api::types::{
    CommandResponse, DiscoverResponse, EventsResponse, HookAcceptedResponse, OutputResponse,
    PauseResponse, ReapResponse, RegisterSessionResponse, RemoveSessionResponse, ResumeResponse,
    SessionsResponse, SetPidResponse,
};
use crate::daemon::api::{HookPost, RegisterSession, SessionQuery};
use crate::daemon::error::DaemonError;
use crate::daemon::services::{HookDecision, HookService, SessionService, TmuxService};
use crate::daemon::state::DaemonState;

/// Parse a UUID string into a [`SessionId`].
///
/// Why here rather than in `api.rs`: every caller is a body in this file, and
/// the refusal has to be identical on both transports — `InvalidRequest` renders
/// as HTTP 400 and as `CODE_INVALID_PARAMS` on the socket.
///
/// # Errors
///
/// [`DaemonError::InvalidRequest`] when `raw` is not a UUID.
///
/// Test: `parity_get_session_malformed_id_agrees_across_transports`.
pub fn parse_id(raw: &str) -> Result<SessionId, DaemonError> {
    uuid::Uuid::parse_str(raw)
        .map(SessionId)
        .map_err(|_| DaemonError::InvalidRequest(format!("malformed session id: {raw}")))
}

/// Result of applying an optional compression level to captured output.
///
/// Why: the command and output endpoints share the same compress-then-return
/// shape; bundling the text and stats lets one helper produce both.
/// What: the (possibly compressed) text, the byte stats, and the level as a
/// lowercase wire string (`None` when no compression was applied).
/// Test: `apply_compression_off_is_passthrough`, `apply_compression_summarise`.
pub struct CompressedOutput {
    /// The output text after compression (or unchanged when off).
    pub text: String,
    /// Byte counts before and after compression.
    pub stats: crate::core::compress::CompressionStats,
    /// Lowercase wire name of the level applied, or `None` when uncompressed.
    pub level_label: Option<String>,
}

/// Apply an optional compression level to captured pane output.
///
/// Why: [`send_command`] and [`get_output`] both accept an optional compression
/// level; doing the compress-or-passthrough decision once keeps them identical.
/// What: when `level` is `Some`, runs `compress_output` and records the level's
/// lowercase label; when `None`, returns the raw text with empty stats and no
/// label.
/// Test: `apply_compression_off_is_passthrough`, `apply_compression_summarise`.
pub fn apply_compression(level: Option<CompressionLevel>, raw: &str) -> CompressedOutput {
    match level {
        Some(level) => {
            let (text, stats) = crate::core::compress::compress_output(raw, level);
            CompressedOutput {
                text,
                stats,
                level_label: Some(compression_level_label(level)),
            }
        }
        None => CompressedOutput {
            text: raw.to_string(),
            stats: crate::core::compress::CompressionStats::default(),
            level_label: None,
        },
    }
}

/// Lowercase wire name for a [`CompressionLevel`].
///
/// Why: API responses report the applied level as a stable lowercase string,
/// matching the `snake_case` serde representation of the enum.
/// Test: `compress_level_label_matches_serde`.
pub fn compression_level_label(level: CompressionLevel) -> String {
    match level {
        CompressionLevel::Off => "off",
        CompressionLevel::Trim => "trim",
        CompressionLevel::Summarise => "summarise",
        CompressionLevel::Caveman => "caveman",
    }
    .to_string()
}

/// Default trailing-line count for a pane capture.
fn default_output_lines() -> u32 {
    50
}

/// Snapshot of managed sessions, optionally project-scoped (`GET /sessions`,
/// `mpm.sessions.list`).
///
/// Test: `parity_sessions_list_agrees_across_transports`.
pub fn list_sessions(state: &Arc<DaemonState>, query: SessionQuery) -> SessionsResponse {
    let sessions = match query.project {
        Some(path) => state.list_sessions_for_project(&path),
        None => state.list_sessions(),
    };
    SessionsResponse { sessions }
}

/// Register a managed session, optionally spawning it (`POST /sessions`,
/// `mpm.sessions.register`; also `POST /api/v1/sessions/connect`,
/// `mpm.sessions.connect`).
///
/// Why spawn happens BEFORE the registry write: a failed spawn must leave the
/// registry untouched, so a refused call never leaves a half-created session
/// visible to a later list. That ordering is the contract, and it is one
/// ordering for both transports because there is one body.
///
/// # Errors
///
/// [`DaemonError`] when `workdir` was supplied and the tmux spawn failed —
/// `claude` or tmux missing (HTTP 422 / `CODE_UNPROCESSABLE`), or the tmux
/// command itself failing (HTTP 500 / `CODE_INTERNAL_ERROR`).
///
/// Test: `parity_sessions_register_agrees_across_transports`,
/// `register_and_remove_session`.
pub fn register_session(
    state: &Arc<DaemonState>,
    body: RegisterSession,
) -> Result<RegisterSessionResponse, DaemonError> {
    // Derive the tmux name from the project directory (`tm-<folder>`) so the
    // registry name matches the folder-based session the CLI creates. A
    // caller-supplied `name` always wins; otherwise fall back to the UUID name.
    let project_dir = body.project_path.as_deref();
    let mut session = Session::new(
        SessionId::new(),
        body.project.clone(),
        ControlModel::Tmux,
        project_dir,
    );
    session.project_path = body.project_path.clone();
    if let Some(name) = body.name.as_deref().filter(|n| !n.is_empty()) {
        session.tmux_name = name.to_string();
    }
    if let Some(workdir) = body.workdir.as_deref() {
        // Mirror the workdir onto the session record so the dashboard and the
        // reaper see the spawn directory, not the project label. The
        // `Session::workdir` field is the per-session working directory; the
        // `project` field is a label / association.
        session.workdir = workdir.to_string_lossy().into_owned();
    }

    // Spawn mode: create the tmux host and start `claude` *before* the session
    // is registered, so a refusal leaves the registry untouched.
    if let Some(workdir) = body.workdir.as_deref() {
        TmuxService::spawn_claude(&session.tmux_name, workdir)?;
        // The session is now actively running `claude`; mark it Active so the
        // dashboard reflects reality rather than the default `Starting` state.
        session.status = SessionStatus::Active;
    }

    let id = session.id;
    let tmux_name = session.tmux_name.clone();
    state.register_session(session);

    // Discover the `claude` PID inside the registered tmux pane in the
    // background so the reaper can monitor process liveness. This is the
    // daemon-side counterpart of the CLI's post-launch PID capture; it does not
    // block the response, and a failure is logged, never fatal.
    crate::daemon::services::session_service::spawn_pid_capture(
        Arc::clone(state),
        id,
        tmux_name.clone(),
    );

    Ok(RegisterSessionResponse {
        id,
        name: tmux_name,
    })
}

/// One session's detail (`GET /sessions/{id}`, `mpm.sessions.get`).
///
/// # Errors
///
/// [`DaemonError::InvalidRequest`] for a malformed id,
/// [`DaemonError::SessionNotFound`] for an unknown one.
///
/// Test: `parity_get_session_agrees_across_transports`,
/// `parity_get_session_unknown_id_agrees_across_transports`.
pub fn get_session(state: &Arc<DaemonState>, id: &str) -> Result<Session, DaemonError> {
    let session_id = parse_id(id)?;
    state
        .session(session_id)
        .ok_or_else(|| DaemonError::SessionNotFound { id: id.to_string() })
}

/// Deregister a session AND kill its tmux host (`DELETE /sessions/{id}`,
/// `mpm.sessions.delete`).
///
/// Why the teardown is best-effort: the registry removal having succeeded is the
/// contract the caller relies on, and the tmux host may already be gone. The
/// kill and the managed-store reconcile are logged and swallowed — on BOTH
/// transports, because they are swallowed here rather than in either handler.
///
/// # Errors
///
/// [`DaemonError::InvalidRequest`] for a malformed id,
/// [`DaemonError::SessionNotFound`] for an unknown one.
///
/// Test: `parity_sessions_delete_agrees_across_transports`, `full_user_cycle`.
pub async fn remove_session(
    state: &Arc<DaemonState>,
    id: &str,
) -> Result<RemoveSessionResponse, DaemonError> {
    let session = parse_id(id)?;
    let removed = state
        .remove_session(session)
        .ok_or_else(|| DaemonError::SessionNotFound { id: id.to_string() })?;

    let tmux_name = removed.tmux_name.clone();
    TmuxService::kill_best_effort(&tmux_name);
    reconcile_managed_store_on_delete(state, &tmux_name).await;

    Ok(RemoveSessionResponse {
        removed: id.to_string(),
    })
}

/// Decommission any SessionManager record that shares `tmux_name`, best-effort.
///
/// Why: the legacy `DaemonState` registry and the `SessionManager` store are two
/// registries that can both track a session under the same tmux name. A delete
/// against the legacy registry must not leave the managed store pointing at a
/// now-dead tmux host, or the orphan-GC and `ls` would show a phantom (#1454).
/// What: lists the managed store, finds records whose `tmux_name` matches and
/// that are not already terminal (Stopped/Decommissioned), and decommissions
/// each. Every step is best-effort: a store-read or decommission failure is
/// logged to stderr via `tracing` and swallowed so the delete still succeeds.
/// Idempotent — already-terminal records are skipped.
/// Test: exercised by `full_user_cycle`; the managed-store decommission path
/// itself is unit-tested in `session_manager::tests`.
async fn reconcile_managed_store_on_delete(state: &Arc<DaemonState>, tmux_name: &str) {
    let mgr = state.session_manager().await;
    for record in mgr.list().await {
        if record.tmux_name != tmux_name {
            continue;
        }
        if matches!(
            record.state,
            crate::session_manager::ManagedSessionState::Stopped
                | crate::session_manager::ManagedSessionState::Decommissioned
        ) {
            continue;
        }
        if let Err(e) = mgr.decommission(&record.id, None).await {
            tracing::warn!(
                name = %tmux_name,
                "delete reconcile: decommission of managed record failed (may already be gone): {e}"
            );
        }
    }
}

/// Reap registry entries with no live tmux session (`DELETE /sessions/dead`,
/// `mpm.sessions.reap`).
///
/// Test: `parity_sessions_reap_agrees_across_transports`.
pub fn reap_sessions(state: &Arc<DaemonState>) -> ReapResponse {
    let result = SessionService::new(state).reap();
    ReapResponse {
        removed: result.reaped,
        stopped: result.stopped,
    }
}

/// Record the OS-level `claude` process PID (`PATCH /sessions/{id}/pid`,
/// `mpm.sessions.set_pid`).
///
/// # Errors
///
/// [`DaemonError::InvalidRequest`] for a malformed id,
/// [`DaemonError::SessionNotFound`] for an unknown one.
///
/// Test: `parity_sessions_set_pid_agrees_across_transports`,
/// `parity_sessions_set_pid_unknown_agrees_across_transports`.
pub fn set_session_pid(
    state: &Arc<DaemonState>,
    id: &str,
    pid: u32,
) -> Result<SetPidResponse, DaemonError> {
    let session = parse_id(id)?;
    if state.set_session_pid(session, pid) {
        Ok(SetPidResponse {
            session_id: id.to_string(),
            pid,
        })
    } else {
        Err(DaemonError::SessionNotFound { id: id.to_string() })
    }
}

/// Auto-discover Claude Code sessions (`POST /sessions/discover`,
/// `mpm.sessions.discover`).
///
/// Test: `parity_sessions_discover_agrees_across_transports`.
pub async fn discover_sessions(state: &Arc<DaemonState>) -> DiscoverResponse {
    let result = crate::daemon::discovery::discover_all(state).await;
    DiscoverResponse {
        discovered: result.adopted,
        sessions: result.sessions,
        skipped: result.skipped,
    }
}

/// The bounded ring buffer of recent hook events (`GET /events/poll`,
/// `mpm.events.poll`).
///
/// Test: `parity_events_poll_agrees_across_transports`.
pub fn recent_events(state: &Arc<DaemonState>) -> EventsResponse {
    EventsResponse {
        events: state.recent_hook_events(),
    }
}

/// One session's slice of the ring buffer (`GET /sessions/{id}/events/poll`,
/// `mpm.sessions.events_poll`).
///
/// # Errors
///
/// [`DaemonError::InvalidRequest`] for a malformed id.
///
/// Test: `parity_session_events_poll_agrees_across_transports`.
pub fn session_events(state: &Arc<DaemonState>, id: &str) -> Result<EventsResponse, DaemonError> {
    let session = parse_id(id)?;
    Ok(EventsResponse {
        events: state.hook_events_for(session),
    })
}

/// Pause a session, saving its state for resume (`POST /sessions/{id}/pause`,
/// `mpm.sessions.pause`).
///
/// Why the pause-file write does not fail the call: the in-memory status flip is
/// the contract, and a disk failure is logged inside `SessionService::pause`.
/// Both transports inherit that single decision.
///
/// # Errors
///
/// [`DaemonError::SessionNotFound`] when neither the UUID nor the friendly name
/// resolves.
///
/// Test: `parity_sessions_pause_leaves_the_same_state_on_both_transports`,
/// `pause_then_resume_round_trips`.
pub fn pause_session(
    state: &Arc<DaemonState>,
    id: &str,
    summary: Option<String>,
) -> Result<PauseResponse, DaemonError> {
    let result = SessionService::new(state).pause(id, summary)?;
    Ok(PauseResponse {
        paused: true,
        session_id: result.session_id,
        summary: result.summary,
    })
}

/// Resume a previously-paused session (`POST /sessions/{id}/resume`,
/// `mpm.sessions.resume`).
///
/// # Errors
///
/// [`DaemonError::SessionNotFound`] for an unknown session,
/// [`DaemonError::SessionNotActive`] when it is not paused (HTTP 409 /
/// `CODE_CONFLICT`).
///
/// Test: `parity_sessions_resume_leaves_the_same_state_on_both_transports`,
/// `parity_sessions_resume_unpaused_agrees_across_transports`.
pub fn resume_session(state: &Arc<DaemonState>, id: &str) -> Result<ResumeResponse, DaemonError> {
    SessionService::new(state).resume(id)?;
    Ok(ResumeResponse { resumed: true })
}

/// Send a command into a session's tmux pane (`POST /sessions/{id}/command`,
/// `mpm.sessions.command`).
///
/// Why the tmux write is best-effort but the state check is not: a `Stopped`
/// session is refused before anything is sent (`SessionNotActive`, HTTP 409 /
/// `CODE_CONFLICT`), while a tmux failure after that point is logged and the
/// caller still gets whatever the pane held. One body, so one policy.
///
/// # Errors
///
/// [`DaemonError::SessionNotFound`] for an unknown session,
/// [`DaemonError::SessionNotActive`] for a stopped one.
///
/// Test: `parity_sessions_command_leaves_the_same_state_on_both_transports`,
/// `parity_sessions_command_stopped_agrees_across_transports`.
pub async fn send_command(
    state: &Arc<DaemonState>,
    id: &str,
    command: &str,
    compress: Option<CompressionLevel>,
) -> Result<CommandResponse, DaemonError> {
    let session = SessionService::new(state).command_target(id)?;
    TmuxService::send_command(&session, command);

    // Give the pane a moment to render the command's output before capturing.
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let raw = TmuxService::capture(&session, 100);
    let compressed = apply_compression(compress, &raw);

    Ok(CommandResponse {
        sent: true,
        output: compressed.text,
        original_bytes: compressed.stats.original_bytes,
        compressed_bytes: compressed.stats.compressed_bytes,
        compress_level: compressed.level_label,
    })
}

/// Capture the current tmux pane output (`GET /sessions/{id}/output` and its
/// `/pane` alias; `mpm.sessions.output` and `mpm.sessions.pane`).
///
/// # Errors
///
/// [`DaemonError::SessionNotFound`] for an unknown session.
///
/// Test: `parity_sessions_output_agrees_across_transports`,
/// `parity_sessions_pane_matches_output_on_both_transports`.
pub fn get_output(
    state: &Arc<DaemonState>,
    id: &str,
    lines: Option<u32>,
    compress: Option<CompressionLevel>,
) -> Result<OutputResponse, DaemonError> {
    let session = SessionService::new(state).resolve(id)?;
    let lines = lines.unwrap_or_else(default_output_lines);
    let raw = TmuxService::capture(&session, lines);
    let compressed = apply_compression(compress, &raw);
    Ok(OutputResponse {
        output: compressed.text,
        lines,
        original_bytes: compressed.stats.original_bytes,
        compressed_bytes: compressed.stats.compressed_bytes,
        compress_level: compressed.level_label,
    })
}

/// Ingest one Claude Code hook event (`POST /hooks`, `mpm.hooks.ingest`).
///
/// Why this is the write that matters most: it is how a claude session announces
/// itself (a `SessionStart` for an unknown id auto-registers it), how the
/// managed store learns a session ended, and how the overseer gets its veto. All
/// three effects land here, so both transports produce them or neither does.
///
/// # Errors
///
/// [`DaemonError::InvalidRequest`] for a malformed session id, and
/// [`DaemonError::OverseerBlocked`] when the overseer vetoes the event (HTTP 403
/// / `CODE_FORBIDDEN`) — a refusal, never a warning-and-continue.
///
/// Test: `parity_hooks_ingest_leaves_the_same_event_log_on_both_transports`,
/// `parity_hooks_ingest_malformed_id_agrees_across_transports`.
pub async fn ingest_hook(
    state: &Arc<DaemonState>,
    post: HookPost,
) -> Result<HookAcceptedResponse, DaemonError> {
    let session = parse_id(&post.session_id)?;

    // Auto-register on SessionStart if not already known. This is how a claude
    // session connects itself to the daemon: its first hook event registers it
    // using the incoming UUID, so discovery and `POST /sessions` are not the
    // only ways a session enters state. The workdir is left empty here and
    // enriched later by a snapshot or subsequent events.
    if post.event == HookEvent::SessionStart && state.session(session).is_none() {
        let mut new_session = Session::new(session, String::new(), ControlModel::Tmux, None);
        new_session.status = SessionStatus::Active;
        state.register_session(new_session);
        tracing::info!("auto-registered session on SessionStart: {session:?}");
    }

    // #1744: correlate Claude session id → managed session on SessionStart;
    // immediately mark the managed session Stopped on SessionEnd.
    if post.event == HookEvent::SessionStart {
        correlate_session_start(state, &post.session_id, &post.payload).await;
    }
    if post.event == HookEvent::SessionEnd {
        handle_session_end(state, &post.session_id).await;
    }

    // #2621 (code-critic MEDIUM): the idle-park surface+auto-nudge dispatch
    // lives inside `HookService::process` (daemon/services/hook_service.rs).
    match HookService::new(Arc::clone(state)).process(session, post.event, post.payload) {
        HookDecision::Block { reason } => Err(DaemonError::OverseerBlocked { reason }),
        _ => Ok(HookAcceptedResponse {
            accepted: post.event,
        }),
    }
}
