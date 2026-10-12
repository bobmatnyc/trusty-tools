//! #9171 A3b-2: the run loop's lifecycle rules (plan AC3, AC4, the lock and
//! dry run). Every file lives in a tempdir; tmux is scripted.

mod cycle_support;

use std::fs;
use std::io;
use std::time::Duration;

use cycle_support::{BrokenHost, Env, QuietHost, TestResult, fleet, screen};
use trusty_architect::poller::{
    Phase, PollLock, PollSession, PollState, RunError, load_state, parse_interval,
};
use trusty_architect::{Clock, FakeClock, ScriptedTmux};

const MINUTE: Duration = Duration::from_secs(60);

fn interval_sleeps(clock: &FakeClock, interval: Duration) -> usize {
    clock.sleeps().iter().filter(|d| **d == interval).count()
}

/// #9171 AC3: a run stopped after the alert append and before the state
/// save re-emits the same alerts, by dedup key, on the next run, and the
/// state file always parses. Red against a run that commits the dedup state
/// before the append, or saves state before appending.
#[test]
fn kill_mid_cycle_reemits_identical_alert_keys() -> TestResult {
    let env = Env::new(&[])?;
    let tmux = fleet(&[("tm-a", "%2", "claude")], &screen(50));
    let clock = FakeClock::new(1_000.0);
    let runner = env.runner(&tmux, &clock, &QuietHost);

    let (mut session, _) = PollSession::load(&env.config, false, clock.now_unix());
    let before = session.state.clone();
    let killed = runner.run_once_with(&mut session, &mut |p| {
        if p == Phase::AlertsAppended {
            Err(io::Error::other("killed"))
        } else {
            Ok(())
        }
    });
    assert!(killed.is_err(), "the hook stops the run: {killed:?}");
    let first = env.keys()?;
    assert!(
        !first.is_empty(),
        "the alerts were appended before the stop"
    );
    assert_eq!(session.state, before, "nothing is committed after a stop");
    assert!(!env.state_path().exists(), "the state is saved last");

    // The same process tries again: the in-memory state was not advanced.
    runner.run_once(&mut session)?;
    let again = env.keys()?;
    assert_eq!(again[first.len()..], first[..], "same keys re-emitted");

    // A restarted poller reloads the saved state and emits nothing twice.
    let (mut restarted, log) = PollSession::load(&env.config, false, clock.now_unix());
    assert!(log.is_empty(), "{log:?}");
    runner.run_once(&mut restarted)?;
    assert_eq!(env.keys()?.len(), again.len(), "deduped after the save");
    let (state, err) = load_state(&env.state_path());
    assert!(err.is_none(), "{err:?}");
    assert!(!state.alerts.is_empty());
    let tmp = fs::read_dir(&env.config.inbox)?
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
        .count();
    assert_eq!(tmp, 0, "no temporary state file is left");
    Ok(())
}

/// No fail-open: an `alerts.jsonl` that cannot be written leaves the dedup
/// state, the events offset and the wake count where they were, and no state
/// is saved, so the alerts are emitted once the file can be written.
#[test]
fn append_failure_commits_nothing() -> TestResult {
    let env = Env::new(&[])?;
    fs::create_dir(env.config.alerts())?;
    let tmux = fleet(&[("tm-a", "%2", "claude")], &screen(50));
    let clock = FakeClock::new(1_000.0);
    let runner = env.runner(&tmux, &clock, &QuietHost);
    let mut session = PollSession::new(PollState::default());
    let r = runner.run_once(&mut session);
    assert!(matches!(r, Err(RunError::Io { .. })), "{r:?}");
    assert_eq!(session.state, PollState::default());
    assert!(!env.state_path().exists());
    assert!(env.log().contains("ERROR"), "{}", env.log());

    fs::remove_dir(env.config.alerts())?;
    runner.run_once(&mut session)?;
    assert!(env.written("tm-a")?.contains(&"ctx".to_owned()));
    Ok(())
}

/// No fail-open: a sensor that fails the cycle (the host check here, as the
/// prototype's `host_alerts` raises) commits nothing and writes no alert.
#[test]
fn failed_cycle_commits_nothing() -> TestResult {
    let env = Env::new(&[])?;
    let tmux = fleet(&[("tm-a", "%2", "claude")], &screen(50));
    let clock = FakeClock::new(1_000.0);
    let state = PollState {
        events_offset: Some(0),
        ..PollState::default()
    };
    let mut session = PollSession::new(state.clone());
    let r = env
        .runner(&tmux, &clock, &BrokenHost)
        .run_once(&mut session);
    assert!(r.is_err(), "{r:?}");
    assert_eq!(session.state, state);
    assert!(env.written("tm-a")?.is_empty());
    assert!(!env.state_path().exists());
    Ok(())
}

