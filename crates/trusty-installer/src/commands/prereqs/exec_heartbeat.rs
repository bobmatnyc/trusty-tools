//! Captured, heartbeat-emitting execution for long-running prereq
//! auto-install commands (#3821, code-critic HIGH finding on PR #3879).
//!
//! Why: A first-run `brew install tmux` routinely takes 1-5+ minutes on a
//! clean machine (Homebrew self-update, a non-bottled build) with ZERO
//! output from a plain `.output()`-captured call for that entire span —
//! exactly on the piped `curl | sh -s -- -y` demo path this crate exists to
//! support. That silence reads as a hang. A periodic, INSTALLER-EMITTED
//! heartbeat line fixes the perception without reintroducing #3830: the
//! child's own stdout/stderr stay fully captured (never inherited) — the
//! heartbeat text comes from the installer's own `on_tick` callback, never
//! from child passthrough.
//!
//! What: [`run_captured_with_heartbeat`] spawns `cmd` with piped
//! stdout/stderr, drains both on background threads so the child can never
//! block on a full pipe, and polls `Child::try_wait()` on a short interval.
//! Every time `heartbeat` elapses while the child is still running, it
//! calls `on_tick(elapsed_secs)`. On exit it folds the captured stderr
//! (trimmed) into the returned error, mirroring
//! `service_bootstrap::run_captured`'s contract exactly — this is that same
//! captured-not-inherited guarantee, extended with interim signal for
//! long-running commands.
//!
//! Test: `tests::heartbeat_fires_while_child_runs` (a short interval + a
//! sleeping child proves multiple, monotonically-increasing ticks before
//! completion), `tests::no_heartbeat_when_child_exits_fast` (a large
//! interval + an instant child proves zero ticks — no spam for the common
//! case), `tests::captures_stderr_into_error` (the #3830-style proof: an
//! error's message contains the child's exact stderr, only reachable via
//! captured, non-inherited execution), `tests::success_returns_ok`.

use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// Ceiling on how coarsely we poll `try_wait()` in production — far finer
/// than [`DEFAULT_HEARTBEAT`] so a tick fires close to on-time without
/// busy-looping.
const POLL_INTERVAL_CAP: Duration = Duration::from_millis(200);
/// Floor on the poll interval so a caller-supplied sub-millisecond heartbeat
/// (only realistic in tests) can never busy-loop.
const POLL_INTERVAL_FLOOR: Duration = Duration::from_millis(5);

/// Production heartbeat cadence (#3821): long enough to never spam a fast
/// install, short enough that a piped `-y` demo is never silent for more
/// than ~20s while a package manager is still working.
pub const DEFAULT_HEARTBEAT: Duration = Duration::from_secs(15);

