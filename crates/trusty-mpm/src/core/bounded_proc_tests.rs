//! Tests for the shared kill-on-timeout subprocess runner (#7965).
//!
//! What is NEW at this layer, and covered nowhere else: the return shape the
//! `gh` adapter throws away — a non-zero exit is `Ok` here, carrying both
//! streams — and the typed timeout the hygiene sweep branches on. The
//! process-group kill and the pipe drain are covered here directly rather than
//! only through `run_with_timeout_kills_the_whole_process_group` in
//! `worktree_reclaim_tests.rs`, because this module now owns them and the `gh`
//! adapter could be retired without taking the guarantee with it.

use std::process::Command;
use std::time::{Duration, Instant};

use super::{BoundedError, run_bounded, run_bounded_with_input};

/// The input variant hands its bytes to the child's stdin and closes it (#7965).
///
/// Why: `git patch-id` reads its patch from stdin; a stdin that is never closed
/// would leave it waiting for more and turn every call into a timeout.
#[test]
fn run_bounded_with_input_feeds_stdin() {
    let mut cmd = Command::new("sh");
    cmd.args(["-c", "wc -c"]);
    let out = run_bounded_with_input(cmd, Some(b"hello".to_vec()), Duration::from_secs(5))
        .expect("a child reading its input must answer");
    assert!(out.status.success(), "{:?}", out.status);
    assert_eq!(out.stdout.trim(), "5");
}

/// A child that never reads its input still times out (#7965).
///
/// Why: 1 MiB is larger than any pipe buffer, so writing it on the caller's
/// thread would block that thread forever and the budget would never be checked.
#[test]
fn run_bounded_with_input_times_out_a_child_that_never_reads() {
    let mut cmd = Command::new("sleep");
    cmd.arg("30");
    let started = Instant::now();
    let err = run_bounded_with_input(cmd, Some(vec![b'x'; 1 << 20]), Duration::from_millis(300))
        .expect_err("a child that never reads must time out");
    assert!(matches!(err, BoundedError::TimedOut), "{err}");
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "the timeout must fire despite the unread input; took {:?}",
        started.elapsed()
    );
}

/// A command that exits non-zero is `Ok` here, with both streams captured.
///
/// Why this is the assertion: the `gh` adapter treats a non-zero exit as a
/// failure, but the hygiene sweep must tell "git refused the fast-forward" (a
/// decision, logged and survivable) from "git never answered" (the #7965 hang).
/// Folding both into `Err` at this layer would erase that distinction.
#[test]
fn run_bounded_captures_stdout_and_status() {
    let mut cmd = Command::new("sh");
    cmd.args(["-c", "echo out; echo err >&2; exit 3"]);
    let out = run_bounded(cmd, Duration::from_secs(5)).expect("a non-zero exit is not an error");
    assert_eq!(out.status.code(), Some(3));
    assert_eq!(out.stdout.trim(), "out");
    assert_eq!(out.stderr.trim(), "err");
}

/// Output larger than one pipe buffer still comes back, and does not deadlock.
///
/// Why: polling `try_wait` without draining the pipes wedges on any command
/// whose output exceeds the buffer — `git status --porcelain` in a dirty
/// checkout, `gh pr list` with 400 pull requests. This is the drain threads'
/// regression test.
#[test]
fn run_bounded_drains_a_pipe_larger_than_its_buffer() {
    let mut cmd = Command::new("sh");
    cmd.args([
        "-c",
        "i=0; while [ $i -lt 20000 ]; do echo 0123456789; i=$((i+1)); done",
    ]);
    let out = run_bounded(cmd, Duration::from_secs(20)).expect("a chatty child must not deadlock");
    assert!(out.status.success(), "{:?}", out.status);
    assert_eq!(out.stdout.lines().count(), 20_000);
}

/// A child that outlives its budget is killed and reported as a TIMEOUT.
///
/// 🔴 #7965 REGRESSION: the variant matters, not just the failure. The hygiene
/// sweep logs a timeout as "this base was abandoned" and keeps going; a spawn
/// error means git is missing. Collapsing them would hide a wedged fetch behind
/// a misleading message.
#[test]
fn run_bounded_kills_a_hung_child() {
    let mut cmd = Command::new("sleep");
    cmd.arg("30");
    let started = Instant::now();
    let err = run_bounded(cmd, Duration::from_millis(300)).expect_err("a hung child must fail");
    assert!(matches!(err, BoundedError::TimedOut), "{err}");
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "the timeout must actually fire; took {:?}",
        started.elapsed()
    );
}

/// 🔴 #6867 REGRESSION, restated at this layer: killing the child must reap its
/// GRANDCHILDREN too.
///
/// Why here as well as through the `gh` adapter: the hygiene sweep's wedge is a
/// `git fetch` whose transport helper (`ssh`, a credential helper) is a
/// grandchild holding the same pipes. `child.kill()` signals one pid and leaves
/// it running, reparented to launchd — which is how #6867 accumulated ~200 orphan
/// pairs in a couple of hours. The stand-in is a shell that backgrounds a long
/// `sleep` and then blocks: one process the runner knows about, one it does not.
#[test]
fn run_bounded_kills_the_whole_process_group() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let pidfile = tmp.path().join("grandchild.pid");
    // The grandchild's fds are redirected so it does not hold the runner's
    // stdout pipe open after its parent dies.
    let script = format!(
        "sleep 300 >/dev/null 2>&1 & echo $! > {}; sleep 300",
        pidfile.display()
    );
    let mut cmd = Command::new("sh");
    cmd.args(["-c", &script]);
    let err = run_bounded(cmd, Duration::from_millis(500)).expect_err("a hung child must fail");
    assert!(matches!(err, BoundedError::TimedOut), "{err}");

    let pid: i32 = std::fs::read_to_string(&pidfile)
        .expect("the shell must have recorded its background child's pid")
        .trim()
        .parse()
        .expect("a pid");

    // The grandchild is reparented on its parent's death, so a killed one is
    // reaped promptly and `kill(pid, 0)` starts reporting ESRCH.
    let deadline = Instant::now() + Duration::from_secs(5);
    while alive(pid) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    let still_there = alive(pid);
    if still_there {
        // Never leave a 300-second sleep behind, whatever the verdict.
        // SAFETY: `pid` was read from a child this test itself started.
        unsafe { libc::kill(pid, libc::SIGKILL) };
    }
    assert!(
        !still_there,
        "the grandchild (pid {pid}) survived its parent's timeout — the process group \
         was not killed (#6867)"
    );
}

/// Is `pid` still a live process?
///
/// SAFETY: `kill(pid, 0)` sends no signal; it only probes for the process's
/// existence and permission to signal it.
#[cfg(unix)]
fn alive(pid: i32) -> bool {
    unsafe { libc::kill(pid, 0) == 0 }
}

/// A binary that is not on PATH is a spawn failure, not a timeout.
#[test]
fn run_bounded_reports_a_spawn_failure() {
    let cmd = Command::new("definitely-not-a-real-binary-7965");
    let err = run_bounded(cmd, Duration::from_secs(5)).expect_err("an unspawnable command fails");
    assert!(matches!(err, BoundedError::Spawn(_)), "{err}");
    assert!(err.to_string().contains("could not be run"), "{err}");
}
