//! MB1595 fix round: the parity join pairs by event identity first, then by a
//! two-cycle tolerance, prints both numbers, and leaves `il_cron` out. New
//! tests: the base report had no identity numbers and no exclusion line.

mod sched_support;

use std::fs;

use sched_support::layout;
use trusty_architect::sched::parity::{self, alert_id};
use trusty_architect::sched::shadow::{EventKey, ShadowAlert, ShadowRecord, WokenKey};
use trusty_architect::sched::slot::parse_iso;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn key(s: &str, k: &str, f: &str) -> EventKey {
    EventKey {
        session: s.to_owned(),
        kind: k.to_owned(),
        fingerprint: f.to_owned(),
    }
}

fn record(slot: i64, alerts: Vec<EventKey>, woken: Vec<EventKey>) -> ShadowRecord {
    ShadowRecord {
        ts: "x".into(),
        job: "poll".into(),
        slot,
        mode: "shadow".into(),
        interval_s: 60,
        ok: true,
        panes: Some(1),
        new: Some(alerts.len() as u64),
        woken: Some(woken.len() as u64),
        logged: Some(0),
        suppressed: Some(0),
        wake_phrase: None,
        wake_target: None,
        alerts: alerts
            .iter()
            .map(|k| ShadowAlert {
                session: k.session.clone(),
                kind: k.kind.clone(),
            })
            .collect(),
        digest_sha: "d".into(),
        error: None,
        haiku_woken: Some(0),
        alert_keys: Some(alerts),
        woken_keys: Some(
            woken
                .into_iter()
                .map(|key| WokenKey { key, haiku: false })
                .collect(),
        ),
    }
}

fn alert_row(ts: &str, k: &EventKey) -> String {
    format!(
        "{{\"ts\": \"{ts}\", \"session\": \"{}\", \"pane\": \"\", \"kind\": \"{}\", \"fingerprint\": \"{}\", \"detail\": \"d\", \"display\": \"x\"}}\n",
        k.session, k.kind, k.fingerprint
    )
}

fn wake_row(ts: &str, k: &EventKey) -> String {
    let id = alert_id(&k.session, &k.kind, &k.fingerprint, ts);
    format!(
        "{{\"ts\": \"{ts}\", \"id\": \"{id}\", \"kind\": \"{}\", \"session\": \"{}\", \"decision\": \"wake\", \"path\": \"rule:always_wake\", \"reason\": \"r\"}}\n",
        k.kind, k.session
    )
}

/// The live poller raised and woke for a PM message five cycles after the
/// shadow (phase skew past the fallback); identity pairs it. An `idle` alert
/// one cycle late with a different fingerprint pairs only by the fallback.
#[test]
fn parity_reports_identity_and_fallback_numbers() -> TestResult {
    let dir = tempfile::tempdir()?;
    let l = layout(dir.path());
    fs::create_dir_all(dir.path().join("inbox"))?;
    let t = parse_iso("2026-11-21T14:00:00Z").ok_or("ts")?;
    let pm = key("mcp:tm-a", "pm_message", "f1");
    let idle_shadow = key("tm-b", "idle", "since-1-aaa");
    let idle_live = key("tm-b", "idle", "since-1-bbb");
    let mut recs = String::new();
    for n in 0..8 {
        let rec = if n == 0 {
            record(t, vec![pm.clone(), idle_shadow.clone()], vec![pm.clone()])
        } else {
            record(t + 60 * n, vec![], vec![])
        };
        recs += &format!("{}\n", serde_json::to_string(&rec)?);
    }
    fs::write(l.shadow_log(), recs)?;
    let (late5, late1) = ("2026-11-21T14:05:00Z", "2026-11-21T14:01:10Z");
    // Golden from the deployed `alert_triage.alert_id` (Python hashlib.sha1).
    assert_eq!(
        alert_id("mcp:tm-a", "pm_message", "f1", late5),
        "9cca3e9e6e"
    );
    fs::write(
        l.alerts(),
        alert_row(late5, &pm) + &alert_row(late1, &idle_live),
    )?;
    fs::write(dir.path().join("inbox/triage.jsonl"), wake_row(late5, &pm))?;
    let rep = parity::load(&l, t - 10)?;
    // The idle pair has two keyed events with different fingerprints: the
    // fallback no longer pairs them, so it is one scheduler-only and one
    // poller-only mismatch (it used to count as a fallback match).
    assert_eq!((rep.alerts.matched, rep.alert_pairs), (1, (1, 0)));
    assert_eq!(rep.alerts_identity.matched, 1);
    assert_eq!(rep.alerts_identity.scheduler_only, 1);
    assert_eq!((rep.wakes.matched, rep.wake_pairs), (1, (1, 0)));
    let text = parity::render(&rep);
    assert!(
        text.contains("alerts (identity only): matched 1 / scheduler-only 1 / poller-only 1\n"),
        "{text}"
    );
    assert!(
        text.contains("pairs by identity / by +-2-cycle fallback: alerts 1 / 0, wakes 1 / 0\n"),
        "{text}"
    );
    assert!(
        text.contains("verdict: FAIL (identity plus fallback;"),
        "{text}"
    );
    Ok(())
}

