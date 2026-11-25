//! MB1595 follow-up rulings: parity leaves Haiku-path wakes out, the shadow
//! seeds its live-queue offsets at the current end, reads `events.jsonl` and
//! `pm-messages.jsonl` from the live inbox without touching them, and a read
//! error keeps the offset. Every file is in a tempdir.

mod cycle_support;
mod sched_support;

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use cycle_support::{Env, QuietHost, TestResult, fleet, screen};
use sched_support::{T0, layout};
use serde_json::{Map, Value};
use trusty_architect::poller::PollSession;
use trusty_architect::poller::pm_intake::pm_intake_alerts;
use trusty_architect::sched::config::PollJob;
use trusty_architect::sched::parity::{self, compute_with};
use trusty_architect::sched::shadow::{self, ShadowAlert, ShadowRecord};
use trusty_architect::{Clock, FakeClock};

fn record(slot: i64, woken: u64, haiku: u64) -> ShadowRecord {
    ShadowRecord {
        ts: "x".into(),
        job: "poll".into(),
        slot,
        mode: "shadow".into(),
        interval_s: 60,
        ok: true,
        panes: Some(1),
        new: Some(woken),
        woken: Some(woken),
        logged: Some(0),
        suppressed: Some(0),
        wake_phrase: None,
        wake_target: None,
        alerts: Vec::<ShadowAlert>::new(),
        digest_sha: "d".into(),
        error: None,
        haiku_woken: Some(haiku),
        alert_keys: None,
        woken_keys: None,
    }
}

/// Ruling (a): a cycle whose wakes are all Haiku-path is no wake, on either
/// side; the count is printed on its own line.
#[test]
fn parity_excludes_haiku_path_wakes_from_the_verdict() {
    let t = T0;
    // t: shadow woke 2, both Haiku; live woke one via Haiku. t+60: a rule wake on both.
    let recs = [record(t, 2, 2), record(t + 60, 1, 0)];
    let rep = compute_with(&recs, &[], &[t + 65], &[t + 5]);
    assert_eq!(
        (
            rep.wakes.matched,
            rep.wakes.scheduler_only,
            rep.wakes.poller_only
        ),
        (1, 0, 0)
    );
    assert_eq!(rep.haiku_excluded, 3);
    let text = parity::render(&rep);
    assert!(text.contains("haiku-path wakes excluded: 3\n"), "{text}");
    assert!(text.contains("wake match: 100.0%"), "{text}");
}

/// Ruling (a): the loader splits live wakes by `triage.jsonl`'s `path`.
#[test]
fn parity_load_reads_live_wakes_from_triage_jsonl() -> TestResult {
    let dir = tempfile::tempdir()?;
    let l = layout(dir.path());
    fs::create_dir_all(dir.path().join("inbox"))?;
    let line = serde_json::to_string(&record(T0, 1, 0))?;
    fs::write(l.shadow_log(), format!("{line}\n"))?;
    let ts = |s: i64| {
        let d = trusty_architect::classify::alert("x", "f", "d", "", "", s as f64).ts;
        format!(
            "{{\"ts\": \"{d}\", \"decision\": \"wake\", \"path\": \"haiku\"}}\n{{\"ts\": \"{d}\", \"decision\": \"wake\", \"path\": \"rule:always_wake\"}}\n"
        )
    };
    fs::write(dir.path().join("inbox/triage.jsonl"), ts(T0 + 5))?;
    let rep = parity::load(&l, T0 - 10)?;
    assert_eq!((rep.wakes.matched, rep.haiku_excluded), (1, 1));
    Ok(())
}

/// Ruling (b)/(c): the first shadow plan seeds `pm_offset` and
/// `events_offset` at the live files' current end, once.
#[test]
fn shadow_plan_seeds_live_offsets_at_the_current_end_once() -> TestResult {
    let dir = tempfile::tempdir()?;
    let l = layout(dir.path());
    let inbox = dir.path().join("inbox");
    fs::create_dir_all(&inbox)?;
    fs::write(
        inbox.join("poll-state.json"),
        r#"{"pm_offset": 3, "events_offset": 4}"#,
    )?;
    fs::write(inbox.join("pm-messages.jsonl"), "{\"kind\": \"done\"}\n")?;
    fs::write(inbox.join("events.jsonl"), "{}\n{}\n")?;
    shadow::plan(&l, &PollJob::default(), Vec::new(), T0)?;
    let state = l.shadow_dir().join(shadow::DRY_RUN_STATE);
    let read = |p: &Path| -> TestResult<Map<String, Value>> {
        Ok(serde_json::from_str(&fs::read_to_string(p)?)?)
    };
    let doc = read(&state)?;
    assert_eq!(
        (doc["pm_offset"].as_u64(), doc["events_offset"].as_u64()),
        (Some(17), Some(6))
    );
    // Later growth is read by the dry run, not reseeded by the next plan.
    fs::write(
        inbox.join("pm-messages.jsonl"),
        "{\"kind\": \"done\"}\n{\"kind\": \"x\"}\n",
    )?;
    shadow::plan(&l, &PollJob::default(), Vec::new(), T0 + 60)?;
    assert_eq!(read(&state)?["pm_offset"].as_u64(), Some(17));
    Ok(())
}

