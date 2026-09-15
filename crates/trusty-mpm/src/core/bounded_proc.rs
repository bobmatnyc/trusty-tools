//! The workspace's one kill-on-timeout runner for a synchronous subprocess
//! (#7965).
//!
//! Why: `std::process::Command::output()` waits forever. Every background sweep
//! the daemon runs — the merged-PR worktree reclaim, the in-project hygiene pass
//! — is a loop of `git`/`gh`/`tmux` subprocesses executed on the tokio blocking
//! pool, and #7965 caught two of those blocking-pool threads parked in
//! `small_probe_read`: a child's stdout that never reached EOF. A sweep that
//! cannot finish keeps consuming the one resource every HTTP handler also needs
//! — this process's CPU and its share of the host's IO — for as long as the
//! daemon lives, which is how a background chore made `GET /health` miss the
//! client's 500 ms discovery probe
//! ([`crate::core::discovery`]) and every `tm` command report the daemon
//! unreachable.
//!
//! What: [`run_bounded`] spawns the child in its OWN process group, drains both
//! pipes on their own threads, polls `try_wait` until `budget` expires, then
//! SIGKILLs the whole group and reaps it. It is the mechanics
//! [`crate::session_manager::worktree_reclaim_gh::run_with_timeout`] has used
//! since #6867, lifted out verbatim so the hygiene sweep does not get a second,
//! subtly different copy; that function is now a thin adapter that maps
//! [`BoundedError`] onto its own `gh`-specific failure taxonomy.
//!
//! # Fail direction
//!
//! Toward a reported failure, never toward a silent success. A timeout, a spawn
//! error and a wait error are all distinct `Err` variants; none of them yields an
//! empty-but-successful [`BoundedOutput`] that a caller could read as "the
//! command ran and found nothing".
//!
//! Test: `run_bounded_captures_stdout_and_status`,
//! `run_bounded_kills_a_hung_child`, `run_bounded_kills_the_whole_process_group`,
//! `run_bounded_reports_a_spawn_failure` in `bounded_proc_tests.rs`.

use std::io::Read;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

/// How long to wait for a drained pipe's thread to hand over what it read.
///
/// Why: the child has already exited or been killed by the time this is used, so
/// the reader thread is at EOF and answers immediately in the normal case. The
/// wait exists only so a wedged reader cannot re-introduce the unbounded wait
/// this module removes.
/// Test: covered through `run_bounded_captures_stdout_and_status`.
const PIPE_DRAIN_WAIT: Duration = Duration::from_secs(2);

/// A finished child's exit status and both captured streams.
///
/// Test: `run_bounded_captures_stdout_and_status`.
#[derive(Debug)]
pub(crate) struct BoundedOutput {
    /// The child's exit status. A non-zero status is NOT an error here — the
    /// caller decides what a non-zero exit means for its own command.
    pub status: ExitStatus,
    /// Everything the child wrote to stdout, lossily decoded.
    pub stdout: String,
    /// Everything the child wrote to stderr, lossily decoded.
    pub stderr: String,
}

/// Why a bounded run produced no output at all.
///
/// Why a typed enum rather than a string: the `gh` adapter counts TIMEOUTS
/// specifically for its consecutive-failure backoff
/// ([`crate::session_manager::worktree_reclaim_gh_gate`]), and an auth failure
/// that returns instantly must never be counted as one.
/// Test: `run_bounded_kills_a_hung_child`, `run_bounded_reports_a_spawn_failure`.
#[derive(Debug)]
pub(crate) enum BoundedError {
    /// The child could not be started.
    Spawn(std::io::Error),
    /// The child started but exposed no pipe for the named stream.
    NoPipe(&'static str),
    /// The child outlived `budget` and its process group was killed.
    TimedOut,
    /// `try_wait` itself failed; the child's fate is unknown.
    Wait(std::io::Error),
}

impl std::fmt::Display for BoundedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Spawn(e) => write!(f, "could not be run: {e}"),
            Self::NoPipe(which) => write!(f, "exposed no {which} pipe"),
            Self::TimedOut => write!(f, "did not answer within its budget"),
            Self::Wait(e) => write!(f, "could not be waited on: {e}"),
        }
    }
}

