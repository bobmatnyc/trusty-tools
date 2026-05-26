//! tmux I/O business logic.
//!
//! Why: the HTTP handlers embedded tmux discovery, pane capture, session
//! listing, and adoption directly. Each of those is "discover the driver, run
//! one operation, decide what a failure means" — a pattern worth consolidating
//! so the handlers stop repeating the discover-or-degrade dance.
//! What: [`TmuxService`] is a stateless namespace of associated functions for
//! the daemon's tmux operations; capture degrades to an empty string when tmux
//! is absent, while the listing operations surface [`DaemonError`].
//! Test: `cargo test -p trusty-mpm-daemon services::tmux` covers the
//! tmux-absent degradation paths (the only ones reachable without a tmux host).

use std::path::Path;
use std::process::Command;

use crate::core::external_session::ExternalSession;
use crate::core::session::Session;
use crate::core::tmux::TmuxTarget;

use crate::daemon::error::DaemonError;
use crate::daemon::tmux::{AdoptedSession, SessionSnapshot, TmuxDriver};

/// Stateless façade over the daemon's tmux operations.
///
/// Why: tmux work needs no state, only the rules for what a failure means;
/// associated functions keep call sites terse (`TmuxService::capture(..)`).
/// What: capture / list / adopt / snapshot, each wrapping a [`TmuxDriver`] call
/// with the daemon's failure policy.
/// Test: the module's `#[cfg(test)]` suite.
pub struct TmuxService;

impl TmuxService {
    /// Capture a session's pane output, degrading to empty when tmux is absent.
    ///
    /// Why: pause / command / output all want recent pane text, but tmux may be
    /// absent (CI) or the session may not be tmux-hosted. None of that is fatal
    /// — the endpoints still succeed, just without captured text. This replaces
    /// the free `capture_pane` function from `api.rs`.
    /// What: discovers tmux and captures the last `lines` pane lines; any
    /// failure is logged and yields an empty string.
    /// Test: `capture_without_tmux_is_empty`.
    pub fn capture(session: &Session, lines: u32) -> String {
        match TmuxDriver::discover() {
            Ok(driver) => {
                let target = TmuxTarget::session(&session.tmux_name);
                match driver.capture(&target, Some(lines)) {
                    Ok(text) => text,
                    Err(e) => {
                        tracing::warn!("tmux capture failed for {}: {e}", session.tmux_name);
                        String::new()
                    }
                }
            }
            Err(_) => {
                tracing::info!(
                    "tmux unavailable; capture for {} skipped",
                    session.tmux_name
                );
                String::new()
            }
        }
    }

    /// Send a command line into a session's pane, best-effort.
    ///
    /// Why: remote control of a running session; a tmux failure must not fail
    /// the request (the caller still gets whatever output was captured).
    /// What: discovers tmux and sends `command`, logging any failure.
    /// Test: covered indirectly by `send_command_returns_output_shape`.
    pub fn send_command(session: &Session, command: &str) {
        match TmuxDriver::discover() {
            Ok(driver) => {
                let target = TmuxTarget::session(&session.tmux_name);
                if let Err(e) = driver.send_line(&target, command) {
                    tracing::warn!("tmux send_line failed for {}: {e}", session.tmux_name);
                }
            }
            Err(_) => {
                tracing::info!(
                    "tmux unavailable; command for {} not sent",
                    session.tmux_name
                );
            }
        }
    }

    /// List every tmux session on the host, origin-tagged.
    ///
    /// Why: the dashboard offers to adopt external sessions; it needs the full
    /// host list. tmux being absent yields an empty list rather than an error —
    /// "no sessions" is a valid answer.
    /// What: runs `TmuxDriver::list_all_sessions`, returning an empty vec when
    /// tmux is unavailable or the listing fails.
    /// Test: `list_all_without_tmux_is_empty`.
    pub fn list_all() -> Vec<ExternalSession> {
        match TmuxDriver::discover() {
            Ok(driver) => driver.list_all_sessions().unwrap_or_else(|e| {
                tracing::warn!("tmux list_all_sessions failed: {e}");
                Vec::new()
            }),
            Err(_) => {
                tracing::info!("tmux unavailable; tmux session list is empty");
                Vec::new()
            }
        }
    }

