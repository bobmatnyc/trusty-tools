//! A test-started `trusty-search` daemon must not outlive its test (#8900).
//!
//! Why: on 2026-09-29 two debug `trusty-search start --foreground` daemons from
//! a test run were still listening two days later, ppid 1, against temp data
//! dirs their tests had deleted. `Drop` covers a panic or a clean return; only
//! a mechanism inside the daemon covers a SIGKILLed test binary, so only a test
//! that SIGKILLs a spawner can prove it.
//! What: this binary re-enters itself. [`spawner_child_mode`] is an `#[ignore]`d
//! helper that starts a daemon the way the suites do, either through
//! `DaemonGuard` or through a CLI that auto-starts one, then blocks. The tests
//! SIGKILL the helper and require the daemon's pid to
//! disappear inside [`REAP_TIMEOUT`], and its `http_addr` file with it, which
//! only the daemon's own SIGTERM path removes. Every daemon here runs on a
//! fresh temp data dir with a fake `HOME` and `--no-auto-discover`, so none
//! touches the real daemon, its allowlist, or the resident-index cap.
//! Test: `cargo test -p trusty-search --test test_daemon_exit_8900`.

#[path = "support/test_daemon.rs"]
mod test_daemon;

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use test_daemon::{pid_alive, DaemonGuard};

/// Puts this binary into spawner mode and names the data dir to use.
const ENV_DATA_DIR: &str = "TRUSTY_8900_DATA_DIR";

/// Which spawn shape the helper uses: `guard` or `cli`.
const ENV_SHAPE: &str = "TRUSTY_8900_SHAPE";

/// How long a daemon gets to boot far enough to write its `http_addr`.
const BOOT_TIMEOUT: Duration = Duration::from_secs(90);

/// How long a daemon gets to notice its spawner died and exit.
///
/// Why: the watchdog polls every 250 ms and grants a 5 s graceful window, so
/// the real bound is a few seconds. 30 s is slack for a loaded box; the
/// pre-fix daemon never exits at all.
const REAP_TIMEOUT: Duration = Duration::from_secs(30);

const POLL: Duration = Duration::from_millis(100);

/// SIGKILL `pid`. Used only to clean up on a failure path.
fn hard_kill(pid: u32) {
    // SAFETY: `kill` accepts any pid; a stale one returns ESRCH.
    unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
}

/// A loopback port nothing is bound to right now.
fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind an ephemeral port");
    listener.local_addr().expect("local addr").port()
}

/// Poll until `ready` holds or `timeout` passes.
fn wait_until(timeout: Duration, mut ready: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    while !ready() {
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(POLL);
    }
    true
}

/// The address the daemon wrote to `<data_dir>/http_addr` once it bound.
fn bound_addr(data_dir: &Path) -> Option<String> {
    let addr = std::fs::read_to_string(data_dir.join("http_addr")).ok()?;
    let addr = addr.trim();
    (!addr.is_empty()).then(|| addr.to_string())
}

/// The pid the daemon recorded in `<data_dir>/daemon.lock`.
fn locked_pid(data_dir: &Path) -> Option<u32> {
    let raw = std::fs::read_to_string(data_dir.join("daemon.lock")).ok()?;
    raw.trim().parse().ok().filter(|pid| *pid > 1)
}

