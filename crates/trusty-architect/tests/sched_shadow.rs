//! Shadow mode and the parity join (MB1595).

mod sched_support;

use sched_support::*;
use trusty_architect::FakeClock;
use trusty_architect::sched::config::PollJob;
use trusty_architect::sched::parity::{self, LiveAlert, compute};
use trusty_architect::sched::shadow::{self, ShadowAlert, ShadowRecord, parse_decision};
use trusty_architect::sched::slot::parse_iso;

const DRY_RUN_OUT: &str = "\
2026-11-21T14:33:05Z panes=35 new=2 woken=2 logged=0 suppressed=31 wake=dry-run: would send to tm-architect: [tm-a]: idle: x architect=ok
  would alert: [tm-trusty-issues]: ctx: ctx 34%
  would alert: [tm-sched]: idle: idle 12m
  would notify: [tm-sched]: idle: idle 12m
";

fn alert(session: &str, kind: &str) -> ShadowAlert {
    ShadowAlert {
        session: session.to_owned(),
        kind: kind.to_owned(),
    }
}

fn record(slot: i64, ok: bool, wake: Option<&str>, alerts: Vec<ShadowAlert>) -> ShadowRecord {
    ShadowRecord {
        ts: "x".into(),
        job: "poll".into(),
        slot,
        mode: "shadow".into(),
        interval_s: 60,
        ok,
        panes: Some(3),
        new: Some(alerts.len() as u64),
        woken: Some(0),
        logged: Some(0),
        suppressed: Some(0),
        wake_phrase: wake.map(str::to_owned),
        wake_target: None,
        alerts,
        digest_sha: "d".into(),
        error: None,
    }
}

#[test]
fn parse_decision_reads_the_cycle_and_alerts() {
    let d = parse_decision(DRY_RUN_OUT);
    assert_eq!(
        (d.panes, d.new, d.woken, d.logged, d.suppressed),
        (Some(35), Some(2), Some(2), Some(0), Some(31))
    );
    assert_eq!(
        d.wake.as_deref(),
        Some("dry-run: would send to tm-architect: [tm-a]: idle: x")
    );
    assert_eq!(
        d.alerts,
        [alert("tm-trusty-issues", "ctx"), alert("tm-sched", "idle")]
    );
    // Nothing printed: nothing parsed, never a guess.
    let none = parse_decision("");
    assert_eq!((none.panes, none.wake, none.alerts.len()), (None, None, 0));
    let live = parse_decision(
        "2026-11-21T14:33:05Z panes=35 new=1 woken=0 logged=1 suppressed=31 wake=none architect=ok",
    );
    assert_eq!(live.wake.as_deref(), Some("none"));
}

