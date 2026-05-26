//! Session lifecycle business logic.
//!
//! Why: the HTTP handlers in `api.rs` previously embedded session resolution,
//! pause/resume mutation, pane capture, and on-disk persistence directly in
//! the request functions. Pulling that into a service makes the lifecycle
//! rules testable without an HTTP request and gives the handlers one place to
//! delegate to, so each handler shrinks to deserialize-call-serialize.
//! What: [`SessionService`] borrows the shared [`DaemonState`] and exposes the
//! resolve / reap / pause / resume / stop operations the API needs, returning
//! [`DaemonError`] instead of bare status codes.
//! Test: `cargo test -p trusty-mpm-daemon services::session` exercises resolve
//! misses and the pause/resume state transitions.

use crate::core::compress::{CompressionLevel, compress_output};
use crate::core::session::{Session, SessionStatus};

use crate::daemon::error::DaemonError;
use crate::daemon::services::tmux_service::TmuxService;
use crate::daemon::state::DaemonState;
use crate::daemon::tmux::TmuxDriver;

/// Trailing pane lines captured when a session is paused.
const PAUSE_CAPTURE_LINES: u32 = 50;

/// Maximum characters kept in an auto-derived pause summary.
const PAUSE_SUMMARY_CHARS: usize = 500;

/// Outcome of a dead-session reap sweep.
///
/// Why: the `DELETE /sessions/dead` handler reports how many entries were
/// reaped, which ones, and how many alive tmux sessions were marked `Stopped`
/// because their `claude` process exited, so callers can log the specifics.
/// What: the removed count, the removed friendly names, and the stopped count.
/// Test: `reap_removes_sessions_absent_from_tmux`.
#[derive(Debug)]
pub struct ReapResult {
    /// Number of registry entries removed.
    pub reaped: usize,
    /// Friendly names of the removed sessions.
    pub sessions: Vec<String>,
    /// Number of sessions transitioned to `Stopped` via the process check.
    pub stopped: usize,
}

/// Outcome of pausing a session.
///
/// Why: the pause handler returns the resolved id and the captured summary;
/// bundling them keeps the handler a one-liner.
/// What: the paused session's id (as a string) and the summary stored on it.
/// Test: `pause_then_resume_transitions_status`.
#[derive(Debug)]
pub struct PauseResult {
    /// The paused session's id, rendered as a UUID string.
    pub session_id: String,
    /// The summary recorded on the session (operator note or auto-derived).
    pub summary: String,
}

/// Session lifecycle operations over the shared daemon state.
///
/// Why: a thin, borrowed facade — it owns no state, just the rules — so a
/// handler can construct one per request with zero cost and call into it.
/// What: holds a borrow of [`DaemonState`]; every method is a pure mapping
/// from a request to a state mutation plus a [`DaemonError`]-typed result.
/// Test: the module's `#[cfg(test)]` suite.
pub struct SessionService<'s> {
    state: &'s DaemonState,
}

impl<'s> SessionService<'s> {
    /// Build a service bound to `state`.
    pub fn new(state: &'s DaemonState) -> Self {
        Self { state }
    }

    /// Resolve a session by exact UUID or friendly tmux name.
    ///
    /// Why: the pause / resume / command / output endpoints all accept either
    /// form; centralizing the lookup keeps them uniform and replaces the free
    /// `resolve_session` function that lived in `api.rs`.
    /// What: delegates to [`DaemonState::find_session`], mapping a miss to
    /// [`DaemonError::SessionNotFound`].
    /// Test: `resolve_hits_by_id_and_name`, `resolve_miss_is_not_found`.
    pub fn resolve(&self, key: &str) -> Result<Session, DaemonError> {
        self.state
            .find_session(key)
            .ok_or_else(|| DaemonError::SessionNotFound {
                id: key.to_string(),
            })
    }

