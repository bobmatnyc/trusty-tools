//! #9034 regression tests for the autostart plan.
//!
//! Every test drives a fake `launchctl`, a fake process table, and a temp-dir
//! lock. None calls the real `launchctl`, port 7880, or `~/.trusty-mpm`.

use super::*;
use std::cell::RefCell;
use std::collections::VecDeque;

/// A pid no process can hold (above the default `pid_max`).
const DEAD_PID: u32 = 4_194_303;

/// A `launchctl print` body for a loaded job in `state`.
fn print_body(state: &str) -> String {
    format!(
        "gui/501/com.trusty.mpm = {{\n\tactive count = 0\n\tpath = /x.plist\n\
         \tstate = {state}\n\n\tendpoints = {{\n\t\tstate = active\n\t}}\n}}\n"
    )
}

/// Scripted `launchctl`: each `print` pops the next answer (`None` = not
/// loaded); `kickstart` and `bootstrap` answer with fixed results.
struct FakeLaunchctl {
    prints: RefCell<VecDeque<Option<String>>>,
    kickstart_ok: bool,
    calls: RefCell<Vec<String>>,
}

impl FakeLaunchctl {
    fn new(prints: Vec<Option<&str>>, kickstart_ok: bool) -> Self {
        Self {
            prints: RefCell::new(prints.into_iter().map(|p| p.map(print_body)).collect()),
            kickstart_ok,
            calls: RefCell::new(Vec::new()),
        }
    }

    fn run(&self, args: &[String]) -> Option<LaunchctlReply> {
        self.calls.borrow_mut().push(args.join(" "));
        let reply = |success: bool, stdout: String| Some(LaunchctlReply { success, stdout });
        match args.first().map(String::as_str) {
            Some("print") => match self.prints.borrow_mut().pop_front().flatten() {
                Some(body) => reply(true, body),
                None => reply(false, String::new()),
            },
            Some("kickstart") => reply(self.kickstart_ok, String::new()),
            _ => reply(true, String::new()),
        }
    }

    fn calls(&self) -> Vec<String> {
        self.calls.borrow().clone()
    }
}

fn target() -> LaunchdTarget {
    LaunchdTarget {
        domain: "gui/501".to_string(),
        label: "com.trusty.mpm".to_string(),
        plist: std::path::PathBuf::from("/fake/com.trusty.mpm.plist"),
    }
}

fn write_lock(path: &std::path::Path, pid: u32) {
    std::fs::write(
        path,
        format!(
            "product = \"trusty-mpm\"\npid = {pid}\naddr = \"http://127.0.0.1:47011\"\n\
             started_at = \"\"\n"
        ),
    )
    .expect("write lock");
}

/// Run [`prepare_autostart`] against `fake`, with `daemon_pids` as the
/// process table's daemon processes.
fn prepare(
    fake: &FakeLaunchctl,
    launchd: bool,
    lock: &std::path::Path,
    daemon_pids: &[u32],
) -> AutostartPlan {
    let run = |args: &[String]| fake.run(args);
    let is_daemon_pid = |pid: u32| daemon_pids.contains(&pid);
    let t = target();
    prepare_autostart(&run, launchd.then_some(&t), lock, &is_daemon_pid)
}

#[test]
fn parse_state_running() {
    assert_eq!(
        parse_launchd_state(&print_body("running")),
        Some(LaunchdJob::Running)
    );
}

#[test]
fn parse_state_not_running() {
    // #9034: exit 0 with `state = not running` is the #4230 state.
    assert_eq!(
        parse_launchd_state(&print_body("not running")),
        Some(LaunchdJob::Stopped)
    );
    assert_eq!(
        parse_launchd_state(&print_body("spawn scheduled")),
        Some(LaunchdJob::Stopped)
    );
}

#[test]
fn parse_state_absent_is_none() {
    assert_eq!(parse_launchd_state("gui/501/x = {\n\tpid = 1\n}\n"), None);
}

#[test]
fn prepare_awaits_a_running_launchd_job() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let fake = FakeLaunchctl::new(vec![Some("running")], true);
    let plan = prepare(&fake, true, &tmp.path().join("daemon.lock"), &[]);
    assert!(matches!(plan, AutostartPlan::AwaitExisting(ref e) if e.contains("running")));
    assert_eq!(fake.calls(), vec!["print gui/501/com.trusty.mpm"]);
}

#[test]
fn prepare_kickstarts_a_loaded_but_stopped_job() {
    // #9034 HIGH: loaded with `state = not running` and no lock (the #4230
    // state) is down and startable, never awaited.
    let tmp = tempfile::tempdir().expect("tempdir");
    let fake = FakeLaunchctl::new(vec![Some("not running")], true);
    let plan = prepare(&fake, true, &tmp.path().join("daemon.lock"), &[]);
    assert_eq!(plan, AutostartPlan::AwaitLaunchd);
    assert_eq!(
        fake.calls(),
        vec![
            "print gui/501/com.trusty.mpm",
            "kickstart gui/501/com.trusty.mpm"
        ]
    );
}

