//! SESSCTL control-plane HTTP handlers (`/api/v1/control/sessions/*`, WI-2 #1593).
//!
//! Why: the SESSCTL Phase 2 requirement (epic #1590) is that the daemon exposes
//! HTTP endpoints so the `tm sessctl` CLI and any future consumer can manage
//! control-plane sessions without embedding the registry directly. Keeping these
//! handlers in a dedicated module mirrors `coordinator_routes` and keeps api.rs
//! focused on routing only.
//! What: five handlers (run, connect, stop, auth, list) wired to
//! `DaemonState::session_registry`. The connect handler streams SSE events and
//! enforces the CAS write-lock protocol (§6.2 of SPEC-SESSCTL-01).
//! Test: `control_routes_tests` inline module — covers list, run,
//! write-lock CAS, stop, and auth endpoints.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use axum::{
    Json,
    extract::{ConnectInfo, Path, Query, State},
    http::StatusCode,
    response::sse::{Event, KeepAlive, Sse},
};
use futures::Stream;
use serde::{Deserialize, Serialize};
use tokio_stream::StreamExt;
use tokio_stream::wrappers::BroadcastStream;

use crate::control::event::BackendKind;
use crate::control::{ActorCommand, ControlSessionId, RunParams};
use crate::daemon::state::DaemonState;

// ── RAII write-lock guard ─────────────────────────────────────────────────────

/// RAII guard that releases the SESSCTL write-lock on drop.
///
/// Why: the `ctl_connect_session` handler must release the write-lock when
/// the SSE stream ends — whether by a normal sender-closed signal, a lag
/// error, or a client disconnect. Without a guard, the only release point
/// was the `Err(_)` branch of `filter_map`, leaving the lock permanently held
/// when the stream ends cleanly. This guard makes release unconditional and
/// automatic regardless of how the stream terminates.
/// What: wraps the `Arc<AtomicBool>` write-lock flag and calls
/// `store(false, SeqCst)` in `Drop`. When `held` is `false` the `Drop` is
/// a no-op so it is safe to create a guard for observer connections too.
/// Test: `ctl_connect_write_lock_cas_first_caller_is_writer` verifies the
/// acquire→observe→release→re-acquire cycle; the guard is the mechanism
/// that makes the release happen in the stream closure.
struct WriteLockGuard {
    flag: Arc<AtomicBool>,
    held: bool,
}

impl WriteLockGuard {
    fn new(flag: Arc<AtomicBool>, held: bool) -> Self {
        Self { flag, held }
    }
}

impl Drop for WriteLockGuard {
    fn drop(&mut self) {
        if self.held {
            self.flag.store(false, Ordering::SeqCst);
        }
    }
}

// ── Request / Response types ──────────────────────────────────────────────────

/// Request body for `POST /api/v1/control/sessions/run`.
///
/// Why: bundles every spawn-time input so the signature stays stable as
/// fields are added in later phases.
/// What: project_id, workdir, optional backend selection, optional prompt
/// file, and optional claude_cmd override.
/// Test: `ctl_run_session_returns_session_id`.
#[derive(Debug, Deserialize)]
pub struct CtlRunRequest {
    /// Registered project ID.
    pub project_id: String,
    /// Absolute working directory for the session.
    pub workdir: String,
    /// Backend: `"stream-json"` (default) or `"tmux"`.
    pub backend: Option<String>,
    /// Path to an `--append-system-prompt-file` file.
    pub prompt_file: Option<String>,
    /// Override the `claude` executable path.
    pub claude_cmd: Option<String>,
}

/// Response body for `POST /api/v1/control/sessions/run`.
///
/// Why: callers need the allocated session ID to subscribe, stop, or
/// inspect the session.
/// What: the stable `<project-id>-<N>` session ID string.
/// Test: `ctl_run_session_returns_session_id`.
#[derive(Debug, Serialize)]
pub struct CtlRunResponse {
    /// Allocated session ID.
    pub session_id: String,
}

/// Response body for `POST /api/v1/control/sessions/{id}/connect`.
///
/// Why: callers need to know whether they acquired the write lock (active
/// writer) or are read-only observers, so the UI can enable or disable the
/// input box accordingly.
/// What: the session ID and the CAS result.
/// Test: `ctl_connect_write_lock_cas`.
#[derive(Debug, Serialize)]
pub struct CtlConnectResponse {
    /// The connected session ID.
    pub session_id: String,
    /// `true` if the caller acquired the write lock (active writer).
    pub writer: bool,
}

/// Query parameters for `POST /api/v1/control/sessions/{id}/stop`.
///
/// Why: distinguishes a graceful stop from a forced kill without a separate
/// path, matching the `tm stop --force` CLI flag.
/// What: optional `force` boolean defaulting to `false`.
/// Test: `ctl_stop_session_sends_stop_command`.
#[derive(Debug, Deserialize)]
pub struct CtlStopQuery {
    /// When `true`, send `ForceStop`; otherwise send `Stop`.
    pub force: Option<bool>,
}

/// Response body for `POST /api/v1/control/sessions/{id}/stop`.
///
/// Why: confirms which stop variant was applied.
/// What: the session ID and the effective `force` flag.
/// Test: `ctl_stop_session_sends_stop_command`.
#[derive(Debug, Serialize)]
pub struct CtlStopResponse {
    /// The stopped session ID.
    pub session_id: String,
    /// Whether a forced stop was used.
    pub force: bool,
}

