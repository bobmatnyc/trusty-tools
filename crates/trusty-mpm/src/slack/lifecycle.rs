//! Slack bot process lifecycle: PID-file recording and precise `stop`.
//!
//! Why: `tm slack stop` must terminate exactly the bot `tm slack start` launched —
//! not any process whose argv happens to contain "slack start" (the old
//! `pkill -f "slack start"` could kill an editor, a grep, or an unrelated tool).
//! A PID file records the one true process so `stop` signals precisely it. This
//! concern is self-contained (no Socket-Mode, executor, or proxy coupling), so it
//! lives in its own module to keep `slack/mod.rs` under the 500-SLOC cap (#2549).
//! What: [`pid_file_path`] locates the file, [`write_pid_file`] records the PID at
//! startup, [`PidFileGuard`] removes it on every exit path of `run`, and
//! [`stop_via_pid_file`] signals the recorded process, returning a typed
//! [`StopOutcome`].
//! Test: `slack::tests` covers `pid_file_path`, the guard's drop behavior, and the
//! stop paths (missing / stale / garbage PID file).

use std::path::Path;

/// The PID-file name (under `~/.trusty-mpm`) for the foreground Slack bot.
///
/// Why: `tm slack stop` must terminate exactly the bot `tm slack start` launched —
/// not any process whose argv happens to contain "slack start" (the old
/// `pkill -f "slack start"` could kill an editor, a grep, or an unrelated tool).
/// A PID file records the one true process so `stop` signals precisely it.
const SLACK_PID_FILE: &str = "slack.pid";

/// Absolute path to the Slack bot PID file (`~/.trusty-mpm/slack.pid`).
///
/// Why: `start` (writer) and `stop` (reader) must agree on one location; deriving
/// it from the shared [`FRAMEWORK_DIR_NAME`](crate::core::paths::FRAMEWORK_DIR_NAME)
/// home root keeps it consistent with the rest of the framework's state files.
/// What: `~/.trusty-mpm/slack.pid`, falling back to `./.trusty-mpm/slack.pid` when
/// the home directory cannot be resolved (mirrors `FrameworkPaths::default`).
/// Test: `pid_file_path_is_under_framework_root`.
pub fn pid_file_path() -> std::path::PathBuf {
    let home = dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from("."));
    home.join(crate::core::paths::FRAMEWORK_DIR_NAME)
        .join(SLACK_PID_FILE)
}

/// Record the current process id in the Slack PID file.
///
/// Why: so `tm slack stop` can signal exactly this foreground bot instead of
/// pattern-matching process argv. Written once at startup, removed on stop.
/// What: creates `~/.trusty-mpm` if needed and writes the current PID as text.
/// A write failure is logged (stderr) and swallowed — an unwritable PID file must
/// not prevent the bot from running; `stop` simply falls back to a manual kill.
/// Test: `pid_file_guard_removes_on_drop` exercises the sibling guard; the write
/// itself is side-effect-only (creates a real file) and covered manually.
pub(crate) fn write_pid_file() {
    let path = pid_file_path();
    if let Some(parent) = path.parent()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        tracing::warn!("could not create Slack PID dir {}: {e}", parent.display());
        return;
    }
    if let Err(e) = std::fs::write(&path, std::process::id().to_string()) {
        tracing::warn!("could not write Slack PID file {}: {e}", path.display());
    }
}

/// RAII guard that best-effort removes the Slack PID file when dropped.
///
/// Why: [`run`](super::run) writes the PID file at startup so `tm slack stop` can
/// signal exactly this process, but a clean exit (e.g. a permanent `disconnect`),
/// an early `?` return, or a panic would otherwise leave a STALE PID file behind —
/// a later `tm slack stop` could then SIGTERM a recycled, unrelated PID. Holding
/// the path in a `Drop` guard removes the file on EVERY exit path of `run`.
/// What: on drop, removes the file at `path`. Best-effort: a `NotFound` is
/// expected (e.g. `tm slack stop` already removed it) and silent; any other
/// removal error is logged to stderr and swallowed — cleanup must never panic.
/// Test: `pid_file_guard_removes_on_drop`, `pid_file_guard_drop_missing_is_silent`.
pub(crate) struct PidFileGuard {
    /// The PID-file path to remove on drop.
    pub(crate) path: std::path::PathBuf,
}

