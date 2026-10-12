//! #9171 A3b-2, plan AC3 with a real kill: a poller SIGKILLed after its
//! alert append and before its state save re-emits the same alerts, by dedup
//! key, when restarted, and its state file parses.
//!
//! The child is this test binary re-run on this one test with an env var
//! naming a tempdir root. It pauses on a sentinel file after the append (not
//! a timer), so the kill always lands between append and save. Ignored: it
//! spawns a process; run with `--include-ignored`.

mod cycle_support;

use std::fs;
use std::io;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use cycle_support::{Env, QuietHost, TestResult, config_at, fleet, screen};
use trusty_architect::poller::{Phase, PollSession, Runner, WakeAll, load_state};
use trusty_architect::{Clock, FakeClock};

const CHILD_ENV: &str = "TRUSTY_ARCHITECT_A3B2_KILL_ROOT";

/// The child: one run that parks forever once its alerts are appended.
fn child(root: &Path) -> TestResult {
    let config = config_at(root, &[])?;
    let tmux = fleet(&[("tm-a", "%2", "claude")], &screen(50));
    let clock = FakeClock::new(1_000.0);
    let runner = Runner {
        tmux: &tmux,
        clock: &clock,
        probe: &QuietHost,
        triage: &WakeAll,
        config: &config,
        dry_run: false,
    };
    let (mut session, _) = PollSession::load(&config, false, clock.now_unix());
    let sentinel = root.join("appended");
    let _ = runner.run_once_with(&mut session, &mut |p| {
        if p == Phase::AlertsAppended {
            fs::write(&sentinel, "")?;
            loop {
                std::thread::sleep(Duration::from_millis(50));
            }
        }
        Ok::<(), io::Error>(())
    });
    Ok(())
}

#[test]
#[ignore = "spawns and SIGKILLs a child poller; run with --include-ignored"]
fn sigkill_between_append_and_save_reemits_identical_keys() -> TestResult {
    if let Some(root) = std::env::var_os(CHILD_ENV) {
        return child(Path::new(&root));
    }
    let env = Env::new(&[])?;
    let mut kid = Command::new(std::env::current_exe()?)
        .args([
            "sigkill_between_append_and_save_reemits_identical_keys",
            "--exact",
            "--include-ignored",
            "--test-threads=1",
        ])
        .env(CHILD_ENV, env.root())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let sentinel = env.root().join("appended");
    let start = Instant::now();
    while !sentinel.exists() && start.elapsed() < Duration::from_secs(30) {
        std::thread::sleep(Duration::from_millis(20));
    }
    kid.kill()?; // SIGKILL on Unix
    kid.wait()?;
    assert!(sentinel.exists(), "the child never reached the append");
    let first = env.keys()?;
    assert!(!first.is_empty(), "the child appended its alerts");
    let (state, err) = load_state(&env.state_path());
    assert!(err.is_none(), "{err:?}");
    assert!(state.alerts.is_empty(), "the kill landed before the save");

    let tmux = fleet(&[("tm-a", "%2", "claude")], &screen(50));
    let clock = FakeClock::new(1_060.0);
    let (mut restarted, _) = PollSession::load(&env.config, false, clock.now_unix());
    env.runner(&tmux, &clock, &QuietHost)
        .run_once(&mut restarted)?;
    let all = env.keys()?;
    assert_eq!(all[first.len()..], first[..], "same keys re-emitted");
    let (_, err) = load_state(&env.state_path());
    assert!(err.is_none(), "{err:?}");
    Ok(())
}