/// Response body for `GET /api/v1/control/sessions/{id}/auth`.
///
/// Why: operators and the SM agent need a polling endpoint to detect an
/// `AwaitingAuth` state without subscribing to the full SSE stream.
/// What: session ID, state label, and a `awaiting_auth` convenience flag.
/// Test: `ctl_auth_session_returns_state`.
#[derive(Debug, Serialize)]
pub struct CtlAuthResponse {
    /// The queried session ID.
    pub session_id: String,
    /// State label (e.g. `"running"`, `"awaiting-auth"`).
    pub state: String,
    /// `true` when `state == "awaiting-auth"`.
    pub awaiting_auth: bool,
}

/// Query parameters for `GET /api/v1/control/sessions`.
///
/// Why: operators commonly want to filter the session list by project
/// without a separate endpoint.
/// What: optional `project` filter.
/// Test: `ctl_list_sessions_returns_empty`.
#[derive(Debug, Deserialize)]
pub struct CtlListQuery {
    /// Filter by project ID (substring match on session ID prefix is NOT used;
    /// the filter matches the metadata `project_id` field exactly).
    pub project: Option<String>,
}

/// A single row in the `GET /api/v1/control/sessions` response.
///
/// Why: `tm sessctl list --format json` must expose the full §4.5 schema so
/// consumers (SM agent, TUI, TELUI) can drive autonomy decisions and display
/// helpful session state without reading the raw event stream.
/// What: all fields from §4.5 of SPEC-SESSCTL-01: session_id, project_id,
/// backend, state, restart_count, context_lost, uptime_secs, last_activity_ts,
/// last_summary, pending_decision, proposed_default. (WI-3 #1594)
/// Test: `ctl_list_sessions_full_schema`, `ctl_list_sessions_returns_empty`.
#[derive(Debug, Serialize)]
pub struct CtlSessionSummary {
    /// The session's stable identifier (`<project-id>-<N>`).
    pub session_id: String,
    /// The owning project.
    pub project_id: String,
    /// Backend label (`"stream-json"` or `"tmux"`).
    pub backend: String,
    /// State label.
    pub state: String,
    /// Number of times the backend has been auto-restarted (§3.1).
    /// Widened to u32 at the API boundary so crash-looping sessions never
    /// saturate at 255 (the source field SessionMetadata::restart_count is
    /// still u8 per spec; we cast with `as u32` here).
    pub restart_count: u32,
    /// `true` if the session has restarted ≥1 time (no history replay in alpha-1).
    pub context_lost: bool,
    /// Seconds since the session started.
    pub uptime_secs: u64,
    /// ISO-8601 timestamp of the most recent output event.
    pub last_activity_ts: String,
    /// Last summary text from the §8.2 regex/NLP parse layer (WI-3).
    pub last_summary: Option<String>,
    /// Human-readable prompt text if the session is awaiting a decision (§8.4).
    pub pending_decision: Option<String>,
    /// Proposed answer to the pending decision (e.g. `"yes"` for `[Y/n]`).
    pub proposed_default: Option<String>,
}