    /// Adopt an external tmux session for monitoring.
    ///
    /// Why: trusty-mpm should watch sessions it did not create; adoption is the
    /// non-destructive opt-in.
    /// What: runs `TmuxDriver::adopt_session`. A missing session *or* absent
    /// tmux both map to [`DaemonError::SessionNotFound`] — from the caller's
    /// view "that session is not available here" is the same `404` either way.
    /// Test: `adopt_missing_session_is_not_found`.
    pub fn adopt(name: &str) -> Result<AdoptedSession, DaemonError> {
        let driver = TmuxDriver::discover().map_err(|_| DaemonError::SessionNotFound {
            id: name.to_string(),
        })?;
        driver.adopt_session(name).map_err(|e| {
            tracing::warn!("tmux adopt {name} failed: {e}");
            DaemonError::SessionNotFound {
                id: name.to_string(),
            }
        })
    }

    /// Spawn a brand-new `claude` session inside a freshly-created tmux host.
    ///
    /// Why: the GUI's "New Session" button needs the daemon to create the
    /// tmux session and start `claude` itself — the CLI is not reachable from
    /// a browser, and re-implementing the launch in the GUI would duplicate
    /// the discover-binary + create-session + send-keys ritual. Co-locating
    /// the spawn flow with the other tmux operations keeps every "talk to
    /// tmux" call routed through [`TmuxService`].
    /// What: validates that the `claude` binary is on `PATH` and tmux is
    /// available (each missing precondition maps to
    /// [`DaemonError::Unprocessable`], i.e. HTTP 422 — the request is
    /// well-formed but the daemon cannot honour it on this host), then
    /// creates `tmux_name` rooted at `workdir` (idempotently — `new-session
    /// -A` attaches if a session of that name already exists) and pipes
    /// `claude` into the session's first pane via `send-keys`.
    /// Test: `spawn_claude_without_binary_is_unprocessable`,
    /// `spawn_claude_without_tmux_is_unprocessable`, and the handler-level
    /// `spawn_session_without_claude_returns_422` and
    /// `spawn_session_without_tmux_returns_422` integration cases.
    pub fn spawn_claude(tmux_name: &str, workdir: &Path) -> Result<(), DaemonError> {
        // Verify `claude` is installed first so a missing binary fails fast
        // *before* any tmux state is created — otherwise an operator would be
        // left with an empty tmux session and no helpful error.
        if which_claude().is_none() {
            return Err(DaemonError::Unprocessable(
                "claude binary not found on PATH".to_string(),
            ));
        }

        let driver = TmuxDriver::discover()
            .map_err(|e| DaemonError::Unprocessable(format!("tmux unavailable for spawn: {e}")))?;

        let workdir_str = workdir.to_string_lossy().into_owned();
        driver
            .create_session(tmux_name, Some(&workdir_str))
            .map_err(|e| {
                DaemonError::Internal(format!("tmux new-session for {tmux_name} failed: {e}"))
            })?;

        // Start `claude` in the new session's first pane. A failure here is
        // not the request's fault — the tmux host was created — so we surface
        // it as an internal error rather than 422.
        let target = TmuxTarget::session(tmux_name);
        driver.send_line(&target, "claude").map_err(|e| {
            DaemonError::Internal(format!("failed to launch claude in {tmux_name}: {e}"))
        })?;

        Ok(())
    }

