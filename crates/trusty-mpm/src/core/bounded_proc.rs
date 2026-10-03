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
//! pipes on their own threads, polls for the exit until `budget` expires, then
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
//! `run_bounded_reports_a_spawn_failure`, `run_bounded_with_input_feeds_stdin`,
//! `run_bounded_reports_a_pipe_held_open_after_exit`,
//! `run_bounded_kills_the_pipe_holder_it_reports_held_open` in
//! `bounded_proc_tests.rs`.

use std::cell::Cell;
use std::io::{Read, Write};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

thread_local! {
    // #8301: the wall-clock deadline every bounded child on this thread obeys.
    static DEADLINE: Cell<Option<Instant>> = const { Cell::new(None) };
}

/// Run `f` with every bounded child it starts on this thread cut off at
/// `deadline` (#8301).
///
/// Why: each child already has its own budget, but a survey is a loop of many
/// children, and the sum of their budgets is not a bound. A merged-PR preview
/// over 238 worktrees ran 78 minutes, one bounded call at a time.
/// What: installs `deadline` (the earlier one, when a deadline is already in
/// force), runs `f`, and restores the previous value through a drop guard, so
/// a panic cannot leak a deadline onto a pooled thread.
/// Test: `a_deadline_cuts_a_child_short_and_refuses_the_next_one`.
pub fn with_deadline<T>(deadline: Instant, f: impl FnOnce() -> T) -> T {
    struct Restore(Option<Instant>);
    impl Drop for Restore {
        fn drop(&mut self) {
            DEADLINE.with(|d| d.set(self.0));
        }
    }
    let outer = DEADLINE.with(Cell::get);
    let effective = outer.map_or(deadline, |o| o.min(deadline));
    let _restore = Restore(DEADLINE.with(|d| d.replace(Some(effective))));
    f()
}

