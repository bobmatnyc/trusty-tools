//! Tests for the `trusty-memory serve` process scan (#277).
//!
//! The pure tests drive [`super::daemon_pids_in`] through a hand-built process
//! list; the real-process test spawns a stand-in whose executable name and
//! argv match a daemon and asserts the live scan finds it.

use super::*;

/// A process-table row for the injected lister.
fn proc_row(pid: u32, name: &str, argv: &[&str]) -> ProcInfo {
    ProcInfo {
        pid,
        name: name.to_string(),
        argv: argv.iter().map(|a| a.to_string()).collect(),
    }
}

fn argv(a: &[&str]) -> Vec<String> {
    a.iter().map(|s| s.to_string()).collect()
}

#[test]
fn classify_argv_separates_the_daemon_from_stdio_bridges() {
    let daemon = [
        &["trusty-memory", "serve", "--foreground"][..],
        &["/Users/x/.cargo/bin/trusty-memory", "serve", "--http"],
        &["trusty-memory", "serve", "--http=127.0.0.1:7070"],
        &["trusty-memory", "-v", "serve", "--http", "127.0.0.1:7070"],
        &["trusty-memory", "serve", "--palace", "p", "--foreground"],
    ];
    for a in daemon {
        assert_eq!(classify_argv(&argv(a)), ProcessRole::Daemon, "{a:?}");
    }
    let bridge = [
        &["trusty-memory", "serve", "--stdio"][..],
        &["trusty-memory", "serve"],
        &["trusty-memory", "serve", "--palace", "p"],
    ];
    for a in bridge {
        assert_eq!(classify_argv(&argv(a)), ProcessRole::StdioBridge, "{a:?}");
    }
    let other = [
        &["trusty-memory", "stop"][..],
        &["trusty-memory", "import", "kuzu", "serve"],
        &["trusty-memory"],
    ];
    for a in other {
        assert_eq!(classify_argv(&argv(a)), ProcessRole::Other, "{a:?}");
    }
}

#[test]
fn daemon_pids_in_returns_only_daemon_mode_serve_processes() {
    let me = 99;
    let procs = [
        proc_row(
            10,
            "trusty-memory",
            &["trusty-memory", "serve", "--foreground"],
        ),
        proc_row(11, "trusty-memory", &["trusty-memory", "serve", "--stdio"]),
        proc_row(12, "trusty-memory", &["trusty-memory", "serve"]),
        proc_row(13, "trusty-memory", &["trusty-memory", "status"]),
        // `cargo run -p trusty-memory -- serve --foreground`: not our binary.
        proc_row(
            14,
            "cargo",
            &[
                "cargo",
                "run",
                "-p",
                "trusty-memory",
                "--",
                "serve",
                "--foreground",
            ],
        ),
        proc_row(me, "trusty-memory", &["trusty-memory", "serve", "--http"]),
        proc_row(15, "trusty-memory", &["trusty-memory", "serve", "--http"]),
    ];
    assert_eq!(daemon_pids_in(&procs, me), vec![10, 15]);
}

#[test]
fn daemon_pids_in_is_empty_when_only_bridges_run() {
    let procs = [
        proc_row(1, "trusty-memory", &["trusty-memory", "serve", "--stdio"]),
        proc_row(2, "trusty-memory", &["trusty-memory", "serve"]),
    ];
    assert!(daemon_pids_in(&procs, 0).is_empty());
}

/// Kills the stand-in daemon on drop, so a failed assertion leaves nothing
/// running.
struct KillOnDrop(std::process::Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[cfg(unix)]
#[test]
fn find_daemon_pids_finds_a_live_serve_foreground_process() {
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    // A `trusty-memory` symlink to bash: the kernel records the symlink's
    // name as the executable, and `-s` makes bash block reading the piped
    // stdin, so the process lives with argv `trusty-memory -s serve
    // --foreground` until it is killed.
    let dir = tempfile::tempdir().expect("tempdir");
    let exe = dir.path().join("trusty-memory");
    std::os::unix::fs::symlink("/bin/bash", &exe).expect("symlink bash");
    let child = Command::new(&exe)
        .args(["-s", "serve", "--foreground"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn stand-in daemon");
    let child = KillOnDrop(child);
    let pid = child.0.id();

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut found = find_daemon_pids();
    while !found.contains(&pid) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
        found = find_daemon_pids();
    }
    assert!(
        found.contains(&pid),
        "scan missed the live stand-in daemon pid {pid}; found {found:?}"
    );
}
