//! #9801 slice 1 Fail-Open Check: a failed probe launch or a failed macOS
//! notification never advances state as if it had worked, and never stalls
//! the poller.
//!
//! Every path lives in a tempdir. No notification is sent: each `osascript`
//! here is a fake program written by the test, or the recording fake.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{Map, Value, json};
use trusty_architect::classify::{Alert, alert};
use trusty_architect::poller::steps::notify_macos::{
    Notifier, NotifyError, Osascript, RecordingNotifier, WATCHDOG_TITLE, applescript,
    notify_watchdog_alerts,
};
use trusty_architect::poller::steps::usage::{
    ProbeLauncher, RecordingProbe, ScriptProbe, UsagePaths, UtcTime, usage_step,
};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const GM: &str = "a2@example.com";

fn paths(dir: &Path) -> UsagePaths {
    UsagePaths {
        ledger: dir.join("ledger.jsonl"),
        cache: dir.join("cache.json"),
        claude_json: dir.join("claude.json"),
        probe_script: dir.join("usage-probe.sh"),
        probe_log: dir.join("usage-probe.log"),
    }
}

/// A ledger whose current account (gmail) owes a `wk>=80` and a `cutover` alert.
fn ledger_with_alerts(p: &UsagePaths) -> TestResult {
    let rows = [
        json!({"ts": "2026-10-07T14:30:00Z", "account": GM, "pct5h": 1, "reset5h": "2026-10-07T20:50:00Z",
               "pctwk": 60, "resetwk": "2026-10-09T07:00:00Z", "source": "manual"}),
        json!({"ts": "2026-10-07T16:12:00Z", "account": GM, "pct5h": 9, "reset5h": "2026-10-07T20:50:00Z",
               "pctwk": 84, "resetwk": "2026-10-09T07:00:00Z", "source": "manual"}),
        json!({"ts": "2026-10-07T16:18:00Z", "account": "next@x", "pct5h": 0, "reset5h": null,
               "pctwk": 50, "resetwk": "2026-10-12T11:00:00Z", "source": "manual"}),
    ];
    let text: String = rows.iter().map(|r| format!("{r}\n")).collect();
    fs::write(&p.ledger, text)?;
    fs::write(
        &p.claude_json,
        json!({"oauthAccount": {"emailAddress": GM}}).to_string(),
    )?;
    Ok(())
}

fn now() -> TestResult<f64> {
    Ok(UtcTime::parse_iso("2026-10-07T16:20:00Z")?.timestamp())
}

fn idle() -> [(String, String); 1] {
    [("tm-x".to_owned(), "%1".to_owned())]
}

/// #9801: the prototype advances `last_probe` and `tries` before it launches,
/// so a launch that fails counts as a probe and its alerts are lost
/// (rulings.jsonl `poll_step:probe_launch_fails`). Red against a port that
/// advances the probe state first or drops the alerts on a launch failure.
#[test]
fn failed_probe_launch_leaves_probe_state_and_keeps_alerts() -> TestResult {
    let dir = tempfile::tempdir()?;
    let p = paths(dir.path());
    ledger_with_alerts(&p)?;
    let mut state = Map::new();
    let probe = RecordingProbe::failing();
    let (alerts, log) = usage_step(&p, &probe, &mut state, now()?, &idle(), false);
    assert_eq!(probe.launched().len(), 1, "the launch was tried");
    let kinds: Vec<&str> = alerts.iter().map(|a| a.fingerprint.as_str()).collect();
    assert_eq!(
        kinds,
        [
            format!("wk>=80:{GM}:2026-10-09T07:00:00Z"),
            format!("cutover:{GM}:next@x")
        ]
    );
    assert!(
        log.iter().any(|l| l.starts_with("usage probe ERROR ")),
        "{log:?}"
    );
    let usage = state["usage"].as_object().ok_or("usage state")?;
    assert_eq!(
        usage.get("last_probe"),
        None,
        "a failed launch is not a probe"
    );
    assert_eq!(usage["tries"], json!(0));

    // The next cycle tries again, because nothing was recorded as probed.
    let ok = RecordingProbe::default();
    let (_, log) = usage_step(&p, &ok, &mut state, now()? + 60.0, &idle(), false);
    assert!(log.is_empty(), "{log:?}");
    assert_eq!(ok.launched().len(), 1);
    assert_eq!(state["usage"]["last_probe"], json!(now()? + 60.0));
    assert_eq!(state["usage"]["tries"], json!(1));
    Ok(())
}