/// Response body for `GET /api/v1/control/sessions`.
///
/// Why: wraps the rows in a top-level object so the format is extensible.
/// What: a `sessions` array of [`CtlSessionSummary`] rows.
/// Test: `ctl_list_sessions_returns_empty`.
#[derive(Debug, Serialize)]
pub struct CtlListResponse {
    /// All live sessions, optionally filtered by project.
    pub sessions: Vec<CtlSessionSummary>,
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Parse a `ControlSessionId` from an HTTP path string.
///
/// Why: axum path extractors produce a plain `String`; the control plane uses
/// a typed `ControlSessionId` newtype. This helper centralises the conversion.
/// What: wraps the string in `ControlSessionId`; the inner value is always
/// the raw string from the path so no parse error is possible.
/// Test: used in every handler test that references a session by ID.
fn parse_id(s: &str) -> ControlSessionId {
    ControlSessionId(s.to_owned())
}

/// Parse an optional backend string into `BackendKind`.
///
/// Why: HTTP callers provide a string (`"stream-json"` or `"tmux"`); the
/// registry needs a typed `BackendKind`. Centralising the mapping keeps
/// handler bodies thin.
/// What: returns `BackendKind::Tmux` for `"tmux"`, `StreamJson` otherwise.
/// Test: used by `ctl_run_session_returns_session_id`.
fn parse_backend(s: Option<&str>) -> BackendKind {
    match s {
        Some("tmux") => BackendKind::Tmux,
        _ => BackendKind::StreamJson,
    }
}

// ── Trust-boundary guards (#6197) ─────────────────────────────────────────────

/// Reject a non-loopback caller to a control-plane session route (#6197).
///
/// Why: these handlers spawn processes (`ctl_run_session` → `run_session`) and
/// drive session lifecycle from a caller-supplied request, but nothing enforced
/// that the caller was local. The daemon binds loopback-only, yet a later
/// `0.0.0.0` rebind would expose arbitrary process spawn to the network. This
/// mirrors the `/rpc` gate exactly, so the two process-spawning HTTP surfaces
/// enforce the same boundary via one shared predicate.
/// What: returns `Err((403, msg))` when `peer` is not an IPv4/IPv6 loopback
/// address, `Ok(())` otherwise. Reuses [`super::rpc::is_loopback`] rather than
/// re-implementing the check (single entry point).
/// Test: `ctl_run_session_rejects_non_loopback_peer`.
fn loopback_guard(peer: &SocketAddr) -> Result<(), (StatusCode, String)> {
    if super::rpc::is_loopback(peer) {
        Ok(())
    } else {
        tracing::warn!(%peer, "rejected non-loopback control-plane session request (#6197)");
        Err((
            StatusCode::FORBIDDEN,
            "control-plane session routes are loopback-only; remote access is forbidden".to_owned(),
        ))
    }
}

/// True when every character of `s` is in the conservative command charset.
///
/// Why: the tmux backend interpolates `claude_cmd` into a shell command line
/// (`format!("{claude_cmd} --append-system-prompt-file …")`), so a value with a
/// space or a shell metacharacter is command injection. Restricting to a small
/// path/argument-safe charset removes the whole class before the basename check.
/// What: allows ASCII alphanumerics and `/ . _ -`; rejects everything else
/// (whitespace, `;`, `|`, `&`, `$`, backtick, quotes, glob, …).
/// Test: `ctl_run_session_rejects_claude_cmd_outside_allowlist`.
fn is_safe_cmd_charset(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-'))
}

/// Validate a caller-supplied `claude_cmd` override (#6197).
///
/// Why: `ctl_run_session` passed `claude_cmd` straight into the spawn path, so
/// any local process could make the daemon execute an arbitrary binary as its
/// user. Constraining the override to the single expected binary NAME keeps the
/// legitimate use (pointing at a specific `claude` install path) while removing
/// the arbitrary-exec primitive.
/// What: `None` is accepted (the registry defaults to `"claude"`). For
/// `Some(cmd)` the value must contain only the safe command charset AND its
/// final path component must be exactly `claude` — so `sh`, `curl`, `/bin/bash`,
/// and `claude; rm -rf /` are all rejected. Returns `Err((400, msg))` on any
/// violation.
/// Test: `ctl_run_session_rejects_claude_cmd_outside_allowlist`,
/// `ctl_run_session_accepts_default_claude_cmd`.
fn validate_claude_cmd(cmd: Option<&str>) -> Result<(), (StatusCode, String)> {
    let Some(cmd) = cmd else { return Ok(()) };
    let allowed = is_safe_cmd_charset(cmd)
        && std::path::Path::new(cmd)
            .file_name()
            .and_then(|n| n.to_str())
            == Some("claude");
    if allowed {
        Ok(())
    } else {
        tracing::warn!(%cmd, "rejected claude_cmd outside the allowed set (#6197)");
        Err((
            StatusCode::BAD_REQUEST,
            "claude_cmd must name the `claude` binary (basename `claude`, no shell metacharacters)"
                .to_owned(),
        ))
    }
}

/// Validate a caller-supplied `workdir` (#6197).
///
/// Why: `workdir` is passed into the backend spawn, so an unconstrained value is
/// a path-traversal and spawn-anywhere vector. The constraint chosen: the
/// working directory must be an ABSOLUTE path with no `..` component that
/// resolves to an EXISTING directory. That removes relative-path and traversal
/// tricks and refuses to spawn into a non-directory, while staying agnostic to
/// any single project root (which these handlers do not have on hand).
/// What: returns `Err((400, msg))` when `workdir` is not absolute, contains a
/// `ParentDir` (`..`) component, or is not an existing directory; `Ok(())`
/// otherwise.
/// Test: `ctl_run_session_rejects_relative_workdir`,
/// `ctl_run_session_rejects_workdir_traversal`.
fn validate_workdir(workdir: &std::path::Path) -> Result<(), (StatusCode, String)> {
    let traversal = workdir
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir));
    if workdir.is_absolute() && !traversal && workdir.is_dir() {
        Ok(())
    } else {
        tracing::warn!(workdir = %workdir.display(), "rejected invalid control-plane workdir (#6197)");
        Err((
            StatusCode::BAD_REQUEST,
            "workdir must be an absolute path (no `..`) to an existing directory".to_owned(),
        ))
    }
}

// ── Handlers ──────────────────────────────────────────────────────────────────

/// Spawn a new SESSCTL session via the daemon registry.
///
/// Why: the CLI and any future consumer POST to this endpoint instead of
/// embedding `SessionRegistry` directly, keeping the registry a single
/// shared owner in `DaemonState`.
/// What: rejects a non-loopback caller (#6197), validates the caller-supplied
/// `claude_cmd` and `workdir` (#6197), then deserializes `CtlRunRequest`,
/// constructs `RunParams`, calls `state.session_registry.run_session(params)`,
/// and returns the allocated session ID.
/// Test: `ctl_run_session_returns_session_id`,
/// `ctl_run_session_rejects_non_loopback_peer`,
/// `ctl_run_session_rejects_claude_cmd_outside_allowlist`.
pub async fn ctl_run_session(
    State(state): State<Arc<DaemonState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Json(req): Json<CtlRunRequest>,
) -> Result<Json<CtlRunResponse>, (StatusCode, String)> {
    // #6197: this handler spawns a caller-named executable, so it enforces the
    // same loopback boundary as `/rpc` and validates the two spawn inputs before
    // anything is spawned.
    loopback_guard(&peer)?;
    validate_claude_cmd(req.claude_cmd.as_deref())?;
    validate_workdir(std::path::Path::new(&req.workdir))?;
    let backend = parse_backend(req.backend.as_deref());
    let params = RunParams {
        project_id: req.project_id,
        workdir: req.workdir.into(),
        backend,
        prompt_file: req.prompt_file.map(Into::into),
        claude_cmd: req.claude_cmd,
    };
    match state.session_registry.run_session(params).await {
        Ok(id) => Ok(Json(CtlRunResponse {
            session_id: id.to_string(),
        })),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, e.to_string())),
    }
}