#[test]
fn prepare_bootstraps_then_kickstarts_an_unloaded_job() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let fake = FakeLaunchctl::new(vec![None, Some("not running")], true);
    let plan = prepare(&fake, true, &tmp.path().join("daemon.lock"), &[]);
    assert_eq!(plan, AutostartPlan::AwaitLaunchd);
    assert_eq!(
        fake.calls(),
        vec![
            "print gui/501/com.trusty.mpm",
            "bootstrap gui/501 /fake/com.trusty.mpm.plist",
            "print gui/501/com.trusty.mpm",
            "kickstart gui/501/com.trusty.mpm"
        ]
    );
}

#[test]
fn prepare_spawns_when_launchd_cannot_start_the_job() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let fake = FakeLaunchctl::new(vec![Some("not running")], false);
    assert_eq!(
        prepare(&fake, true, &tmp.path().join("daemon.lock"), &[]),
        AutostartPlan::Spawn
    );
}

#[test]
fn prepare_awaits_a_live_daemon_lock_pid() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let lock = tmp.path().join("daemon.lock");
    let me = std::process::id();
    write_lock(&lock, me);
    let fake = FakeLaunchctl::new(vec![], true);
    let plan = prepare(&fake, false, &lock, &[me]);
    assert!(
        matches!(plan, AutostartPlan::AwaitExisting(ref e) if e.contains("tm start")),
        "a live daemon lock pid is awaited and names its recovery: {plan:?}"
    );
    assert!(lock.exists(), "a live daemon's lock must never be deleted");
}

#[test]
fn prepare_removes_a_reused_pid_lock_and_spawns() {
    // #9034 MEDIUM: a live pid that is not a daemon process is a reused pid.
    let tmp = tempfile::tempdir().expect("tempdir");
    let lock = tmp.path().join("daemon.lock");
    write_lock(&lock, std::process::id());
    let fake = FakeLaunchctl::new(vec![], true);
    assert_eq!(prepare(&fake, false, &lock, &[]), AutostartPlan::Spawn);
    assert!(!lock.exists(), "a reused-pid lock is stale and is removed");
}

#[test]
fn prepare_clears_dead_pid_lock_and_spawns() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let lock = tmp.path().join("daemon.lock");
    write_lock(&lock, DEAD_PID);
    let fake = FakeLaunchctl::new(vec![], true);
    assert_eq!(prepare(&fake, false, &lock, &[]), AutostartPlan::Spawn);
    assert!(!lock.exists(), "a dead-pid lock is stale and is removed");
}

#[test]
fn timeout_evidence_relaunched_job_still_running_is_slow() {
    let t = target();
    let running = FakeLaunchctl::new(vec![Some("running")], true);
    let run = |args: &[String]| running.run(args);
    let evidence = timeout_evidence(&AutostartPlan::AwaitLaunchd, &run, Some(&t), None);
    assert!(evidence.is_some_and(|e| e.contains("running")));
    let stopped = FakeLaunchctl::new(vec![Some("not running")], true);
    let run = |args: &[String]| stopped.run(args);
    assert_eq!(
        timeout_evidence(&AutostartPlan::AwaitLaunchd, &run, Some(&t), None),
        None,
        "a job that stopped again is down"
    );
}

#[test]
fn timeout_evidence_spawn_depends_on_the_child() {
    let fake = FakeLaunchctl::new(vec![], true);
    let run = |args: &[String]| fake.run(args);
    assert_eq!(
        timeout_evidence(&AutostartPlan::Spawn, &run, None, Some(42)),
        Some("spawned pid 42 still starting".to_string())
    );
    assert_eq!(
        timeout_evidence(&AutostartPlan::Spawn, &run, None, None),
        None
    );
    assert!(fake.calls().is_empty(), "a spawn never asks launchd");
}

#[test]
fn spawned_child_still_running_reports_its_pid() {
    let mut child = std::process::Command::new("sleep")
        .arg("5")
        .spawn()
        .expect("spawn sleep");
    let pid = child.id();
    assert_eq!(spawned_still_running(&mut child), Some(pid));
    let _ = child.kill();
    let _ = child.wait();
}

#[test]
fn spawned_child_that_exited_reports_none() {
    let mut child = std::process::Command::new("true")
        .spawn()
        .expect("spawn true");
    let _ = child.wait();
    assert_eq!(spawned_still_running(&mut child), None);
}

#[test]
fn timeout_error_with_evidence_is_alive_unresponsive() {
    let err = autostart_timeout_error(Some("spawned pid 42 still starting".to_string()));
    let alive = err
        .downcast_ref::<DaemonAliveUnresponsive>()
        .expect("evidence must yield DaemonAliveUnresponsive");
    assert_eq!(alive.evidence, "spawned pid 42 still starting");
}

#[test]
fn timeout_error_without_evidence_is_plain() {
    let err = autostart_timeout_error(None);
    assert!(err.downcast_ref::<DaemonAliveUnresponsive>().is_none());
    assert!(err.to_string().contains("did not become healthy"));
}
