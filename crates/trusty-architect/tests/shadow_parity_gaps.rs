//! MB1595: the shadow-parity gaps between the Rust poller and the deployed
//! `fleet-poll.py`, one regression test per gap (the ctx triage gap lives in
//! `tests/triage_rules.rs`). Every file is in a tempdir; tmux is scripted.

mod cycle_support;

use std::fs;

use cycle_support::{Env, QuietHost, TestResult, fleet, screen};
use trusty_architect::classify::{DedupState, PaneState, dedup, evaluate, parse_pane};
use trusty_architect::poller::PollSession;
use trusty_architect::sched::parity::{self, compute};
use trusty_architect::sched::shadow::{ShadowAlert, ShadowRecord};
use trusty_architect::sched::slot::parse_iso;
use trusty_architect::{Clock, FakeClock};

// --- gap 1: idle fires once per screen, with no re-emit interval -----------

/// `test_fleet_poll_idle.py`'s `_screen(body, clock)`.
fn idle_screen(body: &str, clock: &str) -> String {
    let r = "─".repeat(40);
    format!("⏺ {body}\n✻ Brewed for {clock} · ↓ 52.9k tokens\n\n{r}\n❯ \n  ⏵⏵ bypass 12:01 PM\n")
}

/// `_poll`: evaluate + dedup per step; the idle alerts that survive dedup.
fn poll_idle(steps: &[(&str, f64)], ps: &mut PaneState, st: &mut DedupState) -> usize {
    let mut out = 0;
    for (cap, now) in steps {
        let found: Vec<_> = evaluate(&parse_pane(cap), ps, *now, "s", "p")
            .into_iter()
            .filter(|a| a.kind == "idle")
            .collect();
        out += dedup(st, &found, *now).0.len();
    }
    out
}

#[test]
fn test_idle_same_screen_no_repeat() {
    let a = idle_screen("Done.", "6m 34s");
    let ticking = idle_screen("Done.", "9m 12s");
    let steps = [
        (&*a, 1000.0),
        (&*a, 1700.0),
        (&*ticking, 3600.0),
        (&*a, 5500.0),
    ];
    let n = poll_idle(
        &steps,
        &mut PaneState::default(),
        &mut DedupState::default(),
    );
    assert_eq!(
        n, 1,
        "an unchanged idle screen alerts once, never every 1800 s"
    );
}

#[test]
fn test_idle_changed_screen_alerts() {
    let a = idle_screen("Done.", "6m 34s");
    let b = idle_screen("Finished a different task.", "6m 34s");
    let steps = [(&*a, 1000.0), (&*a, 1700.0), (&*b, 3600.0)];
    let n = poll_idle(
        &steps,
        &mut PaneState::default(),
        &mut DedupState::default(),
    );
    assert_eq!(n, 2);
}

#[test]
fn test_idle_new_stretch_alerts() {
    let (mut ps, mut st) = (PaneState::default(), DedupState::default());
    let same = idle_screen("Done.", "6m 34s");
    let r = "─".repeat(40);
    let waiting =
        format!("⏺ x\n✻ Waiting for 3 background agents to finish\n\n{r}\n❯ \n  ⏵⏵ bypass\n");
    let mut n = poll_idle(&[(&same, 1000.0), (&same, 1700.0)], &mut ps, &mut st);
    n += poll_idle(&[(&waiting, 2000.0)], &mut ps, &mut st);
    n += poll_idle(&[(&same, 3000.0), (&same, 3700.0)], &mut ps, &mut st);
    assert_eq!(n, 2);
}

// --- gap 3: selftest_fail and pm_message read the live inbox ---------------

