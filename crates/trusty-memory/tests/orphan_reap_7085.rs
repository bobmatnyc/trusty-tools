//! A test-spawned `serve --foreground` daemon must die with its spawner even
//! when that spawner is SIGKILLed (#7085).
//!
//! Why: `DaemonGuard` (#5188) kills the daemon in `Drop`, which covers a panic
//! unwind and a clean return. It does not cover SIGKILL — a `cargo test`
//! timeout, an interrupted run, a torn-down agent process tree — because no
//! destructor runs at all. A census on 2026-09-08 found 102 orphaned
//! `target/debug/trusty-memory serve --foreground` daemons from test runs,
//! holding 12.6 GB against temp data dirs their spawners had already deleted.
//! Only a mechanism inside the daemon can close that class, so only a test that
//! actually SIGKILLs a spawner can prove it closed.
//!
//! What: this binary re-enters itself. [`spawner_child_mode`] is an `#[ignore]`d
//! helper that, when handed [`ENV_PIDFILE`], spawns the daemon through the real
//! `trusty_common::parent_death::exit_with_parent` helper, records its pid, and
//! then blocks forever. [`foreground_daemon_exits_when_its_spawner_is_sigkilled`]
//! runs that helper as a child, waits for the daemon to bind its socket, SIGKILLs
//! the helper outright, and asserts the daemon's pid disappears. Re-entry rather
//! than a shell one-liner is what keeps the spawner half of the contract — the
//! Rust helper — under test rather than a hand-copied env assignment.
//!
//! Not `#[serial_test::file_serial]`: every path here is inside a fresh
//! tempdir, the daemon binds only that dir's socket, and the liveness assertion
//! names one pid this test spawned. Nothing is shared with a sibling test
//! binary, so serialising would cost wall-clock for no isolation.
//!
//! Test: `cargo test -p trusty-memory --test orphan_reap_7085`.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Where [`spawner_child_mode`] writes the daemon's pid, and the flag that puts
/// this binary into spawner mode at all.
const ENV_PIDFILE: &str = "TRUSTY_7085_PIDFILE";

/// The isolated data root [`spawner_child_mode`] hands the daemon.
const ENV_DATA_DIR: &str = "TRUSTY_7085_DATA_DIR";

/// How long the daemon gets to boot far enough to bind its socket.
const BOOT_TIMEOUT: Duration = Duration::from_secs(30);

/// How long the daemon gets to notice its spawner died and exit.
///
/// Why: the watchdog polls every 250 ms, so the real bound is under a second.
/// 20 s is slack for a loaded CI box, and it still fails fast against the
/// pre-fix behavior, where the daemon never exits at all.
const REAP_TIMEOUT: Duration = Duration::from_secs(20);

/// Poll interval for every wait in this file.
const POLL: Duration = Duration::from_millis(100);

/// The `trusty-memory` binary Cargo built for this test run.
fn binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_trusty-memory"))
}

/// True while a process with `pid` exists.
///
/// What: `kill` with signal 0 delivers nothing and reports only whether the
/// target is signalable; `ESRCH` is the "gone" answer this test waits for.
fn pid_alive(pid: u32) -> bool {
    // SAFETY: `kill` with signal 0 performs existence and permission checks
    // only, delivering no signal.
    let rc = unsafe { libc::kill(pid as libc::pid_t, 0) };
    rc == 0
        || matches!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::EPERM)
        )
}

/// SIGKILL `pid`, ignoring the result. Used only for cleanup on a failure path.
fn hard_kill(pid: u32) {
    // SAFETY: `kill` is safe to call with any pid; a stale pid returns ESRCH.
    unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
}

/// Block until `path` exists, returning `false` on timeout.
fn wait_for_path(path: &Path, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while !path.exists() {
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(POLL);
    }
    true
}