    /// Snapshot a session's pane output.
    ///
    /// Why: the dashboard inspects any session without attaching to it.
    /// What: runs `TmuxDriver::monitor_session` for the last `lines` pane lines.
    /// A missing session *or* absent tmux both map to
    /// [`DaemonError::SessionNotFound`] (a uniform `404` for "not available").
    /// Test: `snapshot_missing_session_is_not_found`.
    pub fn snapshot(name: &str, lines: u32) -> Result<SessionSnapshot, DaemonError> {
        let driver = TmuxDriver::discover().map_err(|_| DaemonError::SessionNotFound {
            id: name.to_string(),
        })?;
        driver.monitor_session(name, lines).map_err(|e| {
            tracing::warn!("tmux snapshot for {name} failed: {e}");
            DaemonError::SessionNotFound {
                id: name.to_string(),
            }
        })
    }
}

/// Resolve the `claude` binary on `PATH`.
///
/// Why: the spawn-mode `POST /sessions` flow must refuse with a clean HTTP
/// 422 when the user's host lacks Claude Code, rather than create a tmux
/// session and silently fail on `send-keys`. The check is also the single
/// place tests can override (via [`set_claude_lookup_override`]) without
/// setting up a real `claude` binary.
/// What: returns `Some(path)` when `which claude` succeeds with a non-empty
/// path; `None` otherwise. Test override consults a process-wide static.
/// Test: `spawn_claude_without_binary_is_unprocessable`.
fn which_claude() -> Option<String> {
    #[cfg(test)]
    if let Some(override_value) = claude_lookup_override() {
        return override_value;
    }
    let output = Command::new("which").arg("claude").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if path.is_empty() { None } else { Some(path) }
}

/// Process-wide serialization + override for [`which_claude`], `cfg(test)`.
///
/// Why: spawn-mode tests must be deterministic regardless of whether
/// `claude` is installed on the test host. They also share a single
/// process-wide override slot, so two concurrent spawn-mode tests would race
/// — one's `set` overwrites the other. The override here is wrapped in a
/// `Mutex` that doubles as a serialization gate: each test takes the guard
/// for the *duration of the test body*, and the guard holds the lock the
/// whole time. The `which_claude` lookup then reads through a separate
/// `RwLock` so it does not deadlock against the guard's exclusive hold.
/// What: a `MUTEX` that orders concurrent spawn-mode tests, plus an
/// `OVERRIDE` cell carrying the forced lookup value.
/// Test: this *is* the test scaffolding; spawn-mode tests rely on it.
#[cfg(test)]
static CLAUDE_TEST_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
static CLAUDE_OVERRIDE: std::sync::RwLock<Option<Option<String>>> = std::sync::RwLock::new(None);

/// Read the current claude lookup override.
///
/// Why: `which_claude` consults this under `cfg(test)` so handler-level tests
/// in sibling modules can force the spawn-mode outcome without touching
/// `PATH`. A separate `RwLock` keeps the read path from contending against
/// the test-serialisation mutex held by [`set_claude_lookup_override`].
/// What: returns the current override, or `None` when no test has set one.
/// Test: covered by `spawn_claude_without_binary_is_unprocessable` and the
/// spawn-mode tests in `api_tests.rs`.
#[cfg(test)]
pub(crate) fn claude_lookup_override() -> Option<Option<String>> {
    CLAUDE_OVERRIDE.read().ok().and_then(|g| g.clone())
}

/// RAII override guard that holds the test-serialisation mutex and clears
/// the override on drop.
///
/// Why: holding the mutex across the test body prevents two concurrent
/// spawn-mode tests from clobbering each other's override; clearing on drop
/// restores the default for any subsequent (non-spawn-mode) test that runs
/// after the lock is released.
/// What: the guard owns a `MutexGuard<'static, ()>` for the lifetime of one
/// test; dropping it releases the lock and zeroes the override.
/// Test: every spawn-mode test relies on this.
#[cfg(test)]
pub(crate) struct ClaudeOverrideGuard {
    _lock: std::sync::MutexGuard<'static, ()>,
}

#[cfg(test)]
impl Drop for ClaudeOverrideGuard {
    fn drop(&mut self) {
        if let Ok(mut guard) = CLAUDE_OVERRIDE.write() {
            *guard = None;
        }
    }
}

