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

/// SIGKILLs a stand-in by pid on drop, for a child whose `Child` handle was
/// moved into a reaper thread. `None` once reaped, so a reused pid is never
/// signalled.
struct KillPidOnDrop(Option<u32>);

impl Drop for KillPidOnDrop {
    fn drop(&mut self) {
        if let Some(pid) = self.0 {
            let _ = send_signal(pid, "KILL");
        }
    }
}

/// Spawn a stand-in `trusty-memory` process with argv `trusty-memory -s <args>`.
///
/// A `trusty-memory` symlink to bash: the kernel records the symlink's name as
/// the executable, and `-s` makes bash block reading the piped stdin, so the
/// process lives until it is signalled. `-s` is a flag, so `classify_argv`
/// reads the first of `args` as the subcommand.
#[cfg(unix)]
fn spawn_stand_in(dir: &std::path::Path, args: &[&str]) -> std::process::Child {
    use std::process::{Command, Stdio};
    let exe = dir.join("trusty-memory");
    if !exe.exists() {
        std::os::unix::fs::symlink("/bin/bash", &exe).expect("symlink bash");
    }
    Command::new(&exe)
        .arg("-s")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn stand-in")
}

/// The live process table, cut down to `pids`, once every one of them is
/// visible with its argv loaded (bounded at five seconds).
#[cfg(unix)]
fn live_rows_for(pids: &[u32]) -> Vec<ProcInfo> {
    use std::time::{Duration, Instant};
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let rows: Vec<ProcInfo> = list_processes()
            .into_iter()
            .filter(|p| pids.contains(&p.pid) && !p.argv.is_empty())
            .collect();
        if rows.len() == pids.len() || Instant::now() >= deadline {
            return rows;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn stop_reports_no_daemon_when_only_bridges_run() {
    let procs = [proc_row(
        1,
        "trusty-memory",
        &["trusty-memory", "serve", "--stdio"],
    )];
    let err = stop_daemons_in(&procs, 0, Duration::from_millis(10))
        .expect_err("a bridge alone is not a daemon to stop");
    assert_eq!(err.to_string(), "No daemon running");
}

/// `stop` over the real process table, scoped to two stand-ins so the host's
/// own daemon is never in reach: the daemon-mode one is terminated, the stdio
/// bridge keeps running.
#[cfg(unix)]
#[test]
fn stop_terminates_the_daemon_and_spares_a_stdio_bridge() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut daemon = spawn_stand_in(dir.path(), &["serve", "--foreground"]);
    let daemon_pid = daemon.id();
    let mut daemon_guard = KillPidOnDrop(Some(daemon_pid));
    let mut bridge = KillOnDrop(spawn_stand_in(dir.path(), &["serve", "--stdio"]));
    let bridge_pid = bridge.0.id();

    let rows = live_rows_for(&[daemon_pid, bridge_pid]);
    assert_eq!(rows.len(), 2, "both stand-ins visible with argv: {rows:?}");

    // Reap the daemon stand-in as soon as it dies, so `kill -0` stops seeing
    // a zombie; the real daemon is not our child and needs no reaper.
    // `wait` closes the child's stdin first, which would end bash on EOF
    // rather than on the signal, so the handle stays here.
    let _daemon_stdin = daemon.stdin.take();
    let reaper = std::thread::spawn(move || daemon.wait());

    let outcome = stop_daemons_in(&rows, std::process::id(), Duration::from_secs(5))
        .expect("a daemon is running");
    assert_eq!(outcome, StopOutcome::Stopped);
    let status = reaper.join().expect("reaper").expect("wait");
    daemon_guard.0 = None;
    assert!(!status.success(), "daemon ended by a signal: {status:?}");
    assert!(
        bridge.0.try_wait().expect("try_wait").is_none(),
        "the stdio bridge must survive stop"
    );
}

#[cfg(unix)]
#[test]
fn find_daemon_pids_finds_a_live_serve_foreground_process() {
    use std::time::{Duration, Instant};

    let dir = tempfile::tempdir().expect("tempdir");
    let child = KillOnDrop(spawn_stand_in(dir.path(), &["serve", "--foreground"]));
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