/// Re-entry helper: start a daemon the way the suites do, then block.
///
/// Why `#[ignore]`: it is not a test. The parent runs this binary filtered to
/// it with [`ENV_DATA_DIR`] set; without that it returns at once.
/// What: `guard` spawns through `DaemonGuard`, `cli` runs `trusty-search list`
/// against a data dir whose `daemon.port` names a port nothing listens on, so
/// the CLI's health probe fails and it auto-starts a detached daemon. Without
/// that file, discovery falls back to the default port: the real daemon. Both use the stamped `test_daemon::command`.
/// Test: driven by the two `*_is_killed` tests.
#[test]
#[ignore = "re-entry helper for the #8900 tests; does nothing when run directly"]
fn spawner_child_mode() {
    let Some(dir) = std::env::var_os(ENV_DATA_DIR) else {
        return;
    };
    let data_dir = Path::new(&dir);
    let _guard = match std::env::var(ENV_SHAPE).as_deref() {
        Ok("guard") => Some(DaemonGuard::spawn(data_dir, free_port())),
        Ok("cli") => {
            let home = data_dir.join("home");
            std::fs::create_dir_all(&home).expect("create fake HOME");
            std::fs::write(data_dir.join("daemon.port"), free_port().to_string())
                .expect("seed daemon.port");
            test_daemon::command()
                .arg("list")
                .current_dir(data_dir)
                .env("TRUSTY_DATA_DIR", data_dir)
                .env("TRUSTY_DATA_DIR_OVERRIDE", data_dir)
                .env("HOME", &home)
                .env("XDG_CONFIG_HOME", &home)
                .env("TRUSTY_NO_AUTO_DISCOVER", "1")
                .env("TRUSTY_EMBEDDER", "stdio")
                .env("TRUSTY_SKIP_RAM_CHECK", "1")
                .env_remove("TRUSTY_INDEX")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .expect("run trusty-search list");
            // The CLI gives up waiting for readiness and exits; the daemon it
            // started stays up, detached, which is the shape under test.
            None
        }
        other => panic!("unknown {ENV_SHAPE}: {other:?}"),
    };
    loop {
        std::thread::sleep(Duration::from_secs(60));
    }
}

/// Start the helper in `shape`, wait for its daemon, SIGKILL the helper, and
/// require the daemon to exit gracefully.
///
/// Why the kill differs by shape: the `guard` daemon shares the helper's
/// process group, so a group kill would SIGKILL it directly and prove nothing.
/// The `cli` daemon leads its own session (#8783), so the group kill ends the
/// helper and its CLI and leaves the daemon to the watchdog.
fn assert_daemon_dies_with_its_spawner(shape: &str) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let data_dir = tmp.path();
    let mut helper = Command::new(std::env::current_exe().expect("this test binary"));
    helper
        .args([
            "--exact",
            "spawner_child_mode",
            "--ignored",
            "--test-threads",
            "1",
        ])
        .env(ENV_DATA_DIR, data_dir)
        .env(ENV_SHAPE, shape)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    std::os::unix::process::CommandExt::process_group(&mut helper, 0);
    let mut helper = helper.spawn().expect("spawn the re-entry helper");
    let group = helper.id();
    let kill_helper = |helper: &mut std::process::Child| {
        // SAFETY: `group` is the pgid `process_group(0)` gave our own child.
        unsafe { libc::killpg(group as libc::pid_t, libc::SIGKILL) };
        let _ = helper.wait();
    };

    let booted = wait_until(BOOT_TIMEOUT, || {
        bound_addr(data_dir).is_some() && locked_pid(data_dir).is_some()
    });
    let Some(daemon_pid) = locked_pid(data_dir) else {
        kill_helper(&mut helper);
        panic!("the {shape} helper's daemon never wrote daemon.lock");
    };
    if !booted {
        kill_helper(&mut helper);
        hard_kill(daemon_pid);
        panic!("the {shape} helper's daemon {daemon_pid} never wrote http_addr");
    }

    // The point: no `Drop`, no unwind, no teardown runs in the helper.
    if shape == "cli" {
        kill_helper(&mut helper);
    } else {
        helper.kill().expect("SIGKILL the helper");
        helper.wait().expect("reap the helper");
    }

    if !wait_until(REAP_TIMEOUT, || !pid_alive(daemon_pid)) {
        hard_kill(daemon_pid);
        panic!(
            "#8900: {shape} daemon {daemon_pid} was still running {REAP_TIMEOUT:?} after its \
             test process was SIGKILLed; killed it here so this failure leaks nothing"
        );
    }
    assert!(
        !data_dir.join("http_addr").exists(),
        "daemon {daemon_pid} exited without removing http_addr: the watchdog must route \
         through the daemon's SIGTERM shutdown, not exit the process outright"
    );
}

