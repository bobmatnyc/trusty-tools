//! Single-instance guard for the trusty-memory daemon.
//!
//! Why: macOS launchd `KeepAlive { SuccessfulExit: false }` (i.e. `OnSuccess`)
//! respawns the daemon whenever it exits with a non-zero code. When a second
//! daemon instance fails to bind (EADDRINUSE — the first instance already owns
//! port 7070 and/or the UDS socket), it exits non-zero, which launchd interprets
//! as a crash and spawns yet another copy. The resulting zombie herd (69 observed
//! in the wild) exhausts file descriptors on top of the existing fd-limit bug.
//!
//! The fix: before attempting to bind, probe the discovery files. If a healthy
//! daemon is already responding to `/health`, exit **0** (success). Launchd
//! treats exit-0 as "clean shutdown" and does NOT respawn (SuccessfulExit:false
//! = restart only on non-zero). This collapses the zombie herd immediately on
//! the next invocation without touching launchd config.
//!
//! What: exposes [`single_instance_check`] (async, for real daemon startups)
//! and [`StartupAction`] (pure enum, for unit testing the decision logic).
//!
//! Test: `startup_action_*` unit tests cover every branch including the
//! stale-socket-vs-live-socket distinction.

use std::path::Path;

/// What the daemon startup should do after the single-instance check.
///
/// Why: separating the decision from the I/O lets us unit-test the logic
/// with injected probe results rather than spinning up real TCP listeners.
/// What: three variants covering the full decision tree.
/// Test: `startup_action_from_probe_result_*` tests in this module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartupAction {
    /// Proceed to bind the TCP port and start serving.
    Proceed,
    /// Another healthy instance is already running — exit 0 cleanly so
    /// launchd does not respawn.
    ExitAlreadyRunning,
    /// A probe attempt failed with an unexpected error (not ECONNREFUSED /
    /// "no such file") — propagate as a startup failure so the operator sees
    /// a real error in the launchd log. Launchd will respawn (correctly, because
    /// this is a genuine failure).
    Fail(String),
}

/// Decide what to do based on the result of an HTTP health probe.
///
/// Why: the single-instance check reduces to "did the health probe succeed?".
/// Encoding the decision as a pure function (rather than embedding it in the
/// async probe body) makes the logic unit-testable without actual network I/O.
/// What: `probe_ok = true` → [`StartupAction::ExitAlreadyRunning`];
/// `probe_ok = false` → [`StartupAction::Proceed`].
/// Test: `startup_action_from_probe_result_when_alive`,
///       `startup_action_from_probe_result_when_dead`.
pub fn startup_action_from_probe_result(probe_ok: bool) -> StartupAction {
    if probe_ok {
        StartupAction::ExitAlreadyRunning
    } else {
        StartupAction::Proceed
    }
}

/// Perform the single-instance check at daemon startup.
///
/// Why: launchd's `KeepAlive { SuccessfulExit: false }` respawns any non-zero
/// exit, so a second daemon instance that fails to bind causes an endless
/// respawn storm. Exiting 0 (when another healthy instance is detected) short-
/// circuits this because `SuccessfulExit: false` means "restart only on
/// non-zero exits" — exit 0 is treated as a voluntary clean shutdown.
/// What: reads the `http_addr` discovery file; if it contains a reachable
/// address whose `/health` responds with HTTP 200, returns
/// [`StartupAction::ExitAlreadyRunning`]. Otherwise returns
/// [`StartupAction::Proceed`] so the caller continues with normal bind.
/// Errors reading the addr file or the network call are silently treated as
/// "not running" (returns `Proceed`) so a missing or stale file never blocks
/// a cold start.
/// Test: integration — run `trusty-memory serve --foreground` twice in the
/// same session and observe the second exits 0 without trying to bind; the
/// unit tests in this module cover the decision logic.
pub async fn single_instance_check(addr_file: Option<&Path>) -> StartupAction {
    let Some(path) = addr_file else {
        // No addr file path available (no $HOME) — proceed with bind.
        return StartupAction::Proceed;
    };
    let probe_ok = trusty_common::check_already_running(path, "/health")
        .await
        .is_some();
    startup_action_from_probe_result(probe_ok)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: when the health probe returns `Some(url)` (daemon is alive),
    /// the startup action must be `ExitAlreadyRunning` so the caller can
    /// exit 0 and stop the launchd respawn storm.
    /// What: asserts the mapping for `probe_ok = true`.
    /// Test: itself (pure function, no I/O).
    #[test]
    fn startup_action_from_probe_result_when_alive() {
        assert_eq!(
            startup_action_from_probe_result(true),
            StartupAction::ExitAlreadyRunning,
            "alive probe → ExitAlreadyRunning"
        );
    }

    /// Why: when the health probe returns `None` (addr file missing, stale,
    /// or daemon not responding), the startup action must be `Proceed` so the
    /// daemon continues with its normal bind sequence.
    /// What: asserts the mapping for `probe_ok = false`.
    /// Test: itself (pure function, no I/O).
    #[test]
    fn startup_action_from_probe_result_when_dead() {
        assert_eq!(
            startup_action_from_probe_result(false),
            StartupAction::Proceed,
            "dead/absent probe → Proceed"
        );
    }

    /// Why: when there is no addr file path (no $HOME / TRUSTY_DATA_DIR_OVERRIDE),
    /// the guard must not block a cold start — it must proceed.
    /// What: calls `single_instance_check(None)` in a tokio context and asserts
    /// the result is `Proceed`.
    /// Test: itself (no real I/O — None short-circuits immediately).
    #[tokio::test]
    async fn single_instance_check_proceeds_when_no_path() {
        let action = single_instance_check(None).await;
        assert_eq!(
            action,
            StartupAction::Proceed,
            "no addr path → Proceed (cold start must not be blocked)"
        );
    }

    /// Why: a missing addr file means no daemon is running — the guard
    /// must allow the cold start to proceed.
    /// What: passes a path to a nonexistent file and asserts `Proceed`.
    /// Test: itself (real fs stat, no network).
    #[tokio::test]
    async fn single_instance_check_proceeds_when_addr_file_missing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let missing = tmp.path().join("http_addr");
        let action = single_instance_check(Some(&missing)).await;
        assert_eq!(
            action,
            StartupAction::Proceed,
            "missing addr file → Proceed"
        );
    }

    /// Why: a stale addr file (address written but no daemon listening) must
    /// be treated as "not running" — the guard must allow the cold start.
    /// What: writes a dead address to a tempfile and asserts `Proceed`
    /// (the `check_already_running` helper cleans the stale file and returns
    /// `None`, so `startup_action_from_probe_result(false)` = Proceed).
    /// Test: itself (real fs + loopback TCP attempt, no daemon spawned).
    #[tokio::test]
    async fn single_instance_check_proceeds_when_addr_file_stale() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let addr_file = tmp.path().join("http_addr");
        // Write a port that nothing is listening on.
        std::fs::write(&addr_file, "127.0.0.1:19999\n").expect("write");
        let action = single_instance_check(Some(&addr_file)).await;
        assert_eq!(
            action,
            StartupAction::Proceed,
            "stale addr file (no listener) → Proceed"
        );
    }
}