fn fingerprint(p: &Path) -> TestResult<(Vec<u8>, u64, i64, i64)> {
    let m = fs::metadata(p)?;
    Ok((fs::read(p)?, m.ino(), m.mtime(), m.mtime_nsec()))
}

/// Ruling (c): a shadow cycle reads the live `events.jsonl` and
/// `pm-messages.jsonl`, keeps its offsets in its own state, and leaves both
/// live files byte-identical (same inode, same mtime: no write, truncate or
/// rename).
#[test]
fn shadow_cycle_reads_live_queues_and_never_writes_them() -> TestResult {
    let live = tempfile::tempdir()?;
    let events = live.path().join("events.jsonl");
    let pm = live.path().join("pm-messages.jsonl");
    fs::write(&events, "")?;
    fs::write(&pm, "")?;
    let scratch = tempfile::tempdir()?;
    let state = scratch.path().join("dry-run-state.json");
    fs::write(&state, r#"{"events_offset": 0, "pm_offset": 0}"#)?;
    let live_dir = live.path().display().to_string();
    let state_s = state.display().to_string();
    let env = Env::new(&[
        ("ARCHITECT_READ_INBOX_DIR", &live_dir),
        ("ARCHITECT_DRY_RUN_STATE", &state_s),
    ])?;
    fs::write(
        &events,
        "{\"type\": \"permission_prompt\", \"session\": \"tm-a\", \"message\": \"Allow?\"}\n",
    )?;
    fs::write(
        &pm,
        "{\"session\": \"tm-b\", \"kind\": \"done\", \"text\": \"merged\"}\n",
    )?;
    let before = (fingerprint(&events)?, fingerprint(&pm)?);
    let tmux = fleet(&[("tm-a", "%2", "claude")], &screen(10));
    let clock = FakeClock::new(1.8e9);
    let mut runner = env.runner(&tmux, &clock, &QuietHost);
    runner.dry_run = true;
    let (mut session, _) = PollSession::load(&env.config, true, clock.now_unix());
    let summary = runner.run_once(&mut session)?;
    let kinds: Vec<&str> = summary.new.iter().map(|a| a.kind.as_str()).collect();
    assert!(
        kinds.contains(&"permission_prompt") && kinds.contains(&"pm_message"),
        "{kinds:?}"
    );
    assert_eq!((fingerprint(&events)?, fingerprint(&pm)?), before);
    let doc: Map<String, Value> = serde_json::from_str(&fs::read_to_string(&state)?)?;
    let size = |p: &Path| fs::metadata(p).map(|m| m.len()).ok();
    assert_eq!(doc["events_offset"].as_u64(), size(&events));
    assert_eq!(doc["pm_offset"].as_u64(), size(&pm));
    Ok(())
}

/// Critic fail-open: an existing queue that cannot be read (here, a
/// directory) is no alerts and keeps `pm_offset`; before the fix it read as
/// empty and reset the offset to 0, so the next good read replayed the queue.
#[test]
fn pm_intake_read_error_keeps_the_offset() {
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let path = dir.path().join("pm-messages.jsonl");
    fs::create_dir(&path).unwrap_or_else(|e| panic!("{e}"));
    let mut st = Map::new();
    st.insert("pm_offset".to_owned(), Value::from(42));
    let mut log = Vec::new();
    assert!(pm_intake_alerts(&mut st, &path, 1.0e9, true, &mut log).is_empty());
    assert_eq!(st["pm_offset"], Value::from(42));
}

/// The same rule for `events.jsonl`: a read error keeps the stored offset.
#[test]
fn events_read_error_keeps_the_offset() {
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let path = dir.path().join("events.jsonl");
    fs::create_dir(&path).unwrap_or_else(|e| panic!("{e}"));
    let mut off = Some(42);
    assert!(trusty_architect::poller::read_events(&path, &mut off, 1.0e9).is_err());
    assert_eq!(off, Some(42));
}

/// D64 HIGH arm 1: a live queue that is not a readable file (here, a
/// directory) is an error: no marker and no state written.
#[test]
fn shadow_plan_refuses_an_unreadable_live_queue() -> TestResult {
    let dir = tempfile::tempdir()?;
    let l = layout(dir.path());
    fs::create_dir_all(dir.path().join("inbox/pm-messages.jsonl"))?;
    fs::create_dir_all(l.shadow_dir())?;
    let state = l.shadow_dir().join(shadow::DRY_RUN_STATE);
    fs::write(&state, r#"{"pm_offset": 7}"#)?;
    assert!(shadow::plan(&l, &PollJob::default(), Vec::new(), T0).is_err());
    assert_eq!(fs::read_to_string(&state)?, r#"{"pm_offset": 7}"#);
    Ok(())
}

/// D64 HIGH arm 2: a corrupt dry-run state is an error and is left as it is,
/// never read as empty and rewritten.
#[test]
fn shadow_plan_leaves_a_corrupt_dry_run_state_untouched() -> TestResult {
    let dir = tempfile::tempdir()?;
    let l = layout(dir.path());
    fs::create_dir_all(dir.path().join("inbox"))?;
    fs::create_dir_all(l.shadow_dir())?;
    let state = l.shadow_dir().join(shadow::DRY_RUN_STATE);
    fs::write(&state, b"{not json")?;
    assert!(shadow::plan(&l, &PollJob::default(), Vec::new(), T0).is_err());
    assert_eq!(fs::read(&state)?, b"{not json");
    Ok(())
}