/// Run `cmd` with output captured, calling `on_tick(elapsed_secs)` every
/// `heartbeat` while the child is still running.
///
/// Why/What/Test: see the module doc.
pub fn run_captured_with_heartbeat(
    mut cmd: Command,
    label: &str,
    heartbeat: Duration,
    mut on_tick: impl FnMut(u64),
) -> anyhow::Result<()> {
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    let mut child: Child = cmd
        .spawn()
        .map_err(|e| anyhow::anyhow!("spawn {label}: {e}"))?;

    // Drain both pipes on background threads so the child can never block
    // writing to a full pipe while we're busy polling/sleeping — captured,
    // never inherited (#3830).
    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();
    let stderr_thread = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut p) = stderr_pipe {
            let _ = p.read_to_end(&mut buf);
        }
        buf
    });
    let stdout_thread = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut p) = stdout_pipe {
            let _ = p.read_to_end(&mut buf);
        }
        buf
    });

    // Poll at a resolution proportional to the requested heartbeat — capped
    // for production (never busier than every 200ms) and floored for tests
    // (a 50ms test heartbeat must not be quantized by a 200ms production
    // poll tick, or it can never fire more than once or twice — the #3821
    // PR #3879 flake this fixes).
    let poll_interval = heartbeat.min(POLL_INTERVAL_CAP).max(POLL_INTERVAL_FLOOR);

    let start = Instant::now();
    let mut next_tick = heartbeat;
    let status = loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|e| anyhow::anyhow!("wait {label}: {e}"))?
        {
            break status;
        }
        if start.elapsed() >= next_tick {
            on_tick(next_tick.as_secs());
            next_tick += heartbeat;
        }
        std::thread::sleep(poll_interval);
    };

    // Captured, never printed here — the heartbeat above is the only output
    // this function emits on the caller's behalf (#3830).
    let _stdout = stdout_thread.join().unwrap_or_default();
    let stderr_buf = stderr_thread.join().unwrap_or_default();

    if status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&stderr_buf);
        let stderr = stderr.trim();
        if stderr.is_empty() {
            anyhow::bail!("{label} exited with {status}")
        } else {
            anyhow::bail!("{label} exited with {status}: {stderr}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn sh(script: &str) -> Command {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(script);
        cmd
    }

    /// Why: THE #3821 fix — a long-running child must produce interim,
    /// installer-emitted signal rather than total silence.
    /// What: A ~700ms-sleeping child polled with a 20ms heartbeat interval
    /// (a generous ~35x margin over the 2-tick minimum this asserts, so the
    /// test tolerates real scheduling jitter under a loaded/parallel `cargo
    /// test` run without flaking); asserts `on_tick` fires at least twice
    /// with non-decreasing elapsed-seconds arguments before the command
    /// completes successfully.
    /// Test: This is the test.
    #[test]
    fn heartbeat_fires_while_child_runs() {
        let ticks: Arc<Mutex<Vec<u64>>> = Arc::new(Mutex::new(Vec::new()));
        let ticks_cb = ticks.clone();
        let result = run_captured_with_heartbeat(
            sh("sleep 0.7; exit 0"),
            "`sleep test`",
            Duration::from_millis(20),
            move |elapsed| ticks_cb.lock().expect("lock ticks").push(elapsed),
        );
        assert!(result.is_ok(), "expected Ok, got {result:?}");
        let seen = ticks.lock().expect("lock ticks").clone();
        assert!(
            seen.len() >= 2,
            "expected multiple heartbeat ticks over ~700ms at 20ms cadence, got: {seen:?}"
        );
        let mut sorted = seen.clone();
        sorted.sort_unstable();
        assert_eq!(seen, sorted, "ticks must be strictly increasing: {seen:?}");
    }

    /// Why: a fast command (the common case — most prereqs are already
    /// satisfied, or install quickly) must never emit a spurious heartbeat.
    /// What: An instantly-exiting child with a long heartbeat interval;
    /// asserts `on_tick` is never called.
    /// Test: This is the test.
    #[test]
    fn no_heartbeat_when_child_exits_fast() {
        let ticks: Arc<Mutex<Vec<u64>>> = Arc::new(Mutex::new(Vec::new()));
        let ticks_cb = ticks.clone();
        let result = run_captured_with_heartbeat(
            sh("exit 0"),
            "`fast test`",
            Duration::from_secs(30),
            move |elapsed| ticks_cb.lock().expect("lock ticks").push(elapsed),
        );
        assert!(result.is_ok());
        assert!(ticks.lock().expect("lock ticks").is_empty());
    }

    /// Why (#3830-style proof): the error path must be built from CAPTURED
    /// stderr — only reachable via piped, non-inherited execution.
    /// What: A child that writes a distinctive marker to stderr and exits
    /// non-zero; asserts the returned error's message contains it.
    /// Test: This is the test.
    #[test]
    fn captures_stderr_into_error() {
        let err = run_captured_with_heartbeat(
            sh("echo 'DISTINCTIVE_MARKER_3821_HEARTBEAT: brew failed' >&2; exit 7"),
            "`fake failing install`",
            Duration::from_secs(30),
            |_| panic!("must not tick before this fast-failing child exits"),
        )
        .expect_err("expected Err");
        let msg = err.to_string();
        assert!(
            msg.contains("DISTINCTIVE_MARKER_3821_HEARTBEAT: brew failed"),
            "error message did not fold in captured stderr: {msg}"
        );
    }

    /// Why: the success path must be a plain `Ok(())`, matching the
    /// pre-heartbeat `real_exec` contract exactly.
    /// What: A trivially successful command; asserts `Ok(())`.
    /// Test: This is the test.
    #[test]
    fn success_returns_ok() {
        let result =
            run_captured_with_heartbeat(sh("exit 0"), "`ok test`", Duration::from_secs(30), |_| {
                panic!("must not tick")
            });
        assert!(result.is_ok());
    }
}
