//! #11540: the ledger shim (`scripts/fleet-poll-ledger.py`) run by the
//! poller each cycle. A fake runner stands in for the child; every path is a
//! tempdir. Each failure test is red against a shim runner that only logs.

mod cycle_support;

use std::cell::RefCell;
use std::fs;
use std::io;
use std::path::Path;
use std::time::Duration;

use cycle_support::{Env, QuietHost, TestResult, fleet, screen};
use serde_json::json;
use trusty_architect::poller::steps::ledger_shim::{SHIM_ALERT, SHIM_TIMEOUT, run_ledger_shim};
use trusty_architect::poller::steps::scripts::{ChildOutput, ScriptSteps, Subprocess};
use trusty_architect::poller::{Config, PollSession, Runner, load_state};
use trusty_architect::{Clock, FakeClock};

/// Answers the shim with `shim`; every other child exits 0 with no output.
struct FakeShim {
    shim: Box<dyn Fn() -> io::Result<ChildOutput>>,
    calls: RefCell<Vec<(Vec<String>, Duration)>>,
}

impl FakeShim {
    fn new(shim: impl Fn() -> io::Result<ChildOutput> + 'static) -> Self {
        Self {
            shim: Box::new(shim),
            calls: RefCell::new(Vec::new()),
        }
    }

    fn shim_calls(&self) -> Vec<(Vec<String>, Duration)> {
        let calls = self.calls.borrow();
        let is_shim = |a: &Vec<String>| a.iter().any(|s| s.ends_with("fleet-poll-ledger.py"));
        calls.iter().filter(|(a, _)| is_shim(a)).cloned().collect()
    }
}

impl Subprocess for FakeShim {
    fn spawn_detached(&self, _: &[String], _: Option<&Path>) -> io::Result<u32> {
        Ok(1)
    }
    fn output(&self, argv: &[String], timeout: Duration) -> io::Result<ChildOutput> {
        self.calls.borrow_mut().push((argv.to_vec(), timeout));
        if argv.iter().any(|a| a.ends_with("fleet-poll-ledger.py")) {
            return (self.shim)();
        }
        Ok(ChildOutput {
            code: Some(0),
            ..ChildOutput::default()
        })
    }
}

fn exits(code: Option<i32>, stdout: &str, stderr: &str) -> io::Result<ChildOutput> {
    Ok(ChildOutput {
        code,
        stdout: stdout.to_owned(),
        stderr: stderr.to_owned(),
    })
}

const OK_OUT: &str = r#"{"alerts": [{"ts": "2026-11-16T12:00:00Z", "session": "", "pane": "", "kind": "dream", "fingerprint": "dream|2026-11-16", "detail": "d", "display": "[dashboard]: dream pass due"}], "state": {"dream": {"last": "2026-11-16"}, "alerts": {"x": 1}}}"#;

/// Writes `<root>/scripts/fleet-poll-ledger.py` (the fake runs nothing).
fn with_script(env: &Env) -> io::Result<()> {
    fs::create_dir_all(env.root().join("scripts"))?;
    fs::write(env.root().join("scripts/fleet-poll-ledger.py"), "")
}

#[test]
fn ledger_shim_success_raises_no_alert() -> TestResult {
    let env = Env::new(&[])?;
    with_script(&env)?;
    let fake = FakeShim::new(|| exits(Some(0), OK_OUT, ""));
    let run = run_ledger_shim(&ScriptSteps::new(&fake, &env.config), &env.config, 1000.0);
    assert!(run.failing.is_empty(), "{:?}", run.failing);
    assert_eq!(run.alerts.len(), 1);
    assert_eq!(run.alerts[0].display, "[dashboard]: dream pass due");
    // Only the shim's own keys merge; a typed key is never overwritten.
    assert_eq!(
        serde_json::Value::Object(run.state),
        json!({"dream": {"last": "2026-11-16"}})
    );
    let calls = fake.shim_calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].1, SHIM_TIMEOUT);
    assert!(!calls[0].0.contains(&"--shadow".to_owned()));
    Ok(())
}