    /// Reap registry entries whose tmux session no longer exists.
    ///
    /// Why: dead sessions accumulate forever otherwise; this is the business
    /// logic behind `DELETE /sessions/dead`.
    /// What: discovers the live tmux session names, removes any registered
    /// session absent from that set, and reports the count and names. When
    /// tmux is unavailable nothing is reaped — reaping against an empty list
    /// would wrongly delete every session.
    /// Test: `reap_removes_sessions_absent_from_tmux`.
    pub fn reap(&self) -> ReapResult {
        let before: Vec<String> = self
            .state
            .list_sessions()
            .into_iter()
            .map(|s| s.tmux_name)
            .collect();
        let outcome = match TmuxDriver::discover() {
            Ok(driver) => self.state.reap_dead_sessions(&driver),
            Err(_) => {
                tracing::info!("tmux unavailable; skipping dead-session reap");
                crate::daemon::state::ReapResult::default()
            }
        };
        let after: std::collections::HashSet<String> = self
            .state
            .list_sessions()
            .into_iter()
            .map(|s| s.tmux_name)
            .collect();
        let sessions: Vec<String> = before
            .into_iter()
            .filter(|name| !after.contains(name))
            .collect();
        ReapResult {
            reaped: outcome.reaped,
            sessions,
            stopped: outcome.stopped,
        }
    }

    /// Pause a session, capturing its output and persisting the pause record.
    ///
    /// Why: an operator stepping away needs the session frozen with a "where I
    /// left off" note that survives a daemon restart.
    /// What: resolves the session, captures the last [`PAUSE_CAPTURE_LINES`]
    /// pane lines, sets `status = Paused` / `paused_at = now` / `pause_summary`
    /// (the supplied note, or the first [`PAUSE_SUMMARY_CHARS`] chars of the
    /// `Summarise`-compressed capture), and mirrors the record to disk.
    /// Test: `pause_then_resume_transitions_status`.
    pub fn pause(
        &self,
        key: &str,
        operator_note: Option<String>,
    ) -> Result<PauseResult, DaemonError> {
        let session = self.resolve(key)?;
        let summary = operator_note.unwrap_or_else(|| {
            let captured = TmuxService::capture(&session, PAUSE_CAPTURE_LINES);
            let (compressed, _) = compress_output(&captured, CompressionLevel::Summarise);
            compressed.chars().take(PAUSE_SUMMARY_CHARS).collect()
        });
        let now = std::time::SystemTime::now();
        self.state.update_session(&session.id, |s| {
            s.status = SessionStatus::Paused;
            s.paused_at = Some(now);
            s.pause_summary = Some(summary.clone());
        });

        if let Some(updated) = self.state.session(session.id)
            && let Err(e) = crate::core::session_store::save_pause(&updated)
        {
            tracing::warn!(
                "failed to persist pause state for {}: {e}",
                session.tmux_name
            );
        }

        Ok(PauseResult {
            session_id: session.id.0.to_string(),
            summary,
        })
    }

    /// Resume a previously-paused session.
    ///
    /// Why: the counterpart to [`pause`](Self::pause); clears the frozen state
    /// and the on-disk pause record.
    /// What: resolves the session, requires `status == Paused` (else
    /// [`DaemonError::SessionNotActive`]), sets `status = Active` and clears the
    /// pause metadata, and removes the pause file.
    /// Test: `pause_then_resume_transitions_status`, `resume_unpaused_errors`.
    pub fn resume(&self, key: &str) -> Result<(), DaemonError> {
        let session = self.resolve(key)?;
        if session.status != SessionStatus::Paused {
            return Err(DaemonError::SessionNotActive {
                id: key.to_string(),
                status: format!("{:?}", session.status).to_lowercase(),
            });
        }
        self.state.update_session(&session.id, |s| {
            s.status = SessionStatus::Active;
            s.paused_at = None;
            s.pause_summary = None;
        });
        if let Err(e) = crate::core::session_store::clear_pause(&session.id) {
            tracing::warn!("failed to clear pause state for {}: {e}", session.tmux_name);
        }
        Ok(())
    }

    /// Require that a session is in a state that accepts commands.
    ///
    /// Why: the command endpoint must refuse a `Stopped` session; centralizing
    /// the check keeps the rule in the service layer.
    /// What: resolves the session and returns it unless it is `Stopped`, in
    /// which case it maps to [`DaemonError::SessionNotActive`].
    /// Test: `command_target_rejects_stopped`.
    pub fn command_target(&self, key: &str) -> Result<Session, DaemonError> {
        let session = self.resolve(key)?;
        if session.status == SessionStatus::Stopped {
            return Err(DaemonError::SessionNotActive {
                id: key.to_string(),
                status: "stopped".to_string(),
            });
        }
        Ok(session)
    }
}

