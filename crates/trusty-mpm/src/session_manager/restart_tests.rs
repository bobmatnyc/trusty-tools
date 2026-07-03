//! Tests for the graceful-shutdown / restart lifecycle (`restart_ops`).
//!
//! Why: `session_manager/tests.rs` is at the 1500-SLOC test cap; the
//! shutdown-specific coverage that exercises the DEFAULT `graceful_stop` trait
//! impl lives here so neither file grows past its limit. Keeping it in a focused
//! sibling also makes the SIGTERM-path coverage easy to find.
//! What: a minimal driver that does NOT override `graceful_stop` (so the trait
//! default runs) plus an OS-level test that the default impl actually delivers
//! SIGTERM to a known PID and then kills the tmux session.
//! Test: this file IS the test module; run with `cargo test -p trusty-mpm`.

use std::sync::Mutex;

use tempfile::TempDir;

use super::manager::{ManagedError, ManagedTmuxDriver, SessionManager};
use super::tests::FakeTmuxDriver;

/// `graceful_terminate_runtime` signals the runtime BEFORE reclaiming the pane (#1975).
///
/// Why: CLI stop/decommission must give the `claude` process a termination signal
/// (and a grace window) to flush state, not an abrupt `kill_session`. With no real
/// tmux the PID probe returns `None`, so `signal_terminate` falls back to a Ctrl-C
/// interrupt; the grace window is 0 s under `#[cfg(test)]`. Lives here (not in the
/// at-cap `tests.rs`) alongside the other `restart_ops` graceful-shutdown coverage.
/// What: seeds the session so the helper's self-guard sees it as live, drives the
/// helper directly, and asserts BOTH the interrupt fired and the pane was
/// subsequently killed — the two-phase drain.
/// Test: this function IS the test.
#[tokio::test]
async fn graceful_terminate_runtime_signals_then_kills() {
    let dir = TempDir::new().unwrap();
    let fake = FakeTmuxDriver::new();
    let mgr = SessionManager::new(dir.path(), fake.clone()).await.unwrap();

    // Seed the session so the helper's self-guard (`session_exists`, which reads
    // `list_sessions()`) reports it as live; otherwise the helper no-ops.
    fake.seeded_names
        .lock()
        .unwrap()
        .push("tm-drain-1".to_string());

    mgr.graceful_terminate_runtime("tm-drain-1").await;

    assert_eq!(
        *fake.interrupt_calls.lock().unwrap(),
        vec!["tm-drain-1".to_string()],
        "expected a Ctrl-C interrupt (SIGTERM fallback) before the pane is reclaimed"
    );
    assert_eq!(
        *fake.kill_calls.lock().unwrap(),
        vec!["tm-drain-1".to_string()],
        "expected the pane to be reclaimed after the grace window"
    );
}

/// `graceful_terminate_runtime` is a no-op when the tmux session no longer exists.
///
/// Why: the drain helper now self-guards (fast-path) so callers looping over dead
/// sessions (e.g. prune-idle) do not pay the SIGTERM grace window per already-gone
/// session, and a decommission of an already-terminated runtime does no signalling.
/// What: drives the helper against a name that was never seeded (so `session_exists`
/// reports false) and asserts neither an interrupt nor a kill was issued.
/// Test: this function IS the test.
#[tokio::test]
async fn graceful_terminate_runtime_noop_when_session_gone() {
    let dir = TempDir::new().unwrap();
    let fake = FakeTmuxDriver::new();
    let mgr = SessionManager::new(dir.path(), fake.clone()).await.unwrap();

    // Do NOT seed "tm-already-gone" — the session_exists guard must short-circuit.
    mgr.graceful_terminate_runtime("tm-already-gone").await;

    assert!(
        fake.interrupt_calls.lock().unwrap().is_empty(),
        "must not signal a session that no longer exists"
    );
    assert!(
        fake.kill_calls.lock().unwrap().is_empty(),
        "must not kill a session that no longer exists"
    );
}

