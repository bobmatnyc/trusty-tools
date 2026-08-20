//! Bounded-wait capture spawn — the deadline-carrying sibling of
//! [`super::disclaimed_output`].
//!
//! Why: every other spawn shape in this module waits on its child with no
//! bound. `core::output_style::claude_supports_native_output_style` probes
//! `claude --version` on the session-launch path, and a wedged `claude` binary
//! made that wait never return — a live `tm session` launch, and every surface
//! that composes a prompt, hung indefinitely and left the probe child orphaned
//! to PID 1 when the operator killed `tm` (issue #5969).
//! What: [`disclaimed_stdout_with_timeout`] spawns through
//! [`super::disclaimed_stdout_piped_spawn`] — so the child is still TCC-
//! disclaimed on macOS — drains stdout on a helper thread, and polls the
//! child's exit non-blockingly (`waitpid(WNOHANG)` on the disclaimed path,
//! `Child::try_wait` on the native path) until `timeout` elapses. On the
//! deadline it SIGKILLs the child and reaps it before returning
//! `io::ErrorKind::TimedOut`, so no probe process outlives the call. Only the
//! thread that polls ever waits on the pid, so no kill can ever race a reap
//! onto a recycled pid. `stderr` is discarded (this spawn shape sends it to
//! `/dev/null`) and the returned `Output::stderr` is always empty; stdin is
//! inherited rather than `/dev/null`, which differs from
//! [`super::disclaimed_output`] and suits the probe this exists for — a
//! `--version` call that reads no input and is now killed on a deadline
//! either way.
//! Test: `tests::disclaimed_stdout_with_timeout_returns_output_for_fast_child`,
//! `tests::disclaimed_stdout_with_timeout_kills_and_reaps_wedged_child`,
//! `tests::disclaimed_stdout_with_timeout_reports_spawn_error_for_missing_binary`.

use std::io::{self, Read as _};
use std::process::{Command, ExitStatus, Output};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use super::stdout_piped::{StdoutPipedHandle, StdoutPipedSpawn, disclaimed_stdout_piped_spawn};

/// How often the bounded wait re-checks whether the child has exited.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// How long the stdout drain thread may still be finishing after the child
/// exited before its bytes are given up on. A pipe that has both hit EOF and
/// been closed by the child drains in microseconds; this only matters when a
/// grandchild inherited the write end, and giving up there is strictly better
/// than the unbounded wait it replaces.
const DRAIN_GRACE: Duration = Duration::from_secs(1);

/// Run `program` with `args`, capturing stdout, and give up after `timeout`.
///
/// Why: see the module docs — the `claude --version` probe needs a wait it can
/// lose (issue #5969).
/// What: spawns via [`super::disclaimed_stdout_piped_spawn`] (stdout piped,
/// stderr to `/dev/null`, TCC disclaimed on macOS), then polls for exit until
/// `timeout` elapses. A child that outlives the deadline is SIGKILLed and
/// reaped, and the call returns `io::ErrorKind::TimedOut`. `Output::stderr` is
/// always empty.
/// Test: `tests::disclaimed_stdout_with_timeout_returns_output_for_fast_child`,
/// `tests::disclaimed_stdout_with_timeout_kills_and_reaps_wedged_child`,
/// `tests::disclaimed_stdout_with_timeout_reports_spawn_error_for_missing_binary`.
pub fn disclaimed_stdout_with_timeout(
    program: &str,
    args: &[String],
    timeout: Duration,
) -> io::Result<Output> {
    let mut cmd = Command::new(program);
    cmd.args(args);
    let StdoutPipedSpawn {
        id,
        mut stdout,
        handle,
    } = disclaimed_stdout_piped_spawn(cmd)?;

    // Drain stdout off-thread: `read_to_end` blocks until EOF, which a wedged
    // child never delivers, so the deadline below must not sit behind it.
    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf);
        let _ = tx.send(buf);
    });

    let mut waiter = Waiter(handle);
    let deadline = Instant::now() + timeout;
    loop {
        match waiter.try_wait() {
            Ok(Some(status)) => {
                let stdout = rx.recv_timeout(DRAIN_GRACE).unwrap_or_default();
                return Ok(Output {
                    status,
                    stdout,
                    stderr: Vec::new(),
                });
            }
            Ok(None) => {}
            // A poll that cannot answer must not leave the child running
            // either — that is the whole point of #5969. `ECHILD` is the one
            // exception: it says the pid is already gone, so signalling it
            // could only reach a recycled one.
            Err(err) => {
                if err.raw_os_error() != Some(libc::ECHILD) {
                    waiter.kill_and_reap();
                }
                return Err(err);
            }
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        std::thread::sleep(POLL_INTERVAL.min(remaining));
    }

    waiter.kill_and_reap();
    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        format!("`{program}` (pid {id}) did not exit within {timeout:?}; child killed"),
    ))
}

