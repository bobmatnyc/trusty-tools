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
//! the helper outright, and asserts the daemon's pid disappears AND that it
//! unlinked its socket on the way out. Re-entry rather than a shell one-liner is
//! what keeps the spawner half of the contract — the Rust helper — under test
//! rather than a hand-copied env assignment.
//!
//! Why the socket assertion (#7085 review): a vanished pid says only that the
//! process ended. Replacing the watchdog's body with a bare `process::exit`
//! would satisfy it. Only `serve_with_shutdown` removes the socket file, and it
//! reaches that line only by running the daemon's real SIGTERM path, so the
//! file's absence is what separates a graceful exit from any other death.
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

/// Set on the spawner to make it reach the daemon through a bridge level, which
/// is what makes the daemon a detached grandchild rather than a direct child.
const ENV_VIA_BRIDGE: &str = "TRUSTY_7085_VIA_BRIDGE";

/// Where [`bridge_child_mode`] writes the detached daemon's pid.
const ENV_BRIDGE_PIDFILE: &str = "TRUSTY_7085_BRIDGE_PIDFILE";

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

/// The parent pid of `pid`, via `ps`.
///
/// Why `ps` rather than a `proc_pidinfo` call: this is one assertion in one
/// test, and the platform-specific struct plumbing would be a second copy of
/// what `trusty_common::parent_death` already owns.
fn ppid_of(pid: u32) -> Option<u32> {
    let out = std::process::Command::new("ps")
        .args(["-o", "ppid=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    String::from_utf8(out.stdout).ok()?.trim().parse().ok()
}

/// Block until `pid` is reparented to init, returning `false` on timeout.
///
/// Why the poll: the bridge writes the daemon's pid and only THEN exits, so
/// there is a short window in which the daemon's ppid is still the bridge.
fn wait_for_reparent_to_init(pid: u32, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while ppid_of(pid) != Some(1) {
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
/// the named file, and then blocks until killed. With [`ENV_VIA_BRIDGE`] also
/// set it inserts one more level first — see [`bridge_child_mode`].
/// Test: driven by `foreground_daemon_exits_when_its_spawner_is_sigkilled` and
/// `detached_grandchild_daemon_exits_when_its_spawner_is_sigkilled`.
#[test]
#[ignore = "re-entry helper, run as a child by the two orphan-reap tests"]
fn spawner_child_mode() {
    let Ok(pidfile) = std::env::var(ENV_PIDFILE) else {
        return;
    };
    let data_dir = std::env::var(ENV_DATA_DIR).expect("spawner mode needs a data dir");

    if std::env::var(ENV_VIA_BRIDGE).is_ok() {
        run_via_bridge(&pidfile, &data_dir);
        return;
    }

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

/// Spawner-mode variant that puts a short-lived bridge between it and the daemon.
///
/// Why: the production shape that produced most of the orphans is not a direct
/// child. A stdio bridge auto-starts the daemon DETACHED and then exits, so the
/// daemon's parent is gone within milliseconds and only the stamped pid's
/// identity can speak for whether the run that wanted it is still alive.
/// What: stamps a re-entry into [`bridge_child_mode`] with THIS process's pid,
/// waits for that bridge to exit (as a real bridge does at EOF), then blocks.
/// The daemon it left behind is a grandchild whose ppid is 1.
fn run_via_bridge(pidfile: &str, data_dir: &str) {
    let status = trusty_common::parent_death::exit_with_parent(
        std::process::Command::new(std::env::current_exe().expect("locate this test binary"))
            .args([
                "--exact",
                "bridge_child_mode",
                "--ignored",
                "--test-threads",
                "1",
            ])
            .env(ENV_BRIDGE_PIDFILE, pidfile)
            .env("TRUSTY_DATA_DIR_OVERRIDE", data_dir)
            .env("TRUSTY_SKIP_PALACE_ENFORCEMENT", "1")
            .env("RUST_LOG", "warn")
            .env_remove(ENV_PIDFILE)
            .env_remove(ENV_VIA_BRIDGE)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null()),
    )
    .status()
    .expect("run the bridge helper");
    assert!(status.success(), "the bridge helper must exit cleanly");

    // The bridge is gone and the daemon is reparented to pid 1. Block until our
    // own caller SIGKILLs us.
    loop {
        std::thread::sleep(Duration::from_secs(60));
    }
}

/// Re-entry helper standing in for a stdio bridge: start the daemon DETACHED and
/// exit immediately.
///
/// Why: this is the call that produced six orphans per full suite run — the
/// bridge starts the daemon on behalf of whoever started the bridge, and then
/// leaves. It goes through the real
/// [`trusty_common::daemon_guard::spawn_detached_forwarding_parent_link`], so the
/// test covers the forwarding decision rather than a hand-copied env assignment.
/// What: does nothing unless [`ENV_BRIDGE_PIDFILE`] is set. With it, spawns
/// `serve --foreground` detached, records the pid, and returns.
/// Test: driven by `detached_grandchild_daemon_exits_when_its_spawner_is_sigkilled`.
#[test]
#[ignore = "re-entry helper, run as a child by detached_grandchild_daemon_exits_when_its_spawner_is_sigkilled"]
fn bridge_child_mode() {
    let Ok(pidfile) = std::env::var(ENV_BRIDGE_PIDFILE) else {
        return;
    };
    let pid = trusty_common::daemon_guard::spawn_detached_forwarding_parent_link(
        binary(),
        &["serve", "--foreground"],
    )
    .expect("detached daemon spawn");
    std::fs::write(&pidfile, pid.to_string()).expect("record daemon pid");
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
    assert_daemon_dies_with_its_spawner(false);
}

/// The same proof for the DETACHED grandchild, which is where most of the
/// orphans actually came from.
///
/// Why (#7085 review): the direct-child case is caught by the reparent prong,
/// which a grandchild does not have. Six daemons per full `trusty-memory` suite
/// run were started by a stdio bridge that exits immediately, leaving a daemon
/// whose ppid is 1 from the first moment. Only the stamped process's identity —
/// pid plus start time — can say whether that daemon is still wanted, so it
/// needs its own end-to-end proof.
/// What: as the test above, except the helper inserts a bridge level that starts
/// the daemon through `spawn_detached_forwarding_parent_link` and exits.
#[test]
fn detached_grandchild_daemon_exits_when_its_spawner_is_sigkilled() {
    assert_daemon_dies_with_its_spawner(true);
}

/// Drive one scenario end to end: start a killable spawner, let it produce a
/// daemon, SIGKILL it, and require the daemon to be gone inside [`REAP_TIMEOUT`].
///
/// `via_bridge` selects the shape: `false` spawns the daemon as the spawner's
/// direct child, `true` routes it through a short-lived bridge so the daemon is
/// a detached grandchild.
fn assert_daemon_dies_with_its_spawner(via_bridge: bool) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let pidfile = tmp.path().join("daemon.pid");

    let mut cmd =
        std::process::Command::new(std::env::current_exe().expect("locate this test binary"));
    cmd.args([
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
    .stderr(std::process::Stdio::null());
    if via_bridge {
        cmd.env(ENV_VIA_BRIDGE, "1");
    }
    let mut spawner = cmd.spawn().expect("spawn the re-entry helper");

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

    // #7085 review: in the bridge shape the daemon must ALREADY be an orphan by
    // parentage before the spawner dies. If it were still a descendant of the
    // spawner, the reparent prong would carry this test and the identity prong —
    // the only thing a real detached daemon has — would go unproven.
    if via_bridge {
        let ppid = ppid_of(daemon_pid);
        assert!(
            wait_for_reparent_to_init(daemon_pid, BOOT_TIMEOUT),
            "daemon {daemon_pid} was still parented to {ppid:?} rather than init, so this \
             scenario would not exercise the detached-grandchild path"
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

    // #7085 review: the pid disappearing proves only that the process ended,
    // which a bare `process::exit` inside the watchdog would satisfy just as
    // well. `serve_with_shutdown` unlinks the socket on its way out and nothing
    // else does, so the file being gone is the assertion only a shutdown that
    // ran the daemon's own SIGTERM path can pass. The unlink happens before the
    // process exits, so no further wait is needed here.
    assert!(
        !socket.exists(),
        "daemon {daemon_pid} exited but left {} behind — the watchdog must route \
         through the daemon's SIGTERM handler so the socket is unlinked and the \
         indexes flushed, not exit the process outright",
        socket.display()
    );
}