/// #9801: a sampler failure before the probe decision (an unreadable ledger,
/// a corrupt `usage` state) leaves the stored usage state as it was; the
/// prototype has already recorded the account change by then.
#[test]
fn a_sampler_error_commits_no_usage_state() -> TestResult {
    let dir = tempfile::tempdir()?;
    let p = paths(dir.path());
    ledger_with_alerts(&p)?;
    fs::write(&p.ledger, b"\xff not utf-8\n")?;
    let before = json!({"usage": {"account": "a3@example.com", "since": 0}});
    let mut state = before.as_object().cloned().unwrap_or_default();
    let probe = RecordingProbe::default();
    let (alerts, log) = usage_step(&p, &probe, &mut state, now()?, &idle(), false);
    assert!(alerts.is_empty());
    assert!(probe.launched().is_empty(), "no probe after a failure");
    assert_eq!(log.len(), 1, "{log:?}");
    assert!(log[0].starts_with("usage sampler ERROR "), "{log:?}");
    assert_eq!(Value::Object(state), before, "state unchanged");

    // A `usage` entry that is not an object is reported, not overwritten.
    ledger_with_alerts(&p)?;
    let mut state = json!({"usage": 5}).as_object().cloned().unwrap_or_default();
    let (alerts, log) = usage_step(&p, &probe, &mut state, now()?, &idle(), false);
    assert!(alerts.is_empty() && probe.launched().is_empty());
    assert!(log[0].starts_with("usage sampler ERROR "), "{log:?}");
    assert_eq!(state["usage"], json!(5));
    Ok(())
}

/// An alert computation that fails after the probe decision keeps the probe
/// record: the probe really ran. Only the alerts are lost, with a log line.
#[test]
fn an_alert_failure_after_a_probe_keeps_the_probe_record() -> TestResult {
    let dir = tempfile::tempdir()?;
    let p = paths(dir.path());
    ledger_with_alerts(&p)?;
    let bad = json!({"ts": "2026-10-07T16:19:00Z", "account": "next@x", "pct5h": "lots",
                     "reset5h": null, "pctwk": 50, "resetwk": "2026-10-12T11:00:00Z"});
    let text = fs::read_to_string(&p.ledger)? + &format!("{bad}\n");
    fs::write(&p.ledger, text)?;
    let mut state = Map::new();
    let probe = RecordingProbe::default();
    let (alerts, log) = usage_step(&p, &probe, &mut state, now()?, &idle(), false);
    assert!(alerts.is_empty());
    assert!(log[0].starts_with("usage sampler ERROR "), "{log:?}");
    assert_eq!(probe.launched().len(), 1);
    assert_eq!(state["usage"]["tries"], json!(1));
    Ok(())
}

fn script(dir: &Path, name: &str, body: &str) -> TestResult<PathBuf> {
    let path = dir.join(name);
    fs::write(&path, format!("#!/bin/sh\n{body}\n"))?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755))?;
    Ok(path)
}

/// Waits up to five seconds for `path` to hold a whole line.
fn wait_for_line(path: &Path) -> String {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(5) {
        if let Ok(text) = fs::read_to_string(path)
            && text.ends_with('\n')
        {
            return text;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    String::new()
}

/// The real launcher: `usage-probe.sh <session> <pane> <account>`, detached,
/// output appended to `usage-probe.log`. A missing script is an error.
#[test]
fn script_probe_starts_the_script_detached_and_reports_a_missing_one() -> TestResult {
    let dir = tempfile::tempdir()?;
    let mut p = paths(dir.path());
    let seen = dir.path().join("seen");
    p.probe_script = script(
        dir.path(),
        "usage-probe.sh",
        &format!("echo \"$@\" > '{}'; echo probe-ran", seen.display()),
    )?;
    let probe = ScriptProbe::new(&p);
    probe.launch("tm-x", "%1", GM)?;
    assert_eq!(wait_for_line(&seen), format!("tm-x %1 {GM}\n"));
    assert_eq!(wait_for_line(&p.probe_log), "probe-ran\n");

    let missing = ScriptProbe::new(&UsagePaths {
        probe_script: dir.path().join("no-such-probe.sh"),
        ..p.clone()
    });
    assert!(missing.launch("tm-x", "%1", GM).is_err());
    let no_inbox = ScriptProbe::new(&UsagePaths {
        probe_log: dir.path().join("missing-dir").join("usage-probe.log"),
        ..p
    });
    assert!(no_inbox.launch("tm-x", "%1", GM).is_err());
    Ok(())
}

fn alerts(kinds: &[&str]) -> Vec<Alert> {
    kinds
        .iter()
        .map(|k| alert(k, "fp", "detail", "tm-architect", "%1", 0.0))
        .collect()
}

/// Only `architect_*` alerts notify, under the watchdog title; a failed
/// notification is one `notify ERROR` line and the rest are still sent.
#[test]
fn failed_notifications_are_logged_and_the_rest_still_sent() {
    let new = alerts(&["idle", "architect_dead", "usage", "architect_frozen"]);
    let ok = RecordingNotifier::default();
    assert!(notify_watchdog_alerts(&ok, &new).is_empty());
    let sent = ok.sent();
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[0], (WATCHDOG_TITLE.to_owned(), new[1].display.clone()));
    assert_eq!(sent[1].1, new[3].display);

    let failing = RecordingNotifier::failing();
    let log = notify_watchdog_alerts(&failing, &new);
    assert_eq!(failing.sent().len(), 2, "every notification is tried");
    assert_eq!(log.len(), 2, "{log:?}");
    assert!(
        log.iter().all(|l| l.starts_with("notify ERROR ")),
        "{log:?}"
    );
}

/// A hung `osascript` is killed at the timeout, so it cannot stall the poller.
#[test]
fn osascript_that_hangs_is_killed_at_the_timeout() -> TestResult {
    let dir = tempfile::tempdir()?;
    let notifier = Osascript {
        program: script(dir.path(), "osascript", "sleep 30")?,
        timeout: Duration::from_millis(300),
    };
    let start = Instant::now();
    let got = notifier.notify("Architect watchdog", "x");
    assert!(matches!(got, Err(NotifyError::Timeout(_))), "{got:?}");
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "{:?}",
        start.elapsed()
    );
    Ok(())
}