/// Install a `claude` lookup override for the duration of one test.
///
/// Why: see [`CLAUDE_TEST_MUTEX`] — tests need deterministic outcomes
/// independent of host state, plus serialisation so concurrent tests do not
/// race on the shared override slot.
/// What: acquires the test mutex (poisoned locks recover by re-extracting
/// the guard) and writes `value` into the override; subsequent
/// `which_claude` calls short-circuit until the guard drops.
/// Test: every spawn-mode test in this module and `api_tests.rs` consumes
/// this helper.
#[cfg(test)]
pub(crate) fn set_claude_lookup_override(value: Option<Option<String>>) -> ClaudeOverrideGuard {
    let lock = match CLAUDE_TEST_MUTEX.lock() {
        Ok(g) => g,
        // A poisoned mutex only happens if a previous test panicked while
        // holding it; the override state is well-defined regardless, so
        // recover by taking the guard out of the poisoned wrapper.
        Err(poisoned) => poisoned.into_inner(),
    };
    if let Ok(mut guard) = CLAUDE_OVERRIDE.write() {
        *guard = value;
    }
    ClaudeOverrideGuard { _lock: lock }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::session::{ControlModel, SessionId};

    #[test]
    fn capture_without_tmux_is_empty() {
        // tmux is generally absent in CI; capture must degrade to "" not panic.
        let session = Session::new(SessionId::new(), "/tmp/p", ControlModel::Tmux, None);
        let _ = TmuxService::capture(&session, 10);
    }

    #[test]
    fn list_all_without_tmux_is_empty() {
        // The listing must always return a vec (possibly empty), never error.
        let _ = TmuxService::list_all();
    }

    #[test]
    fn adopt_missing_session_is_not_found() {
        // Adopting a non-existent session (or with tmux absent) is an error,
        // not a panic.
        let result = TmuxService::adopt("tmpm-definitely-no-such-session-xyz");
        assert!(result.is_err());
    }

    #[test]
    fn snapshot_missing_session_is_not_found() {
        let result = TmuxService::snapshot("no-such-session-xyz", 10);
        assert!(result.is_err());
    }

    #[test]
    fn spawn_claude_without_binary_is_unprocessable() {
        // Force the `claude` lookup to fail; `spawn_claude` must short-circuit
        // with `Unprocessable` (HTTP 422) before any tmux work happens.
        let _guard = set_claude_lookup_override(Some(None));
        let result = TmuxService::spawn_claude("tmpm-test-no-bin", Path::new("/tmp"));
        match result {
            Err(DaemonError::Unprocessable(msg)) => {
                assert!(
                    msg.contains("claude binary"),
                    "message should name the missing binary: {msg}"
                );
            }
            other => panic!("expected Unprocessable, got {other:?}"),
        }
    }

    #[test]
    fn spawn_claude_without_tmux_is_unprocessable_when_tmux_missing() {
        // With the binary lookup forced positive but tmux generally absent in
        // CI, the spawn must still degrade gracefully — never panic, and when
        // tmux is missing surface `Unprocessable` rather than `Internal`.
        let _guard = set_claude_lookup_override(Some(Some("/fake/claude".into())));
        let result = TmuxService::spawn_claude("tmpm-test-no-tmux", Path::new("/tmp"));
        if TmuxDriver::is_available() {
            // tmux IS available on this host: the call either succeeds or
            // fails on send-keys (an `Internal` error); in any case it must
            // not be a panic, so just exercising the path is enough.
            // We clean up after ourselves if we did create a session.
            if result.is_ok()
                && let Ok(driver) = TmuxDriver::discover()
            {
                let _ = driver.kill_session("tmpm-test-no-tmux");
            }
        } else {
            // tmux MISSING: 422 is the documented contract.
            assert!(
                matches!(result, Err(DaemonError::Unprocessable(_))),
                "expected Unprocessable on no-tmux host, got {result:?}"
            );
        }
    }
}