/// `il_cron` never reaches the shadow (a dry run only prints `would run`);
/// it is counted, with its reason, and left out of the match.
#[test]
fn parity_excludes_il_cron_with_its_reason() -> TestResult {
    let dir = tempfile::tempdir()?;
    let l = layout(dir.path());
    fs::create_dir_all(dir.path().join("inbox"))?;
    let t = parse_iso("2026-11-21T14:00:00Z").ok_or("ts")?;
    let rec = record(t, vec![], vec![]);
    fs::write(
        l.shadow_log(),
        format!("{}\n", serde_json::to_string(&rec)?),
    )?;
    let il = key("", "il_cron", "job-a");
    let ts = "2026-11-21T14:00:20Z";
    fs::write(l.alerts(), alert_row(ts, &il))?;
    fs::write(dir.path().join("inbox/triage.jsonl"), wake_row(ts, &il))?;
    let rep = parity::load(&l, t - 10)?;
    assert_eq!(rep.il_cron_excluded, 2);
    assert_eq!((rep.alerts.poller_only, rep.wakes.poller_only), (0, 0));
    let text = parity::render(&rep);
    assert!(
        text.contains("il_cron excluded: 2 (unmeasurable in dry run: il-cron.py:249)\n"),
        "{text}"
    );
    Ok(())
}

/// MB1595 window 4: the two pollers keep separate idle timers, so the same
/// idle screen carries `since-<epoch>-<hash>` with epochs 98 s apart. Red
/// before the fix: no identity, and the fallback refuses two keyed events.
#[test]
fn parity_pairs_idle_alerts_whose_epochs_differ() -> TestResult {
    let dir = tempfile::tempdir()?;
    let l = layout(dir.path());
    fs::create_dir_all(dir.path().join("inbox"))?;
    let t = parse_iso("2026-11-23T02:08:12Z").ok_or("ts")?;
    let shadow = key("tm-a", "idle", "since-1795399071-8e42f0ed439d");
    let rec = record(t, vec![shadow.clone()], vec![shadow]);
    fs::write(
        l.shadow_log(),
        format!("{}\n", serde_json::to_string(&rec)?),
    )?;
    // The live mate, 131 s later: beyond the two-cycle fallback.
    let live = key("tm-a", "idle", "since-1795399169-8e42f0ed439d");
    let ts = "2026-11-23T02:10:23Z";
    fs::write(l.alerts(), alert_row(ts, &live))?;
    fs::write(dir.path().join("inbox/triage.jsonl"), wake_row(ts, &live))?;
    let rep = parity::load(&l, t - 10)?;
    assert_eq!((rep.alerts.matched, rep.wakes.matched), (1, 1), "{rep:?}");
    assert_eq!(rep.idle_relaxed, (1, 1));
    let text = parity::render(&rep);
    assert!(
        text.contains(
            "idle pairs on (session, kind, screen hash), epochs within 300 s: alerts 1, wakes 1\n"
        ),
        "{text}"
    );
    assert!(text.contains("verdict: "), "{text}");
    Ok(())
}

/// Both pollers debounce ctxwatch on one shared state file, so the one that
/// runs first raises it: counted with its reason and left out of the match.
#[test]
fn parity_excludes_ctxwatch_with_its_reason() -> TestResult {
    let dir = tempfile::tempdir()?;
    let l = layout(dir.path());
    fs::create_dir_all(dir.path().join("inbox"))?;
    let t = parse_iso("2026-11-23T02:19:00Z").ok_or("ts")?;
    let shadow = key("tm-b", "ctxwatch", "S1");
    let rec = record(t, vec![shadow], vec![]);
    fs::write(
        l.shadow_log(),
        format!("{}\n", serde_json::to_string(&rec)?),
    )?;
    let live = key("tm-c", "ctxwatch", "S2a");
    let ts = "2026-11-23T02:19:22Z";
    fs::write(l.alerts(), alert_row(ts, &live))?;
    fs::write(dir.path().join("inbox/triage.jsonl"), wake_row(ts, &live))?;
    let rep = parity::load(&l, t - 10)?;
    assert_eq!(rep.ctxwatch_excluded, 3);
    assert_eq!((rep.alerts.scheduler_only, rep.alerts.poller_only), (0, 0));
    assert_eq!(rep.wakes.poller_only, 0);
    let text = parity::render(&rep);
    assert!(
        text.contains(
            "ctxwatch excluded: 3 (shared debounce state: the first poller to run raises it)\n"
        ),
        "{text}"
    );
    Ok(())
}