impl Drop for PidFileGuard {
    fn drop(&mut self) {
        match std::fs::remove_file(&self.path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                tracing::warn!(
                    "could not remove Slack PID file {} on exit: {e}",
                    self.path.display()
                );
            }
        }
    }
}

/// The result of a `tm slack stop` attempt, for a precise operator message.
///
/// Why: `stop` must report honestly — it should NOT print "no process found" when
/// it actually hit an error (an unreadable PID file or a failed signal). The old
/// `pkill` path conflated "no match" with "pkill itself errored"; a typed outcome
/// keeps the three cases distinct.
/// What: `Stopped(pid)` signalled a live bot; `NotRunning` found no PID file or a
/// stale one (process already gone); `Failed(msg)` is a real error.
/// Test: `stop_via_pid_file_*` cover the missing-file and stale-pid paths.
#[derive(Debug, PartialEq, Eq)]
pub enum StopOutcome {
    /// A running bot (this PID) was signalled to stop.
    Stopped(u32),
    /// No bot was running (no PID file, or the recorded PID is already gone).
    NotRunning,
    /// Stopping failed for a real reason (e.g. an unreadable PID file).
    Failed(String),
}

/// Stop the foreground Slack bot recorded in the PID file at `path`.
///
/// Why: this is the precise replacement for `pkill -f "slack start"` — it signals
/// exactly the process `tm slack start` recorded, so it can never kill an
/// unrelated process, and it distinguishes "not running" from a genuine failure.
/// Taking `path` as a parameter keeps it unit-testable against a temp file.
/// What: reads the PID, sends `SIGTERM` to it (Unix), removes the PID file, and
/// returns a [`StopOutcome`]. A missing PID file → `NotRunning`; a recorded PID
/// that no longer exists → `NotRunning` (stale file, still cleaned up); an
/// unreadable/garbage PID file or a signal error other than "no such process" →
/// `Failed`.
/// Test: `stop_via_pid_file_missing_is_not_running`,
/// `stop_via_pid_file_stale_pid_is_not_running`.
pub fn stop_via_pid_file(path: &Path) -> StopOutcome {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return StopOutcome::NotRunning,
        Err(e) => return StopOutcome::Failed(format!("could not read PID file: {e}")),
    };
    let pid: u32 = match raw.trim().parse() {
        Ok(pid) => pid,
        Err(e) => {
            // A corrupt PID file is unusable; remove it so the next start is clean.
            let _ = std::fs::remove_file(path);
            return StopOutcome::Failed(format!("PID file is not a valid pid: {e}"));
        }
    };
    let outcome = signal_terminate(pid);
    // Always clear the PID file once we have acted on it (stopped or stale).
    let _ = std::fs::remove_file(path);
    outcome
}

/// Send `SIGTERM` to `pid`, classifying the result.
///
/// Why: `stop_via_pid_file` must distinguish a successful signal from "the process
/// is already gone" (a stale PID file, not an error) and from a real failure.
/// Isolating the raw `kill(2)` call keeps that classification in one place.
/// What: on Unix, calls `libc::kill(pid, SIGTERM)`; `ESRCH` (no such process) maps
/// to [`StopOutcome::NotRunning`], any other errno to [`StopOutcome::Failed`], and
/// success to [`StopOutcome::Stopped`]. On non-Unix it reports unsupported.
/// Test: covered indirectly via `stop_via_pid_file_stale_pid_is_not_running`.
fn signal_terminate(pid: u32) -> StopOutcome {
    #[cfg(unix)]
    {
        // SAFETY: `kill` is async-signal-safe and merely posts a signal; passing a
        // pid and a constant signal number has no memory-safety implications.
        let rc = unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
        if rc == 0 {
            return StopOutcome::Stopped(pid);
        }
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::ESRCH) {
            // The process is already gone — a stale PID file, not a failure.
            StopOutcome::NotRunning
        } else {
            StopOutcome::Failed(format!("failed to signal pid {pid}: {err}"))
        }
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        StopOutcome::Failed("stopping the Slack bot is only supported on Unix".to_string())
    }
}
