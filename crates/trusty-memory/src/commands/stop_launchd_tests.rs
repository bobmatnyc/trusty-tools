//! Tests for the launchd half of `stop` and `service start` (#8750).
//!
//! macOS-only, like the `launchd` module: fakes stand in for the unit, so the
//! real user domain is never touched.

use super::*;

/// A launchd unit stand-in (#8750): reports its state and answers `terminate`
/// by `outcome`. Never touches the real user domain.
struct FakeUnit {
    state: std::result::Result<launchd::UnitState, &'static str>,
    pid: u32,
    outcome: Terminate,
}

#[derive(Clone, Copy)]
enum Terminate {
    /// SIGTERM the pid, as `launchctl kill SIGTERM` does.
    Signal,
    /// Report success but deliver nothing: a daemon that ignores the stop.
    Ignore,
    /// `launchctl kill` itself fails.
    Fail,
}

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

    fn kickstart(&self) -> Result<()> {
        anyhow::bail!("stop never kickstarts")
    }
}

/// A launchd-owned daemon stand-in: its pid, a kill-on-drop guard, its stdin
/// (held so bash blocks), and a reaper so `kill -0` stops seeing it on exit.
type LaunchdStandIn = (
    u32,
    KillPidOnDrop,
    std::process::ChildStdin,
    std::thread::JoinHandle<std::io::Result<std::process::ExitStatus>>,
);

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

/// A loaded unit for `service start` (#8750): `before` until a kickstart,
/// then `after`. Counts kickstarts; never touches the real user domain.
struct StartFake {
    before: launchd::UnitState,
    after: launchd::UnitState,
    kickstarts: std::cell::Cell<u32>,
}

impl StartFake {
    fn new(before: launchd::UnitState, after: launchd::UnitState) -> Self {
        Self {
            before,
            after,
            kickstarts: std::cell::Cell::new(0),
        }
    }
}

impl launchd::LaunchdUnit for StartFake {
    fn label(&self) -> &str {
        "com.trusty.memory.test"
    }

    fn state(&self) -> Result<launchd::UnitState> {
        Ok(if self.kickstarts.get() == 0 {
            self.before
        } else {
            self.after
        })
    }

    fn terminate(&self) -> Result<()> {
        anyhow::bail!("service start never terminates")
    }

    fn kickstart(&self) -> Result<()> {
        self.kickstarts.set(self.kickstarts.get() + 1);
        Ok(())
    }
}

/// Why (#8750 review HIGH-2): after `stop` the unit is loaded with no pid.
/// The install half of `service start` finds it current and does nothing, so
/// only a kickstart brings the daemon back.
#[test]
fn service_start_kickstarts_a_loaded_unit_with_no_pid() {
    let unit = StartFake::new(
        launchd::UnitState::Loaded { pid: None },
        launchd::UnitState::Loaded { pid: Some(4242) },
    );
    let outcome =
        launchd::start_loaded_unit(&unit, Duration::from_secs(1)).expect("kickstart starts it");
    assert_eq!(outcome, launchd::StartOutcome::Kickstarted(4242));
    assert_eq!(unit.kickstarts.get(), 1);
}

/// A running unit is left alone: a kickstart there is not asked for.
#[test]
fn service_start_leaves_a_running_unit_alone() {
    let running = launchd::UnitState::Loaded { pid: Some(7) };
    let unit = StartFake::new(running, running);
    let outcome = launchd::start_loaded_unit(&unit, Duration::from_secs(1)).expect("running");
    assert_eq!(outcome, launchd::StartOutcome::AlreadyRunning(7));
    assert_eq!(unit.kickstarts.get(), 0);
}

/// Fail-Open Check (#8750): a kickstart that leaves no process is a failed
/// start, never a quiet success; a unit that is not loaded is an error too.
#[test]
fn service_start_fails_when_the_kickstarted_unit_never_runs() {
    let idle = launchd::UnitState::Loaded { pid: None };
    let unit = StartFake::new(idle, idle);
    let err = launchd::start_loaded_unit(&unit, Duration::from_millis(200))
        .expect_err("no pid after the kickstart");
    assert!(err.to_string().contains("no process"), "{err}");
    assert_eq!(unit.kickstarts.get(), 1);

    let absent = launchd::UnitState::NotLoaded;
    let err = launchd::start_loaded_unit(&StartFake::new(absent, absent), Duration::ZERO)
        .expect_err("not loaded");
    assert!(err.to_string().contains("not loaded"), "{err}");
}

/// Why (#8750 review HIGH-2): the stop message promised a restart from
/// `service start`, which did nothing for an idle unit. It must name that
/// command, which now kickstarts, and not claim the daemon is running.
#[test]
fn stop_message_names_the_command_that_restarts_the_daemon() {
    let msg = launchd::stopped_message("com.trusty.memory");
    assert!(msg.contains("`trusty-memory service start`"), "{msg}");
    assert!(msg.contains("kickstart"), "{msg}");
}

/// Why (#8750 review MEDIUM-1): every non-zero `launchctl print` exit read as
/// "not loaded", so an unanswerable launchctl let `stop` report no daemon.
/// Only launchd's not-found answer is "not loaded".
#[test]
fn launchctl_print_exits_classify_into_loaded_absent_or_error() {
    use launchd::{classify_print, PrintVerdict};
    let not_found = "Bad request.\nCould not find service \"com.trusty.memory\" in domain";
    for (code, stderr, want) in [
        (Some(0), "", PrintVerdict::Loaded),
        (Some(113), "", PrintVerdict::NotLoaded),
        (Some(3), not_found, PrintVerdict::NotLoaded),
    ] {
        assert_eq!(classify_print(code, stderr).expect("classified"), want);
    }
    for (code, stderr) in [(Some(5), "Input/output error"), (Some(1), ""), (None, "")] {
        let err = classify_print(code, stderr).expect_err("not a not-found answer");
        assert!(err.to_string().contains("launchctl print"), "{err}");
    }
}