/// `budget`, shortened to the time left before this thread's deadline (#8301).
///
/// What: `budget` unchanged when no deadline is in force; `None` once the
/// deadline has passed, which [`run_bounded_with_input`] reports as
/// [`BoundedError::TimedOut`] without spawning anything.
/// Test: `a_deadline_cuts_a_child_short_and_refuses_the_next_one`.
pub fn clamp_to_deadline(budget: Duration) -> Option<Duration> {
    let Some(deadline) = DEADLINE.with(Cell::get) else {
        return Some(budget);
    };
    let left = deadline.saturating_duration_since(Instant::now());
    (!left.is_zero()).then(|| budget.min(left))
}

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
pub struct BoundedOutput {
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
pub enum BoundedError {
    /// The child could not be started.
    Spawn(std::io::Error),
    /// The child started but exposed no pipe for the named stream.
    NoPipe(&'static str),
    /// The child outlived `budget` and its process group was killed.
    TimedOut,
    /// `try_wait` itself failed; the child's fate is unknown.
    Wait(std::io::Error),
    /// The child exited, but something it left behind still held the named
    /// pipe open, so its output is incomplete (#8306).
    PipeHeldOpen(&'static str),
}

impl std::fmt::Display for BoundedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Spawn(e) => write!(f, "could not be run: {e}"),
            Self::NoPipe(which) => write!(f, "exposed no {which} pipe"),
            Self::TimedOut => write!(f, "did not answer within its budget"),
            Self::Wait(e) => write!(f, "could not be waited on: {e}"),
            Self::PipeHeldOpen(which) => {
                write!(
                    f,
                    "exited, but its {which} pipe stayed open past the drain wait"
                )
            }
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

/// SIGKILL the failed child's whole process group, then reap it (#6867).
///
/// What: `killpg` on the child's pid — which [`isolate_process_group`] made the
/// group id — then the direct kill and the `wait` that reaps the zombie. The
/// group is signalled BEFORE the reap: once `wait` returns the pid may be
/// recycled and the group id would name someone else's processes. The child
/// must be running or an unreaped zombie; [`supervise`] never reaps (#8306).
/// Test: `run_bounded_kills_the_whole_process_group`,
/// `run_bounded_kills_the_pipe_holder_it_reports_held_open`.
#[cfg(unix)]
fn kill_child_group(child: &mut Child) {
    if let Ok(pgid) = libc::pid_t::try_from(child.id()) {
        // SAFETY: `child` has not been waited on yet — running or a zombie — so
        // its pid is still reserved and, because the child was spawned with
        // `process_group(0)`, names its own process group (never 0, never the
        // caller's). A group with no members left returns ESRCH, ignored.
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
/// their own threads, polls for the exit every 25 ms until `budget` expires,
/// then kills the GROUP and reaps. `Ok` carries the exit status and both streams
/// even for a non-zero exit; every `Err` means no output was produced. Every
/// error after the spawn kills the group before returning (#7652 critic round 2,
/// #8306). The kill on an errored wait carries no test of its own: `waitid`
/// fails only with `ECHILD` (a reaped or stolen child), which this crate cannot
/// provoke without a wait seam whose only user would be that test. It shares
/// the one kill site with the tested timeout and held-open arms.
/// Test: `run_bounded_captures_stdout_and_status`, `run_bounded_kills_a_hung_child`,
/// `run_bounded_kills_the_whole_process_group`, `run_bounded_reports_a_spawn_failure`.
pub fn run_bounded(cmd: Command, budget: Duration) -> Result<BoundedOutput, BoundedError> {
    run_bounded_with_input(cmd, None, budget)
}

/// [`run_bounded`], writing `input` to the child's stdin first (#8306).
///
/// Why: `git patch-id` reads its patch on stdin. A blocking `write_all` into a
/// child that never reads is its own unbounded wait, so the write runs on a
/// thread the deadline does not wait for.
/// What: with `Some(input)`, stdin is piped, written and closed on its own
/// thread; with `None`, stdin is whatever `cmd` already set. The poll starts at
/// 1 ms and backs off to 25 ms, so a fast git call is not charged a full poll.
/// A pipe still held open after the exit is [`BoundedError::PipeHeldOpen`],
/// never an empty `Ok` a caller would read as "no output".
/// Test: `run_bounded_with_input_feeds_stdin`,
/// `run_bounded_reports_a_pipe_held_open_after_exit`,
/// `run_bounded_kills_the_pipe_holder_it_reports_held_open`.
pub fn run_bounded_with_input(
    mut cmd: Command,
    input: Option<Vec<u8>>,
    budget: Duration,
) -> Result<BoundedOutput, BoundedError> {
    // #8301: the caller's deadline caps this child's own budget; past it,
    // nothing is spawned and the call reads as a timeout — an unknown answer.
    let Some(budget) = clamp_to_deadline(budget) else {
        return Err(BoundedError::TimedOut);
    };
    // #6867: BEFORE the spawn — a group cannot be joined retroactively.
    isolate_process_group(&mut cmd);
    if input.is_some() {
        cmd.stdin(Stdio::piped());
    }
    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(BoundedError::Spawn)?;
    if let (Some(input), Some(mut stdin)) = (input, child.stdin.take()) {
        // A child killed mid-write fails the write with EPIPE; nothing waits on it.
        std::thread::spawn(move || {
            let _ = stdin.write_all(&input);
        });
    }
    match supervise(&mut child, budget) {
        // Every pipe reached EOF, so no pipe holder is left to kill; the zombie
        // is reaped here for its status. This reap fails only with ECHILD — the
        // pid is no longer ours — so it is deliberately NOT followed by a
        // `killpg`, which could then land on a recycled pid's group.
        Ok((stdout, stderr)) => Ok(BoundedOutput {
            status: child.wait().map_err(BoundedError::Wait)?,
            stdout,
            stderr,
        }),
        // #8306: EVERY post-spawn error — TimedOut, Wait, NoPipe, PipeHeldOpen —
        // kills the group before returning. A held-open pipe means a grandchild
        // is still running; returning without the kill leaked it.
        Err(e) => {
            kill_child_group(&mut child);
            Err(e)
        }
    }
}

/// Wait for `child` to exit and drain both of its pipes, WITHOUT reaping it.
///
/// Why (#8306): the caller must be able to kill the child's process group on any
/// error here, and the group id is the child's pid. An unreaped zombie keeps that
/// pid reserved, so the `killpg` cannot land on a recycled pid's group.
/// What: `Ok` carries both streams once the child has exited and both pipes hit
/// EOF; the child is left a zombie for the caller to reap. Every `Err` leaves it
/// either running or an unreaped zombie.
/// Test: `run_bounded_kills_a_hung_child`,
/// `run_bounded_kills_the_pipe_holder_it_reports_held_open`.
fn supervise(child: &mut Child, budget: Duration) -> Result<(String, String), BoundedError> {
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
    let mut pause = Duration::from_millis(1);
    // #7652 critic round 2: an errored wait leaves the loop without reaching
    // the deadline; `?` hands it to the caller's group kill like the timeout.
    while !has_exited(child).map_err(BoundedError::Wait)? {
        if Instant::now() >= deadline {
            return Err(BoundedError::TimedOut);
        }
        std::thread::sleep(pause);
        pause = (pause * 2).min(Duration::from_millis(25));
    }
    // #8306: an undrained pipe is an error, never an empty success.
    let out = stdout
        .recv_timeout(PIPE_DRAIN_WAIT)
        .map_err(|_| BoundedError::PipeHeldOpen("stdout"))?;
    let err = stderr
        .recv_timeout(PIPE_DRAIN_WAIT)
        .map_err(|_| BoundedError::PipeHeldOpen("stderr"))?;
    Ok((out, err))
}

/// Has `child` exited? Leaves an exited child unreaped (#8306).
///
/// What: `waitid(WEXITED | WNOHANG | WNOWAIT)`. A zeroed `si_pid` means no
/// child changed state — the portable reading, since macOS returns 0 without
/// filling `siginfo_t` under `WNOHANG`.
/// Test: `run_bounded_kills_the_pipe_holder_it_reports_held_open`.
#[cfg(unix)]
fn has_exited(child: &Child) -> std::io::Result<bool> {
    let pid = libc::id_t::from(child.id());
    // SAFETY: an all-zero `siginfo_t` is a valid value of this plain C struct.
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    loop {
        // SAFETY: `info` is a valid, writable `siginfo_t`; `pid` is our own
        // unreaped child, and WNOWAIT leaves it unreaped.
        let rc = unsafe {
            libc::waitid(
                libc::P_PID,
                pid,
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if rc == 0 {
            // SAFETY: `si_pid` is valid for a SIGCHLD-shaped `siginfo_t`, and
            // the zeroed value reads 0 when `waitid` filled nothing.
            return Ok(unsafe { info.si_pid() } != 0);
        }
        let err = std::io::Error::last_os_error();
        if err.kind() != std::io::ErrorKind::Interrupted {
            return Err(err);
        }
    }
}

#[cfg(not(unix))]
fn has_exited(child: &mut Child) -> std::io::Result<bool> {
    child.try_wait().map(|status| status.is_some())
}

#[cfg(test)]
#[path = "bounded_proc_tests.rs"]
mod bounded_proc_tests;
