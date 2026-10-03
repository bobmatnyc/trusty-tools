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

    stop_daemons_in(&rows, std::process::id(), Duration::from_secs(5)).expect("the daemon stops");
    let status = reaper.join().expect("reaper").expect("wait");
    daemon_guard.0 = None;
    assert!(!status.success(), "daemon ended by a signal: {status:?}");
    assert!(
        bridge.0.try_wait().expect("try_wait").is_none(),
        "the stdio bridge must survive stop"
    );
}

/// Why (#277 MEDIUM-4): `stop` exited 0 when a daemon outlived SIGKILL, so a
/// script (the import runbook's "stop, then import") went on against a live
/// daemon. The stand-in is never reaped until the test ends, so after the
/// signals it lingers as a zombie that `kill -0` still sees.
#[cfg(unix)]
#[test]
fn stop_fails_when_a_daemon_is_still_alive_after_sigkill() {
    let dir = tempfile::tempdir().expect("tempdir");
    let daemon = KillOnDrop(spawn_stand_in(dir.path(), &["serve", "--foreground"]));
    let pid = daemon.0.id();
    let rows = live_rows_for(&[pid]);
    assert_eq!(rows.len(), 1, "stand-in visible with argv: {rows:?}");

    let err = stop_daemons_in(&rows, std::process::id(), Duration::from_millis(200))
        .expect_err("a daemon that survives SIGKILL is a failed stop");
    assert!(err.to_string().contains("still running"), "{err}");
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

/// A launchd unit stand-in (#8750): reports its state and answers `terminate`
/// by `outcome`. Never touches the real user domain.
#[cfg(unix)]
struct FakeUnit {
    state: std::result::Result<launchd::UnitState, &'static str>,
    pid: u32,
    outcome: Terminate,
}

#[cfg(unix)]
#[derive(Clone, Copy)]
enum Terminate {
    /// SIGTERM the pid, as `launchctl kill SIGTERM` does.
    Signal,
    /// Report success but deliver nothing: a daemon that ignores the stop.
    Ignore,
    /// `launchctl kill` itself fails.
    Fail,
}

#[cfg(unix)]
impl launchd::LaunchdUnit for FakeUnit {
    fn label(&self) -> &str {
        "com.trusty.memory.test"
    }

    fn state(&self) -> Result<launchd::UnitState> {
        self.state.map_err(|e| anyhow::anyhow!(e))
    }

    fn terminate(&self) -> Result<()> {
        match self.outcome {
            Terminate::Signal => Ok(send_signal(self.pid, "TERM")?),
            Terminate::Ignore => Ok(()),
            Terminate::Fail => anyhow::bail!("launchctl kill exited 3"),
        }
    }
}

/// A launchd-owned daemon stand-in: its pid, a kill-on-drop guard, its stdin
/// (held so bash blocks), and a reaper so `kill -0` stops seeing it on exit.
#[cfg(unix)]
type LaunchdStandIn = (
    u32,
    KillPidOnDrop,
    std::process::ChildStdin,
    std::thread::JoinHandle<std::io::Result<std::process::ExitStatus>>,
);

#[cfg(unix)]
fn launchd_stand_in(dir: &std::path::Path) -> LaunchdStandIn {
    let mut child = spawn_stand_in(dir, &["serve", "--foreground"]);
    let pid = child.id();
    let stdin = child.stdin.take().expect("piped stdin");
    let reaper = std::thread::spawn(move || child.wait());
    (pid, KillPidOnDrop(Some(pid)), stdin, reaper)
}

/// Why (#8750): `stop` reported "No daemon running" while launchd ran one the
/// process-table scan did not classify. The table handed in is empty — the
/// scan missed it — and the unit's label is what finds and stops it.
#[cfg(unix)]
#[test]
fn stop_terminates_a_launchd_daemon_the_process_table_misses() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (pid, mut guard, _stdin, reaper) = launchd_stand_in(dir.path());
    let unit = FakeUnit {
        state: Ok(launchd::UnitState::Loaded { pid: Some(pid) }),
        pid,
        outcome: Terminate::Signal,
    };

    launchd::stop_with_unit(
        &[],
        std::process::id(),
        Duration::from_millis(10),
        &unit,
        Duration::from_secs(5),
    )
    .expect("the launchd daemon is found by its label and stopped");
    let status = reaper.join().expect("reaper").expect("wait");
    guard.0 = None;
    assert!(!status.success(), "ended by SIGTERM: {status:?}");
}

/// Fail-Open Check (#8750): a `launchctl kill` that fails is a failed stop that
/// names the label — never "No daemon running", never a success.
#[cfg(unix)]
#[test]
fn stop_propagates_a_failed_launchd_kill() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (pid, _guard, _stdin, _reaper) = launchd_stand_in(dir.path());
    let unit = FakeUnit {
        state: Ok(launchd::UnitState::Loaded { pid: Some(pid) }),
        pid,
        outcome: Terminate::Fail,
    };

    let err = launchd::stop_with_unit(
        &[],
        std::process::id(),
        Duration::from_millis(10),
        &unit,
        Duration::from_secs(1),
    )
    .expect_err("a failed launchctl kill fails the stop");
    let msg = format!("{err:#}");
    assert!(msg.contains("com.trusty.memory.test"), "{msg}");
    assert!(msg.contains("launchctl kill exited 3"), "{msg}");
    assert!(pid_alive(pid), "nothing else signalled the daemon");
}

/// Fail-Open Check (#8750): a launchd daemon still alive after the grace is a
/// failed stop. It is not SIGKILLed: launchd would respawn a signal death.
#[cfg(unix)]
#[test]
fn stop_fails_when_the_launchd_daemon_outlives_the_grace() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (pid, _guard, _stdin, _reaper) = launchd_stand_in(dir.path());
    let unit = FakeUnit {
        state: Ok(launchd::UnitState::Loaded { pid: Some(pid) }),
        pid,
        outcome: Terminate::Ignore,
    };

    let err = launchd::stop_with_unit(
        &[],
        std::process::id(),
        Duration::from_millis(10),
        &unit,
        Duration::from_millis(300),
    )
    .expect_err("a daemon that outlives the grace is a failed stop");
    assert!(err.to_string().contains("still running"), "{err}");
    assert!(pid_alive(pid), "no SIGKILL was sent");
}

/// Fail-Open Check (#8750): launchd that cannot be asked is not "nothing is
/// running". With an empty table the launchd error is the result; a loaded
/// unit between spawns, with an empty table, is still "No daemon running".
#[cfg(unix)]
#[test]
fn stop_does_not_report_no_daemon_when_launchd_cannot_be_queried() {
    let unreadable = FakeUnit {
        state: Err("launchctl print: permission denied"),
        pid: 0,
        outcome: Terminate::Fail,
    };
    let err = launchd::stop_with_unit(
        &[],
        std::process::id(),
        Duration::from_millis(10),
        &unreadable,
        Duration::from_millis(10),
    )
    .expect_err("an unreadable unit with an empty table is not a success");
    let msg = format!("{err:#}");
    assert_ne!(err.to_string(), "No daemon running", "{msg}");
    assert!(msg.contains("permission denied"), "{msg}");

    let idle = FakeUnit {
        state: Ok(launchd::UnitState::Loaded { pid: None }),
        pid: 0,
        outcome: Terminate::Fail,
    };
    let err = launchd::stop_with_unit(
        &[],
        std::process::id(),
        Duration::from_millis(10),
        &idle,
        Duration::from_millis(10),
    )
    .expect_err("an idle unit and an empty table hold nothing to stop");
    assert_eq!(err.to_string(), "No daemon running");
}