/// A minimal driver that does NOT override `graceful_stop`, so the DEFAULT trait
/// impl runs — exercising its real SIGTERM-with-known-PID branch.
///
/// Why: the `FakeTmuxDriver` in `tests.rs` overrides `graceful_stop` to skip
/// signalling, so none of those tests prove the default impl delivers SIGTERM.
/// This driver leaves `graceful_stop` to the trait default and records only the
/// post-signal `kill_session` call, so the test can assert both the OS-level
/// SIGTERM (the child dies) and the mandatory cleanup kill.
/// What: no-op `create_session`/`send_line`/`capture`/`list_sessions` plus a
/// `kill_calls` recorder; `graceful_stop` is inherited from the trait default.
/// Test: `default_graceful_stop_sends_sigterm`.
#[cfg(unix)]
struct DefaultGsDriver {
    kill_calls: Mutex<Vec<String>>,
}

#[cfg(unix)]
impl ManagedTmuxDriver for DefaultGsDriver {
    fn create_session(&self, _name: &str, _workdir: &str) -> Result<(), ManagedError> {
        Ok(())
    }
    fn kill_session(&self, name: &str) -> Result<(), ManagedError> {
        self.kill_calls.lock().unwrap().push(name.to_owned());
        Ok(())
    }
    fn send_line(&self, _name: &str, _text: &str) -> Result<(), ManagedError> {
        Ok(())
    }
    fn capture(&self, _name: &str, _lines: usize) -> Result<String, ManagedError> {
        Ok(String::new())
    }
    fn list_sessions(&self) -> Result<Vec<String>, ManagedError> {
        Ok(Vec::new())
    }
    // graceful_stop intentionally NOT overridden — exercise the trait default.
}

/// The default `graceful_stop` impl sends SIGTERM to a known PID, then kills.
///
/// Why: PR A follow-up nit — prove the default trait impl's SIGTERM-with-known-
/// PID branch actually terminates the process (not just records a call). A
/// recycled-PID kill is the risk this branch must get right, so it is worth an
/// OS-level assertion against a real child.
/// What: spawns a long-lived `sleep` child, calls the DEFAULT `graceful_stop`
/// with that child's PID, then asserts the child exits (SIGTERM delivered) AND
/// `kill_session` was called as the final cleanup step. An RAII guard reaps the
/// child on any early exit so the test never leaks a process.
/// Test: this is the test.
#[cfg(unix)]
#[test]
fn default_graceful_stop_sends_sigterm() {
    use std::process::{Child, Command};
    use std::time::{Duration, Instant};

    // RAII guard: ensures the child is killed+reaped even if an assert panics.
    struct ChildGuard(Child);
    impl Drop for ChildGuard {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    let child = Command::new("sleep")
        .arg("30")
        .spawn()
        .expect("spawn sleep child");
    let pid = child.id();
    let mut guard = ChildGuard(child);

    let driver = DefaultGsDriver {
        kill_calls: Mutex::new(Vec::new()),
    };

    // Default impl: SIGTERM the known PID, then kill_session.
    driver
        .graceful_stop("tmpm-default-gs", Some(pid))
        .expect("graceful_stop");

    // The cleanup kill must have run.
    assert!(
        driver
            .kill_calls
            .lock()
            .unwrap()
            .iter()
            .any(|n| n == "tmpm-default-gs"),
        "default graceful_stop must call kill_session after signalling"
    );

    // The child must terminate from the SIGTERM within a short window.
    let deadline = Instant::now() + Duration::from_secs(5);
    let exited = loop {
        match guard.0.try_wait().expect("try_wait") {
            Some(_status) => break true,
            None if Instant::now() >= deadline => break false,
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    };
    assert!(
        exited,
        "default graceful_stop must deliver SIGTERM — the sleep child should exit"
    );
}
