//! #11541 review: every self-refresh failure kind is a `self_refresh` alert
//! in the cycle's `failing` set (alerts.jsonl and the push book), not only a
//! log line. Every path lives under a tempdir; nothing touches a live inbox.

mod cycle_support;

use std::fs;
use std::io;
use std::path::Path;
use std::time::Duration;

use cycle_support::{Env, QuietHost, TestResult, listing};
use serde_json::json;
use trusty_architect::ScriptedTmux;
use trusty_architect::classify::Alert;
use trusty_architect::poller::steps::scripts::{ChildOutput, ScriptSteps, Subprocess};
use trusty_architect::poller::{PollSession, PollState, Runner, Triage, load_state};
use trusty_architect::{Clock, FakeClock};

/// Wakes nobody, so the self-refresh step always runs.
struct NoWake;

impl Triage for NoWake {
    fn triage(&self, _: &[Alert], _: f64, _: &mut PollState, _: bool) -> (Vec<Alert>, u64) {
        (Vec::new(), 0)
    }
}

/// `pgrep` finds nothing; a detached start succeeds unless `spawn_fails`.
struct FakeSub {
    spawn_fails: bool,
}

impl Subprocess for FakeSub {
    fn spawn_detached(&self, _: &[String], _: Option<&Path>) -> io::Result<u32> {
        if self.spawn_fails {
            return Err(io::Error::other("spawn refused"));
        }
        Ok(4242)
    }
    fn output(&self, _: &[String], _: Duration) -> io::Result<ChildOutput> {
        Ok(ChildOutput {
            code: Some(1),
            ..ChildOutput::default()
        })
    }
}

fn idle() -> String {
    let rule = "─".repeat(80);
    format!("⏺ Done.\n\n{rule}\n❯\u{a0}\n{rule}\n  ⏵⏵ bypass\n")
}

fn transcript(dir: &Path, usage: serde_json::Value) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    let line = json!({"type": "assistant", "message": {"usage": usage}, "entrypoint": "cli"});
    fs::write(dir.join("t.jsonl"), format!("{line}\n"))
}

/// Runs two cycles; returns the alert fingerprints of kind `self_refresh` in
/// alerts.jsonl and the keys of the saved push book.
fn two_cycles(
    vars: &[(&str, &str)],
    usage: Option<serde_json::Value>,
    capture: Option<&str>,
    spawn_fails: bool,
) -> TestResult<(Vec<String>, Vec<String>)> {
    let env = Env::new(vars)?;
    if let Some(u) = usage {
        transcript(&env.config.self_ctx_dir, u)?;
    }
    let mut tmux = ScriptedTmux::new()
        .fallback("list-panes", &listing(&[("tm-architect", "%1", "claude")]))
        .fallback("list-windows", "");
    if let Some(c) = capture {
        tmux = tmux.fallback("capture-pane", c);
    }
    let clock = FakeClock::new(1_763_294_400.0);
    let sub = FakeSub { spawn_fails };
    let steps = ScriptSteps::new(&sub, &env.config);
    let runner = Runner {
        triage: &NoWake,
        steps: Some(&steps),
        ..env.runner(&tmux, &clock, &QuietHost)
    };
    let (mut session, _) = PollSession::load(&env.config, false, clock.now_unix());
    let _ = runner.run_once(&mut session);
    clock.sleep(Duration::from_secs(60));
    let _ = runner.run_once(&mut session);
    let mut fps = Vec::new();
    if let Ok(text) = fs::read_to_string(env.config.alerts()) {
        for line in text.lines() {
            let v: serde_json::Value = serde_json::from_str(line)?;
            if v["kind"] == "self_refresh" {
                fps.push(v["fingerprint"].as_str().unwrap_or_default().to_owned());
            }
        }
    }
    let (state, _) = load_state(&env.state_path());
    let push = state
        .other
        .get("push")
        .and_then(|p| p.as_object())
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default();
    Ok((fps, push))
}

fn assert_kind(kind: &str, got: &(Vec<String>, Vec<String>)) {
    assert!(
        got.0.iter().any(|f| f == kind),
        "{kind}: alerts {:?}",
        got.0
    );
    let key = format!("self_refresh:{kind}");
    assert!(got.1.contains(&key), "{kind}: push book {:?}", got.1);
}

fn sixty() -> serde_json::Value {
    json!({"input_tokens": 600_000})
}

/// A bad `SELF_REFRESH_PCT` is a `config` fault alert.
#[test]
fn self_refresh_config_fault_is_an_alert() -> TestResult {
    let got = two_cycles(&[("SELF_REFRESH_PCT", "lots")], None, Some(&idle()), false)?;
    assert_kind("config", &got);
    Ok(())
}

/// A zero `SELF_CTX_WINDOW` is a `window` fault alert.
#[test]
fn self_refresh_zero_window_is_an_alert() -> TestResult {
    let got = two_cycles(&[("SELF_CTX_WINDOW", "0")], None, Some(&idle()), false)?;
    assert_kind("window", &got);
    Ok(())
}

/// Unreadable self-ctx tokens are a `tokens` fault alert.
#[test]
fn self_refresh_unreadable_tokens_is_an_alert() -> TestResult {
    let bad = json!({"input_tokens": "many"});
    let got = two_cycles(&[], Some(bad), Some(&idle()), false)?;
    assert_kind("tokens", &got);
    Ok(())
}

/// A failed Architect-pane capture is an alert from both readers.
#[test]
fn self_refresh_capture_failure_is_an_alert() -> TestResult {
    let got = two_cycles(&[], Some(sixty()), None, false)?;
    assert_kind("capture", &got);
    assert_kind("architect-capture", &got);
    Ok(())
}

/// A failed `self-clear.sh` start is a `start` fault alert.
#[test]
fn self_refresh_failed_start_is_an_alert() -> TestResult {
    let got = two_cycles(&[], Some(sixty()), Some(&idle()), true)?;
    assert_kind("start", &got);
    Ok(())
}

/// A healthy run raises no `self_refresh` alert.
#[test]
fn a_clean_self_refresh_raises_nothing() -> TestResult {
    let cap = idle();
    let got = two_cycles(&[], Some(sixty()), Some(&cap), false)?;
    assert!(got.0.is_empty(), "{:?}", got.0);
    Ok(())
}