#[test]
fn shadow_plan_seeds_a_scratch_inbox_and_never_writes_the_live_one() {
    let dir = tempfile::tempdir().unwrap();
    let l = layout(dir.path());
    std::fs::create_dir_all(dir.path().join("inbox")).unwrap();
    std::fs::write(dir.path().join("inbox/poll-state.json"), r#"{"panes":{}}"#).unwrap();
    let before = std::fs::read(dir.path().join("inbox/poll-state.json")).unwrap();
    let env = vec![
        ("ARCHITECT_INBOX_DIR".to_owned(), "/live/inbox".to_owned()),
        ("PATH".to_owned(), "/bin".to_owned()),
    ];
    let (spec, ctx) = shadow::plan(&l, &PollJob::default(), env, T0).unwrap();
    assert_eq!(spec.argv, ["tm", "architect", "poll", "--dry-run"]);
    let scratch = l.shadow_dir();
    // The dry run points at the scratch inbox, never the live one.
    let inbox: Vec<_> = spec
        .env
        .iter()
        .filter(|(k, _)| k == "ARCHITECT_INBOX_DIR")
        .collect();
    assert_eq!(inbox.len(), 1);
    assert_eq!(inbox[0].1, scratch.display().to_string());
    assert_eq!(
        std::fs::read_to_string(scratch.join("poll-state.json")).unwrap(),
        r#"{"panes":{}}"#
    );
    assert_eq!(
        std::fs::read(dir.path().join("inbox/poll-state.json")).unwrap(),
        before
    );
    assert_eq!(ctx.interval_s, 60);
    // Without live state the stale scratch copy is removed.
    std::fs::remove_file(dir.path().join("inbox/poll-state.json")).unwrap();
    shadow::plan(&l, &PollJob::default(), vec![], T0).unwrap();
    assert!(!scratch.join("poll-state.json").exists());
}

#[test]
fn shadow_record_carries_the_contract_fields() {
    let dir = tempfile::tempdir().unwrap();
    let l = layout(dir.path());
    let (spec, ctx) = shadow::plan(&l, &PollJob::default(), vec![], T0).unwrap();
    std::fs::write(&spec.stdout, DRY_RUN_OUT).unwrap();
    shadow::record(&l, &ctx, &ok(), T0, T0).unwrap();
    let line = &lines(&l.shadow_log())[0];
    let v: serde_json::Value = serde_json::from_str(line).unwrap();
    for k in [
        "ts",
        "job",
        "slot",
        "mode",
        "panes",
        "new",
        "woken",
        "logged",
        "suppressed",
        "wake_phrase",
        "wake_target",
        "alerts",
        "digest_sha",
    ] {
        assert!(v.get(k).is_some(), "shadow record lacks {k}");
    }
    assert_eq!(
        (v["mode"].as_str(), v["job"].as_str(), v["ok"].as_bool()),
        (Some("shadow"), Some("poll"), Some(true))
    );
    assert_eq!(v["wake_target"].as_str(), Some("tm-architect"));
    // No alert detail text: session and kind only.
    assert!(!line.contains("ctx 34%") && !line.contains("idle 12m"));
    assert_eq!(
        v["alerts"][0],
        serde_json::json!({"session": "tm-trusty-issues", "kind": "ctx"})
    );
    assert_eq!(v["digest_sha"].as_str().unwrap().len(), 12);
}

#[test]
fn failed_shadow_run_is_recorded_as_failed() {
    let dir = tempfile::tempdir().unwrap();
    let l = layout(dir.path());
    let (_, ctx) = shadow::plan(&l, &PollJob::default(), vec![], T0).unwrap();
    shadow::record(&l, &ctx, &timed_out(), T0, T0).unwrap();
    shadow::record(&l, &ctx, &failed(), T0 + 60, T0 + 60).unwrap();
    // Exit 0 but no cycle line: not a pass either.
    shadow::record(&l, &ctx, &ok(), T0 + 120, T0 + 120).unwrap();
    let recs: Vec<ShadowRecord> = lines(&l.shadow_log())
        .iter()
        .map(|x| serde_json::from_str(x).unwrap())
        .collect();
    assert!(recs.iter().all(|r| !r.ok));
    assert_eq!(recs[0].error.as_deref(), Some("timeout"));
    assert!(recs[2].error.as_deref().unwrap().contains("no cycle line"));
}

#[test]
fn poll_job_shadows_until_cutover() {
    let dir = tempfile::tempdir().unwrap();
    let (clock, runner) = (FakeClock::new(T0 as f64), FakeRunner::new());
    *runner.stdout_text.lock().unwrap() = Some(DRY_RUN_OUT.to_owned());
    let jobs = script("job1", "1h", "");
    let mut e = engine(
        dir.path(),
        &clock,
        &runner,
        reg_with(&jobs, r#""poll":{"every_s":60}"#),
    );
    // First sight runs the poll job at once, as a shadow dry-run.
    let (started, records) = tick_and_wait(&mut e);
    assert_eq!(started, ["poll"]);
    assert_eq!(records[0].job, "poll");
    let spec = runner.last();
    assert_eq!(spec.argv.last().map(String::as_str), Some("--dry-run"));
    assert!(spec.env.iter().any(|(k, _)| k == "ARCHITECT_INBOX_DIR"));
    assert!(spec.env.iter().all(|(k, _)| k != "SECRET_TOKEN"));
    assert_eq!(lines(&layout(dir.path()).shadow_log()).len(), 1);
    // Not due again for 60 s.
    advance(&clock, 59);
    assert!(tick_and_wait(&mut e).0.is_empty());
    // After cutover the poll job runs the live argv and writes no shadow line.
    std::fs::write(layout(dir.path()).cutover(), r#"{"mode":"live","at":"x"}"#).unwrap();
    advance(&clock, 1);
    assert_eq!(tick_and_wait(&mut e).0, ["poll"]);
    assert_eq!(runner.last().argv, ["tm", "architect", "poll", "--once"]);
    assert_eq!(lines(&layout(dir.path()).shadow_log()).len(), 1);
    // A corrupt marker is an error, not a guess: the poll job does not start.
    std::fs::write(layout(dir.path()).cutover(), "{broken").unwrap();
    advance(&clock, 60);
    let (started, records) = tick_and_wait(&mut e);
    assert!(started.is_empty());
    assert_eq!(records[0].outcome, "fail");
}

#[test]
fn poll_slot_that_finds_the_previous_cycle_alive_is_an_overrun() {
    let dir = tempfile::tempdir().unwrap();
    let (clock, runner) = (FakeClock::new(T0 as f64), FakeRunner::new());
    *runner.stdout_text.lock().unwrap() = Some(DRY_RUN_OUT.to_owned());
    runner.hold.store(true, std::sync::atomic::Ordering::SeqCst);
    let mut e = engine(
        dir.path(),
        &clock,
        &runner,
        reg_with(&script("job1", "1h", ""), r#""poll":{"every_s":60}"#),
    );
    assert_eq!(e.tick().unwrap().started, ["poll"]);
    advance(&clock, 60);
    let rep = e.tick().unwrap();
    assert_eq!(rep.records[0].outcome, "overrun");
    // The held run may not have reached the runner yet; it never exceeds one.
    let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while runner.count() < 1 && std::time::Instant::now() < until {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert_eq!(runner.count(), 1);
    runner
        .hold
        .store(false, std::sync::atomic::Ordering::SeqCst);
    e.wait_idle(std::time::Duration::from_secs(10));
}

#[test]
fn parity_joins_matched_scheduler_only_and_poller_only() {
    let t = parse_iso("2026-11-21T14:00:00Z").unwrap();
    let records = [record(
        t,
        true,
        None,
        vec![alert("s1", "ctx"), alert("s2", "idle")],
    )];
    let live = [
        LiveAlert {
            ts: t + 10,
            key: alert("s1", "ctx"),
        },
        LiveAlert {
            ts: t + 20,
            key: alert("s3", "error"),
        },
        // Outside the window: not counted against this record.
        LiveAlert {
            ts: t + 60,
            key: alert("s9", "idle"),
        },
    ];
    let rep = compute(&records, &live, &[]);
    assert_eq!(
        (
            rep.alerts.matched,
            rep.alerts.scheduler_only,
            rep.alerts.poller_only
        ),
        (1, 1, 1)
    );
    assert_eq!(rep.cycles, 1);
}

#[test]
fn parity_counts_wakes_per_window() {
    let t = parse_iso("2026-11-21T14:00:00Z").unwrap();
    let wake = Some("dry-run: would send to tm-architect: x");
    let records = [
        record(t, true, wake, vec![]),       // both wake
        record(t + 60, true, wake, vec![]),  // scheduler only
        record(t + 120, true, None, vec![]), // poller only
        record(t + 180, true, None, vec![]), // neither
    ];
    let rep = compute(&records, &[], &[t + 5, t + 125]);
    assert_eq!(
        (
            rep.wakes.matched,
            rep.wakes.scheduler_only,
            rep.wakes.poller_only
        ),
        (1, 1, 1)
    );
    assert!((rep.match_pct() - 100.0 / 3.0).abs() < 0.01);
    assert!(parity::render(&rep).contains("wakes: matched 1 / scheduler-only 1 / poller-only 1"));
}

#[test]
fn parity_ignores_failed_shadow_records() {
    let t = parse_iso("2026-11-21T14:00:00Z").unwrap();
    let records = [
        record(t, false, None, vec![alert("s1", "ctx")]),
        record(t + 60, true, None, vec![]),
    ];
    let live = [LiveAlert {
        ts: t + 5,
        key: alert("s1", "ctx"),
    }];
    let rep = compute(&records, &live, &[]);
    assert_eq!((rep.cycles, rep.failed_cycles), (1, 1));
    assert_eq!(
        rep.alerts.poller_only, 0,
        "a failed shadow run is no evidence either way"
    );
}

#[test]
fn parity_load_reads_the_three_files() {
    let dir = tempfile::tempdir().unwrap();
    let l = layout(dir.path());
    std::fs::create_dir_all(dir.path().join("inbox")).unwrap();
    let t = parse_iso("2026-11-21T14:00:00Z").unwrap();
    let rec = record(
        t,
        true,
        Some("dry-run: would send to tm-architect: x"),
        vec![alert("tm-a", "ctx")],
    );
    std::fs::write(
        l.shadow_log(),
        format!("{}\n", serde_json::to_string(&rec).unwrap()),
    )
    .unwrap();
    // The live rows, in the poller's own formats; one row has no timestamp.
    std::fs::write(
        l.alerts(),
        "{\"ts\":\"2026-11-21T14:00:30Z\",\"session\":\"tm-a\",\"pane\":\"%1\",\"kind\":\"ctx\",\"fingerprint\":\"31\",\"detail\":\"d\",\"display\":\"x\"}\n\
         {\"ts\":\"2026-11-21T13:00:00Z\",\"session\":\"old\",\"pane\":\"%1\",\"kind\":\"idle\",\"fingerprint\":\"1\",\"detail\":\"d\",\"display\":\"x\"}\n\
         {\"session\":\"no-ts\"}\n",
    )
    .unwrap();
    std::fs::write(
        l.poll_log(),
        "2026-11-21T14:00:40Z panes=35 new=1 woken=1 logged=0 suppressed=31 wake=sent to %32 architect=ok\n\
         2026-11-21T14:00:50Z share_sync ok\n\
         2026-11-21T14:05:00Z panes=35 new=0 woken=0 logged=0 suppressed=31 wake=none architect=ok\n",
    )
    .unwrap();
    let rep = parity::load(&l, t - 3600).unwrap();
    assert_eq!(
        (rep.cycles, rep.alerts.matched, rep.alerts.poller_only),
        (1, 1, 0)
    );
    assert_eq!(rep.wakes.matched, 1);
    assert_eq!(rep.unread, 1);
    // `since` filters the shadow records.
    assert_eq!(parity::load(&l, t + 1).unwrap().cycles, 0);
    // No shadow file at all is an error, not an empty report.
    std::fs::remove_file(l.shadow_log()).unwrap();
    assert!(parity::load(&l, 0).is_err());
}
