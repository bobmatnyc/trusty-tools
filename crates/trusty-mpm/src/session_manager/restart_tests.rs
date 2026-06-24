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

use super::manager::{ManagedError, ManagedTmuxDriver};

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