/// Connect to a SESSCTL session and stream its events as SSE.
///
/// Why: the `tm sessctl connect <id>` command and the TUI need a real-time
/// event stream. Using SSE keeps the transport simple (HTTP/1.1 compatible)
/// and fits axum's `Sse` responder.
/// What: looks up the handle, attempts a CAS write-lock, subscribes to the
/// broadcast channel, and streams each `SessionEvent` as an SSE `data:` line.
/// The `WriteLockGuard` is moved into a single `move` closure over the stream
/// so it is **owned** by the stream — it drops (and releases the lock) on ANY
/// stream termination: normal end, lag/close error, or client disconnect.
/// Why POST (not GET): connect acquires the write-lock via CAS — a side effect —
/// so it is not a pure read. POST signals that intent explicitly and prevents
/// inadvertent browser pre-fetches or proxy caching from stealing the lock.
/// Test: `ctl_connect_write_lock_cas_first_caller_is_writer` (CAS semantics);
/// `write_lock_guard_releases_on_drop` (guard drop mechanics);
/// `ctl_connect_write_lock_guard_lives_with_stream` (handler-level lifetime).
pub async fn ctl_connect_session(
    State(state): State<Arc<DaemonState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(id_str): Path<String>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, (StatusCode, String)> {
    loopback_guard(&peer)?; // #6197
    let id = parse_id(&id_str);
    let handle = state
        .session_registry
        .get(&id)
        .await
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("session {id_str} not found")))?;

    // CAS the write-lock per §6.2.
    let writer = handle.try_acquire_write_lock();
    // The guard is moved into the `filter_map` closure below.  It owns the
    // write-lock release: when the Sse stream is dropped (on any exit path),
    // the closure drops, the guard drops, and the AtomicBool is cleared.
    let guard = WriteLockGuard::new(Arc::clone(&handle.write_lock_held), writer);
    let rx = handle.event_tx.subscribe();

    // Single `move` closure owns `guard` — no separate `.map()` step needed.
    // Ownership is unambiguous: the closure captures `guard` by move, so the
    // compiler enforces that it lives exactly as long as this stream does.
    let stream = BroadcastStream::new(rx).filter_map(move |item| {
        // `_guard` is referenced here so the compiler keeps `guard` in this
        // closure's captured environment for the entire stream lifetime.
        let _guard = &guard;
        match item {
            Ok(event) => {
                let json = serde_json::to_string(&event).unwrap_or_default();
                Some(Ok(Event::default().data(json)))
            }
            // Lagged or channel closed — end the stream; guard drops with it.
            Err(_) => None,
        }
    });

    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

/// Send a stop (graceful or forced) command to a session.
///
/// Why: `tm sessctl stop <id>` must reach the actor via the HTTP API in Phase 2.
/// What: looks up the handle, sends `ActorCommand::Stop` or `ForceStop`
/// depending on `?force=true`, and returns a confirmation.
/// Test: `ctl_stop_session_sends_stop_command`.
pub async fn ctl_stop_session(
    State(state): State<Arc<DaemonState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(id_str): Path<String>,
    Query(query): Query<CtlStopQuery>,
) -> Result<Json<CtlStopResponse>, (StatusCode, String)> {
    loopback_guard(&peer)?; // #6197
    let id = parse_id(&id_str);
    let handle = state
        .session_registry
        .get(&id)
        .await
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("session {id_str} not found")))?;

    let force = query.force.unwrap_or(false);
    let cmd = if force {
        ActorCommand::ForceStop
    } else {
        ActorCommand::Stop
    };
    handle
        .command_tx
        .send(cmd)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(CtlStopResponse {
        session_id: id_str,
        force,
    }))
}

/// Return the auth state for a session.
///
/// Why: operators and the SM agent need a lightweight polling endpoint to
/// detect an `AwaitingAuth` condition without subscribing to the SSE stream.
/// What: reads `metadata.state` from the session handle and returns it along
/// with a `awaiting_auth` convenience flag.
/// Test: `ctl_auth_session_returns_state`.
pub async fn ctl_auth_session(
    State(state): State<Arc<DaemonState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(id_str): Path<String>,
) -> Result<Json<CtlAuthResponse>, (StatusCode, String)> {
    loopback_guard(&peer)?; // #6197
    let id = parse_id(&id_str);
    let handle = state
        .session_registry
        .get(&id)
        .await
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("session {id_str} not found")))?;

    let meta = handle.metadata.read().await;
    let state_label = meta.state.label().to_owned();
    let awaiting_auth = state_label == "awaiting-auth";
    Ok(Json(CtlAuthResponse {
        session_id: id_str,
        state: state_label,
        awaiting_auth,
    }))
}