/// Why: a `DaemonGuard` daemon's `Drop` never runs when the test binary is
/// SIGKILLed; the parent-death stamp must end it (#8900).
/// Test: itself.
#[test]
fn guard_daemon_exits_when_its_test_binary_is_killed() {
    assert_daemon_dies_with_its_spawner("guard");
}

/// Why: the leaked daemons of #8900 were started by a CLI's auto-spawn, a
/// detached grandchild no test holds a handle to. The CLI must forward the
/// stamp and the daemon must honour it.
/// Test: itself.
#[test]
fn cli_auto_started_daemon_exits_when_its_test_binary_is_killed() {
    assert_daemon_dies_with_its_spawner("cli");
}

/// Why: the ordinary path, a test that returns or panics, must end its daemon
/// at once rather than when the test binary exits.
/// Test: itself.
#[test]
fn guard_drop_ends_its_daemon() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let guard = DaemonGuard::spawn(tmp.path(), free_port());
    let pid = guard.pid();
    assert!(
        wait_until(BOOT_TIMEOUT, || bound_addr(tmp.path()).is_some()),
        "daemon {pid} never wrote http_addr"
    );
    drop(guard);
    assert!(
        wait_until(REAP_TIMEOUT, || !pid_alive(pid)),
        "daemon {pid} outlived its dropped DaemonGuard"
    );
}

/// Why (#8900, #8748): a piped run must not wait on a descendant holding the
/// captured pipes.
/// What: runs `sh`, which backgrounds a `sleep` that inherits both pipes and
/// then `exec`s another. `run_bounded` must return inside its deadline plus
/// [`test_daemon::DRAIN_GRACE`] and leave the backgrounded `sleep` dead.
/// Test: itself.
#[test]
fn run_bounded_returns_while_a_grandchild_holds_the_pipes() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let pidfile = tmp.path().join("grandchild.pid");
    let mut cmd = Command::new("/bin/sh");
    cmd.args(["-c", "sleep 300 & echo $! > \"$1\"; exec sleep 300", "sh"])
        .arg(&pidfile);
    let deadline = Duration::from_secs(2);
    let started = Instant::now();
    let out = test_daemon::run_bounded(&mut cmd, deadline);
    let took = started.elapsed();
    let grandchild: Option<u32> = std::fs::read_to_string(&pidfile)
        .ok()
        .and_then(|s| s.trim().parse().ok());
    let survived = grandchild.filter(|pid| pid_alive(*pid));
    if let Some(pid) = survived {
        hard_kill(pid);
    }
    assert!(
        took < deadline + test_daemon::DRAIN_GRACE + Duration::from_secs(10),
        "run_bounded took {took:?}"
    );
    assert_eq!(out.code, None, "the deadline kill must end the child");
    assert_eq!(
        survived, None,
        "the backgrounded grandchild outlived run_bounded"
    );
}

/// Why: the stamp is only as good as its coverage. A test that builds its own
/// `Command::new` on Cargo's binary path spawns an unstamped binary,
/// which is how the #7085 linkage drifted in trusty-memory.
/// What: scans every `tests/*.rs` and fails on any file other than the support
/// module that names the binary directly.
/// Test: itself.
#[test]
fn every_test_starts_the_binary_through_the_stamped_command() {
    let tests_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    let needle = concat!("CARGO_BIN_EXE_", "trusty-search");
    let mut offenders = Vec::new();
    for entry in std::fs::read_dir(&tests_dir).expect("read tests/") {
        let path = entry.expect("dir entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        let text = std::fs::read_to_string(&path).expect("read test file");
        if text.contains(needle) {
            offenders.push(path.display().to_string());
        }
    }
    assert!(
        offenders.is_empty(),
        "#8900: start trusty-search through tests/support/test_daemon.rs::command, \
         not directly: {offenders:?}"
    );
}