/// #9171 AC4: exactly one `sleep(interval)` per cycle, whatever the cycle
/// does. Rows: zero panes, `list-panes` failing, a cycle that errors. Every
/// row is checked before the assertion.
#[test]
fn loop_sleeps_once_per_cycle() -> TestResult {
    let mut failures = Vec::new();
    let rows: [(&str, ScriptedTmux, bool); 3] = [
        ("zero panes", fleet(&[], ""), false),
        (
            "list-panes fails",
            ScriptedTmux::new()
                .fail("list-panes", "no server running")
                .fallback("list-windows", ""),
            false,
        ),
        (
            "cycle errors",
            fleet(&[("tm-a", "%2", "claude")], &screen(50)),
            true,
        ),
    ];
    for (name, tmux, broken) in rows {
        let env = Env::new(&[])?;
        let clock = FakeClock::new(1_000.0);
        let probe: &dyn trusty_architect::poller::HostProbe =
            if broken { &BrokenHost } else { &QuietHost };
        let runner = env.runner(&tmux, &clock, probe);
        let started = std::time::Instant::now();
        let r = runner.run_loop("60", Some(3));
        if r.is_err() {
            failures.push(format!("{name}: loop returned {r:?}"));
        }
        let n = interval_sleeps(&clock, MINUTE);
        if n != 3 {
            failures.push(format!("{name}: {n} interval sleeps for 3 cycles"));
        }
        if started.elapsed() > Duration::from_secs(1) {
            failures.push(format!("{name}: took {:?}", started.elapsed()));
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
    Ok(())
}

/// A failing `list-panes` logs the prototype's line and skips the cycle.
#[test]
fn list_panes_failure_logs_and_skips_the_cycle() -> TestResult {
    let env = Env::new(&[])?;
    let tmux = ScriptedTmux::new().fail("list-panes", "no server running");
    let clock = FakeClock::new(1_000.0);
    let mut session = PollSession::new(PollState::default());
    let summary = env
        .runner(&tmux, &clock, &QuietHost)
        .run_once(&mut session)?;
    assert_eq!(summary.wake, None);
    assert!(
        env.log().contains("panes=? ERROR tmux list-panes"),
        "{}",
        env.log()
    );
    assert!(env.log().contains("; cycle skipped"));
    assert_eq!(tmux.calls().len(), 1, "nothing after list-panes");
    Ok(())
}

/// #9171 AC4: a zero, negative or non-numeric interval is a config error,
/// raised before any tmux call. Each row is checked before the assertion.
#[test]
fn interval_zero_is_a_config_error() -> TestResult {
    let mut failures = Vec::new();
    for bad in ["0", "-5", "abc", "", "1.5"] {
        if parse_interval(bad).is_ok() {
            failures.push(format!("parse_interval({bad:?}) accepted"));
        }
        let env = Env::new(&[])?;
        let tmux = fleet(&[], "");
        let clock = FakeClock::new(0.0);
        let r = env.runner(&tmux, &clock, &QuietHost).run_loop(bad, Some(1));
        if !matches!(r, Err(RunError::Config(_))) {
            failures.push(format!("run_loop({bad:?}) -> {r:?}"));
        }
        if !tmux.calls().is_empty() {
            failures.push(format!("{bad:?}: tmux called {:?}", tmux.calls()));
        }
    }
    assert_eq!(parse_interval(" 60 ").ok(), Some(MINUTE));
    assert!(failures.is_empty(), "{failures:#?}");
    Ok(())
}

/// One poller per inbox: a second lock on `poll.lock` is refused while the
/// first is held, and granted once it is dropped.
#[test]
fn lock_refuses_a_second_poller_until_released() -> TestResult {
    let env = Env::new(&[])?;
    let path = env.config.lock();
    let first = PollLock::acquire(&path)?;
    let second = PollLock::acquire(&path);
    assert!(
        matches!(&second, Err(RunError::AlreadyRunning { path: p }) if *p == path),
        "{second:?}"
    );
    drop(first);
    let third = PollLock::acquire(&path);
    assert!(third.is_ok(), "{third:?}");
    Ok(())
}

/// The loop takes the lock before any tmux call and refuses to start while
/// another poller holds it.
#[test]
fn loop_refuses_to_start_while_locked() -> TestResult {
    let env = Env::new(&[])?;
    let _held = PollLock::acquire(&env.config.lock())?;
    let tmux = fleet(&[], "");
    let clock = FakeClock::new(0.0);
    let r = env
        .runner(&tmux, &clock, &QuietHost)
        .run_loop("60", Some(1));
    assert!(matches!(r, Err(RunError::AlreadyRunning { .. })), "{r:?}");
    assert!(tmux.calls().is_empty());
    Ok(())
}

/// A dry run takes no lock, writes no file, sends no keys, sleeps no
/// interval and runs one cycle.
#[test]
fn dry_run_sends_nothing_and_writes_nothing() -> TestResult {
    let env = Env::new(&[])?;
    let tmux = fleet(
        &[("tm-architect", "%1", "claude"), ("tm-a", "%2", "claude")],
        &screen(50),
    )
    .fallback("display-message", "tm-architect\n")
    .fallback("send-keys", "");
    let clock = FakeClock::new(1_000.0);
    let mut runner = env.runner(&tmux, &clock, &QuietHost);
    runner.dry_run = true;
    runner.run_loop("60", None)?;
    let files: Vec<String> = fs::read_dir(&env.config.inbox)?
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(files, ["events.jsonl"], "only the fixture's own file");
    assert!(tmux.sends().is_empty());
    assert_eq!(interval_sleeps(&clock, MINUTE), 0);
    let lists = tmux.calls().iter().filter(|c| c[0] == "list-panes").count();
    assert_eq!(lists, 1, "one cycle");
    Ok(())
}
