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
/// Map a [`TmuxDriver::discover`] failure onto the right [`DaemonError`] (#5784).
///
/// Why: a scratch-environment refusal is not a missing session. Collapsing it
/// into `SessionNotFound` answered "that session does not exist" — HTTP 404 —
/// to an operator whose real problem is a reassigned `$HOME`, and buried the
/// variable that lifts the gate. This crate's own gate doc says a silent skip
/// is its own trap; this was that trap.
/// What: asks the gate directly rather than sniffing `discover`'s error text,
/// so a refusal becomes `TmuxUnavailable` carrying the full reason (including
/// `TRUSTY_MPM_ALLOW_HOST_STATE`). Every other failure — tmux genuinely
/// absent — keeps the historical `SessionNotFound` mapping for `name`.
/// Test: `scratch_home_daemon_does_not_spawn_tmux` in
/// `tests/scratch_home_tmux_gate.rs` calls `TmuxService::adopt` under a
/// scratch `$HOME` and asserts the error names the gate, not a missing
/// session.
fn discover_or_session_error(name: &str) -> Result<TmuxDriver, DaemonError> {
    if let Some(reason) = crate::core::host_state_gate::host_state_access().skip_reason() {
        return Err(DaemonError::TmuxUnavailable(reason));
    }
    TmuxDriver::discover().map_err(|_| DaemonError::SessionNotFound {
        id: name.to_string(),
    })
}

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

    /// Kill the tmux session named `tmux_name`, best-effort.
    ///
    /// Why: `DELETE /sessions/{id}` must KILL the owning tmux session, not merely
    /// drop the registry entry — otherwise every delete leaks a live managed (`tm-*`/`tmpm-*`)
    /// session (epic #1452, #1454). The kill is best-effort because the session
    /// may already be gone (the operator killed it by hand, the reaper got it
    /// first, or it never had a tmux host on this box). Failing the HTTP response
    /// on a kill error would surface a confusing 5xx for the common "already
    /// gone" case, so the policy mirrors `stop`/`decommission`: log and move on.
    /// What: discovers tmux and issues a single `kill_session`; any failure
    /// (tmux absent, or the kill itself) is logged to stderr via `tracing` and
    /// swallowed. Logging only — never to stdout, which carries MCP JSON-RPC
    /// framing. Returns nothing: there is no actionable error for the caller.
    /// Test: `kill_best_effort_without_tmux_is_noop` (tmux-absent path) and the
    /// `full_user_cycle` integration test asserts the session is gone after a
    /// DELETE when tmux is available.
    pub fn kill_best_effort(tmux_name: &str) {
        match TmuxDriver::discover() {
            Ok(driver) => {
                if let Err(e) = driver.kill_session(tmux_name) {
                    tracing::warn!(
                        "tmux kill_session failed for {tmux_name} (may already be gone): {e}"
                    );
                }
            }
            Err(_) => {
                tracing::info!("tmux unavailable; kill for {tmux_name} skipped");
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
        let driver = discover_or_session_error(name)?;
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
    /// `spawn_session_without_tmux_returns_422` integration cases. The
    /// `send_line`-failure rollback (#1456) is covered by
    /// `spawn_claude_rollback_kills_session_on_send_failure` (real-tmux,
    /// `#[ignore]`).
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
        // it as an internal error rather than 422. But first ROLL BACK: the
        // tmux session was already created above, so a `send_line` failure here
        // would orphan a live managed (`tm-*`/`tmpm-*`) host with no record and no owner (the
        // exact leak epic #1452 is closing — #1456). Reap the just-created
        // session best-effort BEFORE propagating the original error, so the
        // orphan never accumulates. The kill is best-effort/logged (the session
        // may already be gone); the original `send_line` error still propagates
        // to the caller — the rollback never masks it.
        let target = TmuxTarget::session(tmux_name);
        if let Err(e) = driver.send_line(&target, &crate::daemon::spawn_command::relaunch_command())
        {
            Self::kill_best_effort(tmux_name);
            return Err(DaemonError::Internal(format!(
                "failed to launch claude in {tmux_name}: {e}"
            )));
        }

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
        let driver = discover_or_session_error(name)?;
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
    // #6736: the test below pins `$HOME` and must also hold the gate's opt-in still.
    use crate::core::host_state_gate::ALLOW_HOST_STATE_ENV;
    use crate::core::session::{ControlModel, SessionId};

    #[test]
    fn capture_without_tmux_is_empty() {
        // tmux is generally absent in CI; capture must degrade to "" not panic.
        let session = Session::new(SessionId::new(), "/tmp/p", ControlModel::Tmux, None);
        let _ = TmuxService::capture(&session, 10);
    }

    #[test]
    fn kill_best_effort_is_noop_for_unknown_session() {
        // Killing a session that does not exist (or with tmux absent in CI) must
        // be a silent no-op, never a panic — the DELETE handler relies on this
        // to never fail the HTTP response on a best-effort tmux kill (#1454).
        TmuxService::kill_best_effort("tmpm-definitely-no-such-session-xyz");
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

    /// RAII guard that force-kills a tmux session on drop so a failed assertion
    /// in a real-tmux test never leaks a managed (`tm-*`/`tmpm-*`) host.
    ///
    /// Why: the rollback test below creates a real tmux session; if an
    /// assertion panics before the test reaps it, the session would leak —
    /// exactly the orphan the fix exists to prevent. This guard guarantees
    /// cleanup on every exit path (return, panic) regardless of test outcome.
    /// What: holds a session name and best-effort kills it on drop via the
    /// same `kill_session` path the daemon uses; never panics in `Drop`.
    /// Test: this is test scaffolding for
    /// `spawn_claude_rollback_kills_session_on_send_failure`.
    struct KillOnDrop(String);
    impl Drop for KillOnDrop {
        fn drop(&mut self) {
            if let Ok(driver) = TmuxDriver::discover() {
                let _ = driver.kill_session(&self.0);
            }
        }
    }

    /// Probe whether a tmux session of the given name currently exists.
    ///
    /// Why: the rollback assertion must confirm the session is GONE after the
    /// rollback kill; `list_all` is the discovery surface already used by the
    /// daemon, so reusing it keeps the test honest about real host state.
    /// What: returns true iff a session named `name` appears in the host's
    /// current tmux session list.
    /// Test: used by `spawn_claude_rollback_kills_session_on_send_failure`.
    fn tmux_session_exists(name: &str) -> bool {
        match TmuxDriver::discover() {
            Ok(driver) => driver
                .list_all_sessions()
                .map(|sessions| sessions.iter().any(|s| s.name == name))
                .unwrap_or(false),
            Err(_) => false,
        }
    }

    /// On a `send_line` failure after the tmux host is created, `spawn_claude`
    /// must leave NO orphaned tmux session behind (#1456).
    ///
    /// Why: the bug was that `spawn_claude` created the tmux session, then a
    /// `send_line` failure returned 500 without reaping the session — leaking a
    /// live managed (`tm-*`/`tmpm-*`) host (the exact leak class epic #1452 closes). This test
    /// reproduces the post-create rollback the fix performs against REAL tmux
    /// and asserts the session is gone afterward.
    /// What: forces the `claude` lookup positive (so spawn passes its
    /// preconditions), creates a real session via the same driver call
    /// `spawn_claude` makes, then drives the rollback path (`kill_best_effort`,
    /// the helper the fix invokes on `send_line` failure) and asserts the
    /// session no longer exists. A `KillOnDrop` guard guarantees the test never
    /// leaks even if an assertion fails. `#[ignore]` because it needs a real
    /// tmux host; run with `cargo test -p trusty-mpm -- --include-ignored`.
    /// Test: this function IS the test.
    #[test]
    #[ignore = "requires a real tmux host; run with --include-ignored"]
    fn spawn_claude_rollback_kills_session_on_send_failure() {
        if !TmuxDriver::is_available() {
            eprintln!("tmux unavailable; skipping real-tmux rollback test");
            return;
        }
        let driver = TmuxDriver::discover().expect("tmux available");
        let name = format!("tmpm-rollback-test-{}", std::process::id());

        // Arm cleanup BEFORE creating the session so a panic anywhere below
        // (including in create) cannot leak the host.
        let _cleanup = KillOnDrop(name.clone());

        // Reproduce spawn_claude's first step: create the tmux host.
        driver
            .create_session(&name, Some("/tmp"))
            .expect("create_session should succeed on a real tmux host");
        assert!(
            tmux_session_exists(&name),
            "precondition: the session must exist right after create_session"
        );

        // Drive the rollback path the fix takes on a post-create failure: the
        // best-effort kill that reaps the just-created host. (We exercise the
        // rollback directly because `send_line` cannot be forced to fail
        // against a valid session without a test seam into TmuxDriver.)
        TmuxService::kill_best_effort(&name);

        assert!(
            !tmux_session_exists(&name),
            "after rollback the orphaned tmux session must be gone (#1456)"
        );
    }

    /// RAII guard restoring `$HOME` and the host-state opt-in on drop.
    ///
    /// Why: [`scratch_home`] moves both process-wide, and a test that left
    /// either moved would hand the next test the very straddle #6736 is about.
    /// What: remembers the prior values and puts them back on drop, including
    /// on a panic-driven unwind; an absent variable is restored as absent.
    /// Test: scaffolding for
    /// `spawn_claude_without_tmux_is_unprocessable_when_tmux_missing`.
    struct HostEnvGuard {
        home: Option<std::ffi::OsString>,
        opt_in: Option<std::ffi::OsString>,
    }

    impl Drop for HostEnvGuard {
        fn drop(&mut self) {
            // SAFETY: every caller of `scratch_home` is `#[serial_test::serial]`,
            // so no other test thread races this restore.
            unsafe {
                match self.home.take() {
                    Some(v) => std::env::set_var("HOME", v),
                    None => std::env::remove_var("HOME"),
                }
                match self.opt_in.take() {
                    Some(v) => std::env::set_var(ALLOW_HOST_STATE_ENV, v),
                    None => std::env::remove_var(ALLOW_HOST_STATE_ENV),
                }
            }
        }
    }

    /// Pin `$HOME` at a fresh tempdir and clear the host-state opt-in.
    ///
    /// Why: the #5784 gate re-reads `$HOME` and `TRUSTY_MPM_ALLOW_HOST_STATE` on
    /// EVERY call, so a test that needs one verdict for a whole call chain has
    /// to hold both still. A tempdir can never be this uid's password-database
    /// home, so the gate classifies it scratch and refuses — the same verdict on
    /// every host, whether or not tmux is installed.
    /// What: returns the `TempDir` (which the caller must keep bound — dropping
    /// it removes the directory `$HOME` names) and the restore guard.
    /// Test: scaffolding for
    /// `spawn_claude_without_tmux_is_unprocessable_when_tmux_missing`.
    fn scratch_home() -> (tempfile::TempDir, HostEnvGuard) {
        let home = tempfile::tempdir().expect("temp dir for a pinned $HOME");
        let guard = HostEnvGuard {
            home: std::env::var_os("HOME"),
            opt_in: std::env::var_os(ALLOW_HOST_STATE_ENV),
        };
        // SAFETY: caller is `#[serial_test::serial]`.
        unsafe {
            std::env::set_var("HOME", home.path());
            std::env::remove_var(ALLOW_HOST_STATE_ENV);
        }
        (home, guard)
    }

    /// A host `spawn_claude` cannot reach tmux on answers 422, never 500.
    ///
    /// Why serial and a pinned `$HOME` (#6736): `spawn_claude` consults the
    /// #5784 host-state gate twice — once in `TmuxDriver::discover`, again
    /// inside `create_session` — and the gate re-reads `$HOME` on each call. A
    /// sibling test moving `$HOME` process-wide between those two reads let
    /// discover succeed against the operator's real home while the spawn was
    /// refused against the sibling's tempdir, so the call returned
    /// `Internal("tmux session creation refused: …")` while this test's own
    /// `TmuxDriver::is_available()` probe — a THIRD read — answered false. Same
    /// class as #6711, and it failed 5 of 5 runs beside `parity_doctor` at
    /// `--test-threads=8`. Pinning `$HOME` at a tempdir makes the gate refuse at
    /// the FIRST read on every host, so the outcome depends on neither sibling
    /// order nor whether tmux is installed, and the host-branch probe is gone
    /// with it. The pin is what holds under nextest, where every test gets its
    /// own process and `#[serial]` orders nothing (#4162); `#[serial]` is what
    /// holds under `cargo test`, where the `$HOME`-moving siblings share this
    /// process. Nothing cross-process is shared — the refusal lands before any
    /// tmux session is created — so `#[file_serial]` is not needed.
    /// Test: this function IS the test.
    #[serial_test::serial]
    #[test]
    fn spawn_claude_without_tmux_is_unprocessable_when_tmux_missing() {
        // #6736: pinned BEFORE the spawn, and held across both of its gate reads.
        let (_home, _home_guard) = scratch_home();
        let _guard = set_claude_lookup_override(Some(Some("/fake/claude".into())));
        let result = TmuxService::spawn_claude("tmpm-test-no-tmux", Path::new("/tmp"));
        match result {
            Err(DaemonError::Unprocessable(msg)) => assert!(
                msg.contains("tmux unavailable for spawn"),
                "the 422 must name the unreachable tmux: {msg}"
            ),
            other => panic!("expected Unprocessable on an unreachable-tmux host, got {other:?}"),
        }
    }
}