/// Re-entry helper: spawn the daemon the way a real test does, then block.
///
/// Why: the thing under test is what happens when the daemon's SPAWNER is
/// killed, and a test cannot SIGKILL itself and then assert. This mode gives the
/// daemon a parent that is killable and that is not the assertion process.
/// What: does nothing at all unless [`ENV_PIDFILE`] is set, so an ordinary
/// `--include-ignored` run cannot block on it. With the var set, spawns
/// `serve --foreground` through
/// [`trusty_common::parent_death::exit_with_parent`], writes the child pid to
/// the named file, and then sleeps until killed.
/// Test: driven by `foreground_daemon_exits_when_its_spawner_is_sigkilled`.
#[test]
#[ignore = "re-entry helper, run as a child by foreground_daemon_exits_when_its_spawner_is_sigkilled"]
fn spawner_child_mode() {
    let Ok(pidfile) = std::env::var(ENV_PIDFILE) else {
        return;
    };
    let data_dir = std::env::var(ENV_DATA_DIR).expect("spawner mode needs a data dir");

    let mut child = trusty_common::parent_death::exit_with_parent(
        std::process::Command::new(binary())
            .arg("serve")
            .arg("--foreground")
            .env("TRUSTY_DATA_DIR_OVERRIDE", &data_dir)
            .env("TRUSTY_SKIP_PALACE_ENFORCEMENT", "1")
            .env("RUST_LOG", "warn")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null()),
    )
    .spawn()
    .expect("spawn daemon");

    std::fs::write(&pidfile, child.id().to_string()).expect("record daemon pid");

    // Block on the daemon. Our caller SIGKILLs this process long before the wait
    // returns; waiting rather than sleeping also reaps the daemon in the
    // uninteresting case where it exits on its own first.
    let _ = child.wait();
}

/// SIGKILL the process that spawned the daemon and assert the daemon follows.
///
/// Why (#7085): this is the exact shape that produced 102 orphans — the spawner
/// dies without running a destructor. Before the fix the daemon survives
/// indefinitely and this test fails on [`REAP_TIMEOUT`]; after it, the daemon's
/// own watchdog notices the reparent and exits.
/// What: runs [`spawner_child_mode`] as a child of this test, waits for the
/// daemon to bind its socket (proving it got past the point where the watchdog
/// arms), SIGKILLs the spawner, reaps it, and polls the daemon's pid until it is
/// gone. Cleans up the daemon itself on the failure path so a red run cannot
/// leave behind the very orphan it is about.
#[test]
fn foreground_daemon_exits_when_its_spawner_is_sigkilled() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let pidfile = tmp.path().join("daemon.pid");

    let mut spawner =
        std::process::Command::new(std::env::current_exe().expect("locate this test binary"))
            .args([
                "--exact",
                "spawner_child_mode",
                "--ignored",
                "--test-threads",
                "1",
            ])
            .env(ENV_PIDFILE, &pidfile)
            .env(ENV_DATA_DIR, tmp.path())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn the re-entry helper");

    assert!(
        wait_for_path(&pidfile, BOOT_TIMEOUT),
        "the helper never recorded a daemon pid at {}",
        pidfile.display()
    );
    let daemon_pid: u32 = std::fs::read_to_string(&pidfile)
        .expect("read daemon pid")
        .trim()
        .parse()
        .expect("daemon pid must be numeric");

    // Wait for the daemon to bind its socket, so we know startup ran far enough
    // to arm the watchdog. Without this the test could kill the spawner during
    // the daemon's first few milliseconds and prove nothing.
    let socket = tmp.path().join("trusty-memory").join("trusty-memory.sock");
    if !wait_for_path(&socket, BOOT_TIMEOUT) {
        hard_kill(daemon_pid);
        let _ = spawner.kill();
        let _ = spawner.wait();
        panic!(
            "daemon {daemon_pid} never bound {} within {BOOT_TIMEOUT:?}",
            socket.display()
        );
    }

    // The whole point: kill the spawner outright. No `Drop`, no unwind, no
    // teardown of any kind runs in that process.
    spawner.kill().expect("SIGKILL the helper");
    spawner.wait().expect("reap the helper");

    let deadline = Instant::now() + REAP_TIMEOUT;
    while pid_alive(daemon_pid) {
        if Instant::now() >= deadline {
            hard_kill(daemon_pid);
            panic!(
                "daemon {daemon_pid} was still running {REAP_TIMEOUT:?} after its spawner was \
                 SIGKILLed — a `cargo test` run that is killed outright leaks this daemon \
                 (#7085). Killed it here so this failure does not add another orphan."
            );
        }
        std::thread::sleep(POLL);
    }
}