/// The shadow points `ARCHITECT_INBOX_DIR` at a scratch inbox; the read-only
/// inputs still come from the live one. Red before MB1595: `selftest_fail`
/// read the scratch inbox and `pm_message` had no step.
#[test]
fn shadow_reads_selftest_and_pm_messages_from_the_live_inbox() -> TestResult {
    let live = tempfile::tempdir()?;
    let live_dir = live.path().display().to_string();
    fs::write(
        live.path().join("selftest-state.json"),
        r#"{"failures": ["factory"], "lines": ["factory: overdue"]}"#,
    )?;
    fs::write(
        live.path().join("pm-messages.jsonl"),
        concat!(
            r#"{"ts": "2026-11-21T21:27:33Z", "session": "tm-b", "kind": "done", "pr": 7, "text": "merged"}"#,
            "\n",
            r#"{"ts": "2026-11-21T21:27:34Z", "session": "tm-b", "kind": "ack", "text": "ok"}"#,
            "\n"
        ),
    )?;
    let env = Env::new(&[("ARCHITECT_READ_INBOX_DIR", &live_dir)])?;
    let tmux = fleet(&[("tm-a", "%2", "claude")], &screen(10));
    let clock = FakeClock::new(parse_iso("2026-11-21T22:00:00Z").unwrap_or_default() as f64);
    let runner = env.runner(&tmux, &clock, &QuietHost);
    let (mut session, _) = PollSession::load(&env.config, false, clock.now_unix());
    let summary = runner.run_once(&mut session)?;
    let kinds: Vec<(&str, &str, &str)> = summary
        .new
        .iter()
        .map(|a| (a.kind.as_str(), a.session.as_str(), a.detail.as_str()))
        .collect();
    assert!(
        kinds.iter().any(|(k, _, _)| *k == "selftest_fail"),
        "{kinds:?}"
    );
    let pm: Vec<_> = kinds
        .iter()
        .filter(|(k, _, _)| *k == "pm_message")
        .collect();
    assert_eq!(pm.len(), 1, "the held ack never wakes: {kinds:?}");
    assert_eq!((pm[0].1, pm[0].2), ("tm-b", "done PR#7: merged"));
    Ok(())
}

// --- gap 4: the parity verdict and the wake count --------------------------

fn record(slot: i64, woken: u64, wake: Option<&str>) -> ShadowRecord {
    ShadowRecord {
        ts: "x".into(),
        job: "poll".into(),
        slot,
        mode: "shadow".into(),
        interval_s: 60,
        ok: true,
        panes: Some(3),
        new: Some(woken),
        woken: Some(woken),
        logged: Some(0),
        suppressed: Some(0),
        wake_phrase: wake.map(str::to_owned),
        wake_target: None,
        alerts: Vec::<ShadowAlert>::new(),
        digest_sha: "d".into(),
        error: None,
        haiku_woken: None,
        alert_keys: None,
        woken_keys: None,
    }
}

/// A dry run never sends, so its carried `wake_pending` makes every later
/// cycle print `would send`; that backlog is not a wake decision. Red before
/// MB1595: 102 of 116 cycles counted as scheduler-only wakes.
#[test]
fn parity_counts_only_cycles_whose_triage_woke() {
    let t = parse_iso("2026-11-21T14:00:00Z").unwrap_or_default();
    let would = Some("dry-run: would send to %32: [poll 14:00Z] 9 alerts need you");
    let records = [
        record(t, 1, would),
        record(t + 60, 0, would),
        record(t + 120, 0, would),
    ];
    let rep = compute(&records, &[], &[t + 5]);
    assert_eq!(
        (
            rep.wakes.matched,
            rep.wakes.scheduler_only,
            rep.wakes.poller_only
        ),
        (1, 0, 0)
    );
}

#[test]
fn parity_render_states_percentages_and_a_verdict() {
    let t = parse_iso("2026-11-21T14:00:00Z").unwrap_or_default();
    let rep = compute(
        &[record(t, 1, None), record(t + 60, 1, None)],
        &[],
        &[t + 5],
    );
    let text = parity::render(&rep);
    assert!(text.contains("wake match: 50.0%"), "{text}");
    assert!(text.contains("alert match: 100.0%"), "{text}");
    assert!(text.contains("verdict: FAIL"), "{text}");
}