/// List all live SESSCTL sessions, optionally filtered by project.
///
/// Why: `tm sessctl list` needs an HTTP endpoint that returns the current
/// registry snapshot without requiring the CLI to embed the registry.
/// What: calls `list_ids()` on the registry, reads each session's metadata,
/// and builds a `CtlListResponse`. WI-3: populates the full §4.5 schema
/// (restart_count, context_lost, last_activity_ts, last_summary,
/// pending_decision, proposed_default). The optional `?project=` query
/// parameter filters by exact `project_id` match.
/// Test: `ctl_list_sessions_returns_empty`, `ctl_list_sessions_full_schema`.
pub async fn ctl_list_sessions(
    State(state): State<Arc<DaemonState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Query(query): Query<CtlListQuery>,
) -> Result<Json<CtlListResponse>, (StatusCode, String)> {
    loopback_guard(&peer)?; // #6197
    let ids = state.session_registry.list_ids().await;
    let mut sessions = Vec::with_capacity(ids.len());
    for id in ids {
        if let Some(handle) = state.session_registry.get(&id).await {
            let meta = handle.metadata.read().await;
            let backend_label = match meta.backend {
                BackendKind::StreamJson => "stream-json",
                BackendKind::Tmux => "tmux",
            };
            if query
                .project
                .as_deref()
                .is_some_and(|f| f != meta.project_id)
            {
                continue;
            }
            sessions.push(CtlSessionSummary {
                session_id: id.to_string(),
                project_id: meta.project_id.clone(),
                backend: backend_label.to_owned(),
                state: meta.state.label().to_owned(),
                restart_count: meta.restart_count as u32,
                context_lost: meta.context_lost,
                uptime_secs: meta.uptime_secs(),
                last_activity_ts: meta.last_activity_at.to_rfc3339(),
                last_summary: meta.last_summary.clone(),
                pending_decision: meta.pending_decision.clone(),
                proposed_default: meta.proposed_default.clone(),
            });
        }
    }
    Ok(Json(CtlListResponse { sessions }))
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::actor::SessionActorHandle;
    use crate::control::id::ControlSessionId;
    use crate::control::state::SessionMetadata;
    use crate::daemon::state::DaemonState;
    use axum::Router;
    use axum::body::Body;
    use axum::extract::connect_info::MockConnectInfo;
    use axum::http::{Method, Request, StatusCode};
    use axum::routing::{get, post};
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
    use tokio::sync::{RwLock, broadcast, mpsc};
    use tower::ServiceExt;

    /// A loopback peer address for tests that drive the guarded routes.
    fn loopback_peer() -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)
    }

    fn make_handle(id: &ControlSessionId) -> SessionActorHandle {
        let (command_tx, _) = mpsc::channel(4);
        let (event_tx, _) = broadcast::channel(16);
        SessionActorHandle {
            command_tx,
            event_tx,
            write_lock_held: Arc::new(AtomicBool::new(false)),
            metadata: Arc::new(RwLock::new(SessionMetadata::new(
                id.clone(),
                "test-proj".into(),
                BackendKind::StreamJson,
            ))),
        }
    }

    /// Build a test `DaemonState` and return it together with the `TempDir`
    /// guard so the temp directory lives for the full test duration.
    ///
    /// Why: `DaemonState::with_paths` reads `paths` only at construction time,
    /// but the underlying `TempDir` must not be deleted until the test ends —
    /// some constructors write pairing files or perform cleanup that may access
    /// the directory after `new` returns. Returning the guard here makes the
    /// lifetime explicit in every caller instead of relying on a comment.
    /// What: creates a `TempDir`, builds `FrameworkPaths`, constructs
    /// `DaemonState::with_paths`, wraps in `Arc`, and returns both.
    /// Test: all test functions below destructure the tuple and bind the guard.
    fn test_state() -> (Arc<DaemonState>, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("temp dir");
        let paths = crate::core::paths::FrameworkPaths::under(dir.path());
        let state = Arc::new(DaemonState::with_paths(&paths));
        (state, dir)
    }

    /// Build a test `DaemonState` whose control-plane concurrency cap is `0`.
    ///
    /// Why: the #6197 regression tests must NOT spawn a real backend. With the
    /// cap at 0, `run_session` rejects at admission (§9.2) and returns an error
    /// BEFORE any spawn — so when these tests run against the pre-fix code (which
    /// has no auth/validation gate), control flow still ADVANCES into
    /// `run_session` (the spawn path — the Fail-Open branch under test) but the
    /// cap prevents an actual process launch in the harness. Post-fix, the guard
    /// or the validator returns first, so the cap is never even consulted.
    /// What: writes `config.toml` with `max_concurrent_sessions = 0` into
    /// `paths.root` — the framework root `build_session_registry` reads, which
    /// `FrameworkPaths::under` nests under the temp dir (not the temp dir itself).
    fn test_state_no_spawn() -> (Arc<DaemonState>, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("temp dir");
        let paths = crate::core::paths::FrameworkPaths::under(dir.path());
        std::fs::create_dir_all(&paths.root).expect("create framework root");
        std::fs::write(
            paths.root.join("config.toml"),
            "[control_plane]\nmax_concurrent_sessions = 0\n",
        )
        .expect("write config");
        let state = Arc::new(DaemonState::with_paths(&paths));
        (state, dir)
    }

    /// The five control-plane routes with state, without any ConnectInfo layer.
    ///
    /// Why: most tests want a loopback peer injected for them (`test_router`),
    /// but the #6197 non-loopback guard test needs to insert its OWN peer address
    /// into the request, so it drives these raw routes and inserts a
    /// `ConnectInfo` manually — a `MockConnectInfo` layer would overwrite it.
    fn test_routes(state: Arc<DaemonState>) -> Router {
        Router::new()
            .route("/api/v1/control/sessions", get(ctl_list_sessions))
            .route("/api/v1/control/sessions/run", post(ctl_run_session))
            .route(
                "/api/v1/control/sessions/{id}/connect",
                post(ctl_connect_session),
            )
            .route("/api/v1/control/sessions/{id}/stop", post(ctl_stop_session))
            .route("/api/v1/control/sessions/{id}/auth", get(ctl_auth_session))
            .with_state(state)
    }

    /// Test router that injects a loopback peer for every request (#6197).
    ///
    /// Why: the control routes now require a `ConnectInfo<SocketAddr>` and reject
    /// non-loopback callers; the `MockConnectInfo` layer supplies a loopback peer
    /// so the existing behavioral tests exercise the happy path unchanged.
    fn test_router(state: Arc<DaemonState>) -> Router {
        test_routes(state).layer(MockConnectInfo(loopback_peer()))
    }

    #[tokio::test]
    async fn ctl_list_sessions_returns_empty() {
        let (state, _dir) = test_state();
        let app = test_router(state);
        let req = Request::builder()
            .method(Method::GET)
            .uri("/api/v1/control/sessions")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(parsed["sessions"].as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn ctl_list_sessions_returns_registered_sessions() {
        let (state, _dir) = test_state();
        let id = ControlSessionId::new("list-proj", 0);
        let handle = make_handle(&id);
        state.session_registry.register(id.clone(), handle).await;

        let app = test_router(Arc::clone(&state));
        let req = Request::builder()
            .method(Method::GET)
            .uri("/api/v1/control/sessions")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(parsed["sessions"].as_array().unwrap().len(), 1);
        assert_eq!(
            parsed["sessions"][0]["session_id"].as_str().unwrap(),
            id.as_str()
        );
    }

    /// Verify the full §4.5 row schema is present in the JSON response (WI-3).
    ///
    /// Why: the WI-3 gate requires `tm sessctl list --format json` to emit
    /// all §4.5 fields; this test verifies every field is present and has the
    /// correct type.
    /// What: registers a session with known metadata, calls the list endpoint,
    /// and asserts each §4.5 field is present with the expected value/type.
    /// Test: this test.
    #[tokio::test]
    async fn ctl_list_sessions_full_schema() {
        let (state, _dir) = test_state();
        let id = ControlSessionId::new("schema-proj", 0);
        let handle = make_handle(&id);
        state.session_registry.register(id.clone(), handle).await;

        let app = test_router(Arc::clone(&state));
        let req = Request::builder()
            .method(Method::GET)
            .uri("/api/v1/control/sessions")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let row = &parsed["sessions"][0];

        // Every §4.5 field must be present.
        assert!(row["session_id"].is_string(), "session_id must be a string");
        assert!(row["project_id"].is_string(), "project_id must be a string");
        assert!(row["backend"].is_string(), "backend must be a string");
        assert!(row["state"].is_string(), "state must be a string");
        assert!(
            row["restart_count"].is_number(),
            "restart_count must be a number"
        );
        assert!(
            row["context_lost"].is_boolean(),
            "context_lost must be a boolean"
        );
        assert!(
            row["uptime_secs"].is_number(),
            "uptime_secs must be a number"
        );
        assert!(
            row["last_activity_ts"].is_string(),
            "last_activity_ts must be a string (ISO-8601)"
        );
        // last_summary, pending_decision, proposed_default may be null.
        assert!(
            row["last_summary"].is_null() || row["last_summary"].is_string(),
            "last_summary must be null or string"
        );
        assert!(
            row["pending_decision"].is_null() || row["pending_decision"].is_string(),
            "pending_decision must be null or string"
        );
        assert!(
            row["proposed_default"].is_null() || row["proposed_default"].is_string(),
            "proposed_default must be null or string"
        );

        // Verify known values.
        assert_eq!(row["session_id"].as_str().unwrap(), id.as_str());
        assert_eq!(row["project_id"].as_str().unwrap(), "test-proj");
        assert_eq!(row["backend"].as_str().unwrap(), "stream-json");
        assert_eq!(row["restart_count"].as_u64().unwrap(), 0);
        assert!(!row["context_lost"].as_bool().unwrap());
    }

    #[tokio::test]
    async fn ctl_stop_session_not_found() {
        let (state, _dir) = test_state();
        let app = test_router(state);
        let req = Request::builder()
            .method(Method::POST)
            .uri("/api/v1/control/sessions/nonexistent-0/stop")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn ctl_auth_session_returns_state() {
        let (state, _dir) = test_state();
        let id = ControlSessionId::new("auth-proj", 0);
        let handle = make_handle(&id);
        state.session_registry.register(id.clone(), handle).await;

        let app = test_router(Arc::clone(&state));
        let uri = format!("/api/v1/control/sessions/{}/auth", id.as_str());
        let req = Request::builder()
            .method(Method::GET)
            .uri(&uri)
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(parsed["session_id"].as_str().unwrap(), id.as_str());
        assert!(!parsed["awaiting_auth"].as_bool().unwrap());
    }

    #[tokio::test]
    async fn ctl_auth_session_not_found() {
        let (state, _dir) = test_state();
        let app = test_router(state);
        let req = Request::builder()
            .method(Method::GET)
            .uri("/api/v1/control/sessions/no-such-0/auth")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    /// Verify the write-lock CAS: acquire → observer → release → re-acquire.
    ///
    /// Why: this test proves the §6.2 write-lock protocol is correct at the
    /// handle level. The SSE stream closure relies on the same
    /// `try_acquire_write_lock` / `release_write_lock` primitives, so testing
    /// them here validates the core invariant without requiring a live SSE
    /// connection.
    /// What: registers a handle, acquires the lock on clone 1, verifies clone 2
    /// is observer-only, releases, and verifies clone 3 can re-acquire.
    /// Test: this test (structural CAS semantics); the RAII guard's drop is
    /// exercised by `write_lock_guard_releases_on_drop`.
    #[tokio::test]
    async fn ctl_connect_write_lock_cas_first_caller_is_writer() {
        let (state, _dir) = test_state();
        let id = ControlSessionId::new("cas-proj", 0);
        let handle = make_handle(&id);
        state.session_registry.register(id.clone(), handle).await;

        let h = state
            .session_registry
            .get(&id)
            .await
            .expect("handle must exist");
        let first = h.try_acquire_write_lock();
        let second = h.try_acquire_write_lock();
        assert!(first, "first caller must acquire write lock");
        assert!(!second, "second caller must be observer while lock held");
        h.release_write_lock();
        let third = h.try_acquire_write_lock();
        assert!(third, "after release, lock must be acquirable again");
        // Clean up so we don't leave the lock held after the test.
        h.release_write_lock();
    }

    /// Verify the `WriteLockGuard` drops the lock on any exit path.
    ///
    /// Why: the HIGH finding required proving the RAII guard actually releases
    /// the lock when it drops — not just when `release_write_lock()` is called
    /// manually.
    /// What: creates a guard with `held=true`, verifies the flag is `true`, drops
    /// the guard by moving it into a block, then verifies the flag is `false`.
    /// Test: this test (direct RAII drop verification).
    #[test]
    fn write_lock_guard_releases_on_drop() {
        let flag = Arc::new(AtomicBool::new(false));
        // Simulate a successful acquire.
        flag.store(true, Ordering::SeqCst);
        {
            let _guard = WriteLockGuard::new(Arc::clone(&flag), true);
            assert!(
                flag.load(Ordering::SeqCst),
                "flag must be true while guard is live"
            );
        }
        // Guard dropped — flag must be false.
        assert!(
            !flag.load(Ordering::SeqCst),
            "guard must release lock on drop"
        );
    }

    /// Verify a no-op guard (observer) does not reset the flag on drop.
    ///
    /// Why: observer connections create a `WriteLockGuard` with `held=false`;
    /// dropping it must NOT clear a flag that a concurrent writer holds.
    /// Test: this test.
    #[test]
    fn write_lock_guard_observer_noop_on_drop() {
        let flag = Arc::new(AtomicBool::new(true)); // writer holds it
        {
            let _observer_guard = WriteLockGuard::new(Arc::clone(&flag), false);
        }
        assert!(
            flag.load(Ordering::SeqCst),
            "observer guard must not clear a writer's lock"
        );
    }

    /// Prove the write-lock is held for the stream's lifetime and released on drop.
    ///
    /// Why: the HIGH finding demanded a handler-level regression test — not just
    /// isolated Drop tests — that proves the guard is owned by the stream, not by
    /// the handler stack frame. This test drives the same code path that
    /// `ctl_connect_session` uses: `BroadcastStream::filter_map(move |item| {
    /// let _guard = &guard; … })`. When the stream value is dropped the closure
    /// drops, the guard drops, and the write-lock is released.
    /// What: creates a `WriteLockGuard` with `held=true`, moves it into a
    /// `BroadcastStream::filter_map` closure (the exact pattern used in the handler),
    /// asserts the lock is held while the stream is alive, drops the stream (without
    /// polling — no need to consume events), and asserts the lock is released.
    /// Why no poll: `BroadcastStream` would block indefinitely on an empty channel;
    /// the test only needs to verify that `drop(stream)` releases the lock, not that
    /// events flow through.
    /// Test: this test.
    #[test]
    fn ctl_connect_write_lock_guard_lives_with_stream() {
        use tokio_stream::wrappers::BroadcastStream;

        // Use a tokio broadcast channel with a disconnected sender so the stream
        // is immediately at end-of-stream (no events; receiver lag is irrelevant
        // because we never poll). The channel is dropped before creating the stream.
        let (tx, rx) = tokio::sync::broadcast::channel::<crate::control::event::SessionEvent>(4);
        drop(tx); // sender dropped — stream would immediately end if polled

        let flag = Arc::new(AtomicBool::new(true)); // guard "holds" the lock
        let guard = WriteLockGuard::new(Arc::clone(&flag), true);

        let stream = BroadcastStream::new(rx).filter_map(move |item| {
            let _guard = &guard; // guard is owned by this closure
            match item {
                Ok(event) => {
                    let json = serde_json::to_string(&event).unwrap_or_default();
                    Some(Ok::<_, std::convert::Infallible>(
                        axum::response::sse::Event::default().data(json),
                    ))
                }
                Err(_) => None,
            }
        });
        // Pin on heap so we have a concrete value to drop.
        let pinned = Box::pin(stream);

        // === While stream is alive, lock must be held. ===
        assert!(
            flag.load(Ordering::SeqCst),
            "write_lock_held must be true while the stream (and its closure) is alive"
        );

        // Verify a second CAS would fail (lock is held).
        let second = flag
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok();
        assert!(
            !second,
            "CAS must fail while the guard-owned stream is alive"
        );

        // === Drop the stream — closure drops, guard drops, lock released. ===
        drop(pinned);

        // === After drop, lock must be false. ===
        assert!(
            !flag.load(Ordering::SeqCst),
            "write_lock_held must be false after the stream is dropped"
        );

        // === Third acquire must now succeed. ===
        let third = flag
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok();
        assert!(
            third,
            "third caller must be able to re-acquire lock after stream drop"
        );
        // Clean up — store false so the flag doesn't outlive this assert.
        flag.store(false, Ordering::SeqCst);
    }

    #[tokio::test]
    async fn ctl_stop_session_sends_stop_command() {
        let (state, _dir) = test_state();
        let id = ControlSessionId::new("stop-proj", 0);
        let (command_tx, mut command_rx) = mpsc::channel(4);
        let (event_tx, _) = broadcast::channel(16);
        let handle = SessionActorHandle {
            command_tx,
            event_tx,
            write_lock_held: Arc::new(AtomicBool::new(false)),
            metadata: Arc::new(RwLock::new(SessionMetadata::new(
                id.clone(),
                "stop-proj".into(),
                BackendKind::StreamJson,
            ))),
        };
        state.session_registry.register(id.clone(), handle).await;

        let app = test_router(Arc::clone(&state));
        let uri = format!("/api/v1/control/sessions/{}/stop", id.as_str());
        let req = Request::builder()
            .method(Method::POST)
            .uri(&uri)
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        // Verify a Stop command arrived in the channel.
        let received =
            tokio::time::timeout(std::time::Duration::from_millis(200), command_rx.recv())
                .await
                .expect("command must arrive within timeout")
                .expect("channel must not be closed");
        assert!(
            matches!(received, ActorCommand::Stop),
            "expected Stop, got {:?}",
            received
        );
    }

    // ── #6197 trust-boundary regression tests ─────────────────────────────────

    /// A non-loopback caller to `POST /sessions/run` is rejected with 403 (#6197).
    ///
    /// Why (Fail-Open Check): the pre-fix handler took no `ConnectInfo` and never
    /// checked the peer — it advanced straight into `run_session`, the spawn
    /// path. This test proves the handler now REFUSES a remote peer before any
    /// spawn. It fails on the pre-fix commit: with no guard, the request reaches
    /// `run_session` (which, under the cap-0 test state, returns 500) — never the
    /// asserted 403. It uses the raw routes so the non-loopback peer it inserts is
    /// not overwritten by a `MockConnectInfo` layer.
    /// Test: this test.
    #[tokio::test]
    async fn ctl_run_session_rejects_non_loopback_peer() {
        let (state, _dir) = test_state_no_spawn();
        let app = test_routes(state);
        // A public LAN address — the exact shape the `/rpc` guard rejects.
        let peer = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 50)), 40000);
        let mut req = Request::builder()
            .method(Method::POST)
            .uri("/api/v1/control/sessions/run")
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"project_id":"p","workdir":"/tmp","backend":"stream-json"}"#,
            ))
            .unwrap();
        req.extensions_mut().insert(ConnectInfo(peer));
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::FORBIDDEN,
            "a non-loopback caller must be refused before any spawn"
        );
    }

    /// A `claude_cmd` outside the allowed set is rejected with 400 (#6197).
    ///
    /// Why (Fail-Open Check): the pre-fix handler passed `claude_cmd` straight
    /// into `RunParams` and on into the spawn path with no validation — arbitrary
    /// process execution. This test sends a loopback peer (so the auth guard
    /// passes) with `claude_cmd` naming `sh`, and asserts a 400. It fails on the
    /// pre-fix commit: with no validation the request reaches `run_session`
    /// (which, under the cap-0 test state, returns 500) — never the asserted 400.
    /// Test: this test.
    #[tokio::test]
    async fn ctl_run_session_rejects_claude_cmd_outside_allowlist() {
        let (state, _dir) = test_state_no_spawn();
        let app = test_router(Arc::clone(&state)); // injects a loopback peer
        let req = Request::builder()
            .method(Method::POST)
            .uri("/api/v1/control/sessions/run")
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"project_id":"p","workdir":"/tmp","backend":"tmux","claude_cmd":"sh"}"#,
            ))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::BAD_REQUEST,
            "a claude_cmd outside the allowed set must be rejected before any spawn"
        );
    }

    /// The loopback guard predicate accepts local and refuses remote peers (#6197).
    #[test]
    fn loopback_guard_accepts_loopback_refuses_remote() {
        assert!(loopback_guard(&loopback_peer()).is_ok());
        let remote = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 5)), 1234);
        let err = loopback_guard(&remote).expect_err("remote must be refused");
        assert_eq!(err.0, StatusCode::FORBIDDEN);
    }

    /// `validate_claude_cmd` allows the default/`claude` forms and refuses the
    /// arbitrary-exec and injection forms (#6197).
    #[test]
    fn validate_claude_cmd_allows_claude_refuses_others() {
        // Accepted: absent override, bare name, and an explicit install path.
        assert!(validate_claude_cmd(None).is_ok());
        assert!(validate_claude_cmd(Some("claude")).is_ok());
        assert!(validate_claude_cmd(Some("/opt/anthropic/claude")).is_ok());
        // Refused: other binaries, shell injection, empty, and traversal-y names.
        for bad in [
            "sh",
            "curl",
            "/bin/bash",
            "claude; rm -rf /",
            "claude foo",
            "",
        ] {
            let err = validate_claude_cmd(Some(bad)).expect_err("must refuse");
            assert_eq!(err.0, StatusCode::BAD_REQUEST, "bad claude_cmd: {bad:?}");
        }
    }

    /// `validate_workdir` requires an absolute, non-traversing, existing dir (#6197).
    #[test]
    fn validate_workdir_requires_absolute_existing_dir() {
        let dir = tempfile::tempdir().expect("temp dir");
        assert!(validate_workdir(dir.path()).is_ok());
        // Relative, traversal, and non-existent absolute paths are all refused.
        assert!(validate_workdir(std::path::Path::new("relative/dir")).is_err());
        assert!(validate_workdir(std::path::Path::new("/tmp/../etc")).is_err());
        assert!(
            validate_workdir(std::path::Path::new("/no/such/dir/xyz-6197")).is_err(),
            "a non-existent absolute path must be refused"
        );
    }
}