/// A non-zero exit and a missing program are errors; the prototype ignores
/// the exit status (rulings.jsonl `notify_macos:nonzero_exit`).
#[test]
fn osascript_failures_are_errors() -> TestResult {
    let dir = tempfile::tempdir()?;
    let failing = Osascript {
        program: script(
            dir.path(),
            "osascript",
            "echo 'execution error' >&2; exit 1",
        )?,
        ..Osascript::default()
    };
    match failing.notify("t", "m") {
        Err(NotifyError::Failed { code, stderr }) => {
            assert_eq!(code, Some(1));
            assert_eq!(stderr, "execution error");
        }
        other => return Err(format!("want Failed, got {other:?}").into()),
    }
    let missing = Osascript {
        program: dir.path().join("no-osascript"),
        ..Osascript::default()
    };
    assert!(matches!(
        missing.notify("t", "m"),
        Err(NotifyError::Spawn(_))
    ));
    Ok(())
}

/// The program receives exactly `-e <script>`, the script escaped as the
/// prototype escapes it.
#[test]
fn osascript_receives_dash_e_and_the_script() -> TestResult {
    let dir = tempfile::tempdir()?;
    let seen = dir.path().join("argv");
    let notifier = Osascript {
        program: script(
            dir.path(),
            "osascript",
            &format!("printf '%s\\n' \"$@\" > '{}'", seen.display()),
        )?,
        ..Osascript::default()
    };
    notifier.notify("T\"1", "say \"hi\" \\ there")?;
    let want = format!("-e\n{}\n", applescript("T\"1", "say \"hi\" \\ there"));
    assert_eq!(fs::read_to_string(&seen)?, want);
    assert_eq!(
        applescript("T\"1", "say \"hi\" \\ there"),
        r#"display notification "say \"hi\" \\ there" with title "T\"1""#
    );
    Ok(())
}

/// True while `pid` still names a process (a zombie counts).
fn alive(pid: &str) -> bool {
    std::process::Command::new("kill")
        .args(["-0", pid])
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// #9801: the reaper waited on the child with no deadline, so a probe that
/// hangs held its thread and its process for good. Past the deadline the
/// probe is killed with its whole process group and reaped.
#[test]
fn a_probe_past_its_deadline_is_killed_with_its_group_and_reaped() -> TestResult {
    let dir = tempfile::tempdir()?;
    let mut p = paths(dir.path());
    let pids = dir.path().join("pids");
    p.probe_script = script(
        dir.path(),
        "usage-probe.sh",
        &format!("sleep 30 &\necho \"$$ $!\" > '{}'\nwait", pids.display()),
    )?;
    let probe = ScriptProbe::new(&p).with_deadline(Duration::from_millis(300));
    probe.launch("tm-x", "%1", GM)?;
    let line = wait_for_line(&pids);
    let both: Vec<&str> = line.split_whitespace().collect();
    assert_eq!(both.len(), 2, "{line:?}");
    let start = Instant::now();
    while both.iter().any(|pid| alive(pid)) && start.elapsed() < Duration::from_secs(10) {
        std::thread::sleep(Duration::from_millis(50));
    }
    let gone = both.iter().all(|pid| !alive(pid));
    // Best-effort cleanup when the assertion below is about to fail.
    let _ = std::process::Command::new("kill")
        .args(["-KILL", "--", &format!("-{}", both[0])])
        .stderr(std::process::Stdio::null())
        .status();
    assert!(gone, "the probe and its child outlived the deadline: {line:?}");
    Ok(())
}