/// The exit side of a [`StdoutPipedSpawn`], reduced to the three operations
/// the bounded wait needs, uniform across the native and disclaimed paths.
///
/// Why: the two spawn paths reap differently — `std::process::Child` owns its
/// own wait state, the disclaimed path holds a bare pid — but the deadline
/// logic above is identical for both.
/// What: wraps [`StdoutPipedHandle`] with a non-blocking poll, plus a
/// kill-then-blocking-reap used only once the deadline has passed.
/// Test: see [`disclaimed_stdout_with_timeout`].
struct Waiter(StdoutPipedHandle);

impl Waiter {
    /// `Some(status)` once the child has exited; `None` while it still runs.
    fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        match &mut self.0 {
            StdoutPipedHandle::Native(child) => child.try_wait(),
            #[cfg(target_os = "macos")]
            StdoutPipedHandle::Disclaimed(pid) => super::macos::try_wait_for(*pid),
        }
    }

    /// SIGKILL the child, then block until it is reaped.
    ///
    /// The blocking reap is safe precisely because nothing else waits on this
    /// pid: SIGKILL cannot be caught or ignored, so the wait returns as soon
    /// as the kernel tears the process down. Best-effort — a child that
    /// already exited between the last poll and here is simply reaped.
    fn kill_and_reap(&mut self) {
        match &mut self.0 {
            StdoutPipedHandle::Native(child) => {
                let _ = child.kill();
                let _ = child.wait();
            }
            #[cfg(target_os = "macos")]
            StdoutPipedHandle::Disclaimed(pid) => {
                // SAFETY: `pid` is our own child and has not been reaped —
                // `try_wait` returned `None` for it and nothing else waits on
                // it, so the pid cannot have been recycled.
                unsafe { libc::kill(*pid, libc::SIGKILL) };
                let _ = super::macos::wait_for(*pid);
            }
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    #[serial_test::serial]
    fn disclaimed_stdout_with_timeout_returns_output_for_fast_child() {
        let out = disclaimed_stdout_with_timeout(
            "/bin/sh",
            &args(&["-c", "echo probe-output; exit 7"]),
            Duration::from_secs(10),
        )
        .expect("a child that exits well inside the deadline must not error");
        assert_eq!(out.status.code(), Some(7));
        assert_eq!(String::from_utf8_lossy(&out.stdout), "probe-output\n");
        assert!(out.stderr.is_empty(), "this spawn shape discards stderr");
    }

    /// The #5969 regression: before the fix this call shape (`disclaimed_output`
    /// on a wedged binary) never returned, and killing the caller orphaned the
    /// child to PID 1. The bounded wait must return promptly AND leave no live
    /// process behind.
    #[test]
    #[serial_test::serial]
    fn disclaimed_stdout_with_timeout_kills_and_reaps_wedged_child() {
        let dir = tempfile::tempdir().expect("tempdir");
        let pidfile = dir.path().join("pid");
        // `exec` so the recorded pid IS the long sleep, not a shell wrapper.
        let script = format!("echo $$ > {}; exec sleep 300", pidfile.display());

        let started = Instant::now();
        let err = disclaimed_stdout_with_timeout(
            "/bin/sh",
            &args(&["-c", &script]),
            Duration::from_millis(300),
        )
        .expect_err("a wedged child must surface a timeout, not hang");
        let elapsed = started.elapsed();

        assert_eq!(err.kind(), io::ErrorKind::TimedOut, "err: {err}");
        assert!(
            elapsed < Duration::from_secs(10),
            "bounded wait must return promptly, took {elapsed:?}"
        );

        let pid: libc::pid_t = std::fs::read_to_string(&pidfile)
            .expect("child must have recorded its pid before wedging")
            .trim()
            .parse()
            .expect("pid file must hold a pid");
        // SAFETY: signal 0 performs the existence/permission check only.
        let alive = unsafe { libc::kill(pid, 0) };
        assert_eq!(
            alive, -1,
            "no wedged child may outlive the timeout (pid {pid} still exists)"
        );
        assert_eq!(
            io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH),
            "the child must be gone, not merely unsignalable"
        );
    }

    #[test]
    #[serial_test::serial]
    fn disclaimed_stdout_with_timeout_reports_spawn_error_for_missing_binary() {
        let err = disclaimed_stdout_with_timeout(
            "/nonexistent/definitely-not-a-real-binary-5969",
            &[],
            Duration::from_secs(1),
        )
        .expect_err("spawning a missing binary must error, not time out");
        assert_ne!(
            err.kind(),
            io::ErrorKind::TimedOut,
            "a spawn failure must not be reported as a timeout: {err}"
        );
    }
}