/// Each kind: missing script, cannot start, timeout (killed), non-zero exit,
/// unreadable output. Each is one `ledger_shim` alert with its fingerprint.
#[test]
fn ledger_shim_each_failure_kind_raises_an_alert() -> TestResult {
    let env = Env::new(&[])?;
    let cases: Vec<(&str, bool, FakeShim)> = vec![
        (
            "missing",
            false,
            FakeShim::new(|| exits(Some(0), OK_OUT, "")),
        ),
        (
            "start",
            true,
            FakeShim::new(|| Err(io::Error::new(io::ErrorKind::NotFound, "no python3"))),
        ),
        (
            "timeout",
            true,
            FakeShim::new(|| {
                Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "timed out after 240 seconds",
                ))
            }),
        ),
        (
            "exit",
            true,
            FakeShim::new(|| {
                exits(
                    Some(1),
                    OK_OUT,
                    "fleet-poll-ledger: reminder failed: reminders ERROR x\n",
                )
            }),
        ),
        (
            "output",
            true,
            FakeShim::new(|| exits(Some(0), "not json\n", "")),
        ),
    ];
    for (kind, script, fake) in cases {
        let _ = fs::remove_dir_all(env.root().join("scripts"));
        if script {
            with_script(&env)?;
        }
        let run = run_ledger_shim(&ScriptSteps::new(&fake, &env.config), &env.config, 1000.0);
        let got: Vec<(&str, &str)> = run
            .failing
            .iter()
            .map(|a| (a.kind.as_str(), a.fingerprint.as_str()))
            .collect();
        assert_eq!(got, [(SHIM_ALERT, kind)], "{kind}: {:?}", run.failing);
        assert_eq!(run.failing[0].session, env.config.supervisor, "{kind}");
        assert!(
            run.log.iter().any(|l| l.contains("ledger shim ERROR")),
            "{kind}"
        );
        if kind == "exit" {
            assert!(
                run.failing[0].detail.contains("reminder failed"),
                "{:?}",
                run.failing
            );
            assert_eq!(run.alerts.len(), 1, "the steps that ran still alert");
        }
    }
    Ok(())
}

#[test]
fn ledger_shim_shadow_passes_the_shadow_flag() -> TestResult {
    let env = Env::new(&[])?;
    with_script(&env)?;
    let mut cfg: Config = env.config.clone();
    cfg.shadow = true;
    let fake = FakeShim::new(|| exits(Some(0), OK_OUT, ""));
    run_ledger_shim(&ScriptSteps::new(&fake, &cfg), &cfg, 1000.0);
    let argv = fake.shim_calls().remove(0).0;
    let shadow = cfg.shadow_dir().to_string_lossy().into_owned();
    let state = cfg
        .shadow_dir()
        .join("poll-state.json")
        .to_string_lossy()
        .into_owned();
    let at = |flag: &str| {
        argv.iter()
            .position(|a| a == flag)
            .map(|i| argv[i + 1].clone())
    };
    assert_eq!(at("--shadow"), Some(shadow));
    assert_eq!(at("--state"), Some(state));
    Ok(())
}

fn runner_cycle(env: &Env, fake: &FakeShim) -> TestResult<Vec<String>> {
    let tmux = fleet(&[("tm-a", "%2", "claude")], &screen(10));
    let clock = FakeClock::new(1_763_294_400.0);
    let steps = ScriptSteps::new(fake, &env.config);
    let runner = Runner {
        steps: Some(&steps),
        ..env.runner(&tmux, &clock, &QuietHost)
    };
    let (mut session, _) = PollSession::load(&env.config, false, clock.now_unix());
    let summary = runner.run_once(&mut session)?;
    Ok(summary.new.iter().map(|a| a.kind.clone()).collect())
}

/// A non-zero exit reaches alerts.jsonl and the `push_due` book (the
/// `failing` set), and the shim's state key is saved.
#[test]
fn ledger_shim_failure_reaches_the_cycle_failing_set() -> TestResult {
    let env = Env::new(&[])?;
    with_script(&env)?;
    let fake = FakeShim::new(|| exits(Some(1), OK_OUT, "fleet-poll-ledger: dream failed: x\n"));
    let kinds = runner_cycle(&env, &fake)?;
    assert!(kinds.contains(&SHIM_ALERT.to_owned()), "{kinds:?}");
    assert!(kinds.contains(&"dream".to_owned()), "{kinds:?}");
    let (state, err) = load_state(&env.state_path());
    assert!(err.is_none(), "{err:?}");
    assert!(
        state.other["push"].get("ledger_shim:exit").is_some(),
        "{:?}",
        state.other.get("push")
    );
    assert_eq!(state.other["dream"], json!({"last": "2026-11-16"}));
    assert_eq!(fake.shim_calls().len(), 1, "once per cycle");
    Ok(())
}