/// Read a child pipe to EOF on its own thread.
///
/// Why: a child that fills a pipe buffer nobody is reading blocks in `write`
/// forever, so polling `try_wait` without draining would deadlock on any command
/// with more output than one pipe buffer.
/// What: moves the pipe onto a thread that reads it to EOF and sends the lossily
/// decoded text down a channel exactly once.
/// Test: covered through `run_bounded_captures_stdout_and_status`.
fn drain_pipe(mut pipe: impl Read + Send + 'static) -> Receiver<String> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = pipe.read_to_end(&mut buf);
        let _ = tx.send(String::from_utf8_lossy(&buf).into_owned());
    });
    rx
}

/// Put the child in its own process group, so the whole tree can be killed.
///
/// Why (#6867): killing only the child's pid leaves its grandchildren — a `gh`
/// credential helper, a `git` transport helper over ssh — running and holding the
/// pipes open, which is the exact shape that made the timeout ineffective.
/// What: `process_group(0)` before the spawn; a group cannot be joined
/// retroactively. A no-op off unix.
/// Test: `run_bounded_kills_the_whole_process_group`.
#[cfg(unix)]
fn isolate_process_group(cmd: &mut Command) {
    use std::os::unix::process::CommandExt;
    cmd.process_group(0);
}

#[cfg(not(unix))]
fn isolate_process_group(_cmd: &mut Command) {}

/// SIGKILL the timed-out child's whole process group, then reap it (#6867).
///
/// What: `killpg` on the child's pid — which [`isolate_process_group`] made the
/// group id — then the direct kill and the `wait` that reaps the zombie. The
/// group is signalled BEFORE the reap: once `wait` returns the pid may be
/// recycled and the group id would name someone else's processes.
/// Test: `run_bounded_kills_the_whole_process_group`.
#[cfg(unix)]
fn kill_child_group(child: &mut Child) {
    if let Ok(pgid) = libc::pid_t::try_from(child.id()) {
        // SAFETY: `child` has not been waited on yet, so its pid is still
        // reserved and — because the child was spawned with `process_group(0)` —
        // names its own process group. A group with no members left returns
        // ESRCH, which is ignored.
        unsafe {
            libc::killpg(pgid, libc::SIGKILL);
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(not(unix))]
fn kill_child_group(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// Run `cmd`, killing its whole process group if it outlives `budget` (#7965).
///
/// Why: see the module doc — an unbounded child is how a background sweep turns
/// into a permanent tax on the request path.
/// What: spawns the child in its own process group with both pipes drained on
/// their own threads, polls `try_wait` every 25 ms until `budget` expires, then
/// kills the GROUP and reaps. `Ok` carries the exit status and both streams even
/// for a non-zero exit; every `Err` means no output was produced.
/// Test: `run_bounded_captures_stdout_and_status`, `run_bounded_kills_a_hung_child`,
/// `run_bounded_kills_the_whole_process_group`, `run_bounded_reports_a_spawn_failure`.
pub(crate) fn run_bounded(
    mut cmd: Command,
    budget: Duration,
) -> Result<BoundedOutput, BoundedError> {
    // #6867: BEFORE the spawn — a group cannot be joined retroactively.
    isolate_process_group(&mut cmd);
    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(BoundedError::Spawn)?;
    let stdout = child
        .stdout
        .take()
        .ok_or(BoundedError::NoPipe("stdout"))
        .map(drain_pipe)?;
    let stderr = child
        .stderr
        .take()
        .ok_or(BoundedError::NoPipe("stderr"))
        .map(drain_pipe)?;
    let deadline = Instant::now() + budget;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                return Ok(BoundedOutput {
                    status,
                    stdout: stdout.recv_timeout(PIPE_DRAIN_WAIT).unwrap_or_default(),
                    stderr: stderr.recv_timeout(PIPE_DRAIN_WAIT).unwrap_or_default(),
                });
            }
            Ok(None) if Instant::now() >= deadline => {
                // #6867: the GROUP, not just the pid.
                kill_child_group(&mut child);
                return Err(BoundedError::TimedOut);
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(25)),
            Err(e) => return Err(BoundedError::Wait(e)),
        }
    }
}

#[cfg(test)]
#[path = "bounded_proc_tests.rs"]
mod bounded_proc_tests;