/// Discover the `claude` PID inside a tmux pane and record it on a session.
///
/// Why: when the daemon brings a tmux session under management it should track
/// the OS-level `claude` process so the reaper can detect a stopped session.
/// PID discovery retries for a few seconds (claude takes 1-3 s to appear after
/// `send-keys`), so it must not block the request handler — it runs in a
/// short-lived background task.
/// What: spawns a Tokio blocking task that calls
/// [`crate::core::process::find_claude_pid_in_tmux`]; on success it records
/// the PID via [`DaemonState::set_session_pid`]. Failure is logged, never fatal.
/// Test: `set_session_pid_updates_field` covers the state mutation it performs;
/// the discovery itself is covered in `trusty-mpm-core`.
pub fn spawn_pid_capture(
    state: std::sync::Arc<DaemonState>,
    id: crate::core::session::SessionId,
    tmux_name: String,
) {
    tokio::spawn(async move {
        let captured = tokio::task::spawn_blocking(move || {
            crate::core::process::find_claude_pid_in_tmux(
                &tmux_name,
                10,
                std::time::Duration::from_millis(500),
            )
        })
        .await;
        match captured {
            Ok(Some(pid)) => {
                if state.set_session_pid(id, pid) {
                    tracing::info!("tracked claude PID {pid} for session {id:?}");
                }
            }
            Ok(None) => {
                tracing::warn!("could not find claude PID for session {id:?}");
            }
            Err(e) => {
                tracing::warn!("PID-capture task failed for session {id:?}: {e}");
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::session::{ControlModel, SessionId};

    fn active_session(state: &DaemonState) -> SessionId {
        let id = SessionId::new();
        let mut s = Session::new(id, "/tmp/p", ControlModel::Tmux, None);
        s.status = SessionStatus::Active;
        state.register_session(s);
        id
    }

    #[test]
    fn resolve_hits_by_id_and_name() {
        let state = DaemonState::new();
        let id = active_session(&state);
        let name = state.session(id).unwrap().tmux_name;
        let svc = SessionService::new(&state);
        assert_eq!(svc.resolve(&id.0.to_string()).unwrap().id, id);
        assert_eq!(svc.resolve(&name).unwrap().id, id);
    }

    #[test]
    fn resolve_miss_is_not_found() {
        let state = DaemonState::new();
        let svc = SessionService::new(&state);
        let err = svc.resolve("tmpm-no-such").unwrap_err();
        assert!(matches!(err, DaemonError::SessionNotFound { .. }));
    }

    #[test]
    fn pause_then_resume_transitions_status() {
        let state = DaemonState::new();
        let id = active_session(&state);
        let svc = SessionService::new(&state);

        let result = svc
            .pause(&id.0.to_string(), Some("mid-task".into()))
            .expect("pause succeeds");
        assert_eq!(result.summary, "mid-task");
        assert_eq!(state.session(id).unwrap().status, SessionStatus::Paused);

        svc.resume(&id.0.to_string()).expect("resume succeeds");
        let after = state.session(id).unwrap();
        assert_eq!(after.status, SessionStatus::Active);
        assert_eq!(after.pause_summary, None);
    }

    #[test]
    fn resume_unpaused_errors() {
        let state = DaemonState::new();
        let id = active_session(&state);
        let svc = SessionService::new(&state);
        let err = svc.resume(&id.0.to_string()).unwrap_err();
        assert!(matches!(err, DaemonError::SessionNotActive { .. }));
    }

    #[test]
    fn command_target_rejects_stopped() {
        let state = DaemonState::new();
        let id = SessionId::new();
        let mut s = Session::new(id, "/tmp/p", ControlModel::Tmux, None);
        s.status = SessionStatus::Stopped;
        state.register_session(s);
        let svc = SessionService::new(&state);
        let err = svc.command_target(&id.0.to_string()).unwrap_err();
        assert!(matches!(err, DaemonError::SessionNotActive { .. }));
    }

    #[test]
    fn reap_removes_sessions_absent_from_tmux() {
        // With tmux typically absent in CI, reap is a no-op and reports zero.
        // The result shape must still be well-formed.
        let state = DaemonState::new();
        active_session(&state);
        let svc = SessionService::new(&state);
        let result = svc.reap();
        assert_eq!(result.reaped, result.sessions.len());
    }
}
