//! #11826, parity window 6: one regression test per fix, each red on the
//! pre-fix commit. The rows are window 6's own (2026-11-24T23:46:17Z to
//! 2026-11-25T01:55:00Z); `tests/fixtures/parity6/inbox` holds the whole
//! window, cut from the live root with detail text dropped.
//! Fixes 3 (watchdog wait rule) and 7 (`--by-kind`) are tested beside their
//! code: `tests/watchdog_ported.rs` and `tests/sched_cli.rs`.

mod sched_support;

use std::fs;
use std::path::{Path, PathBuf};

use sched_support::{T0, layout};
use trusty_architect::sched::config::PollJob;
use trusty_architect::sched::parity::{self, alert_id};
use trusty_architect::sched::shadow::{self, EventKey, ShadowAlert, ShadowRecord, WokenKey};
use trusty_architect::sched::slot::parse_iso;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const START: &str = "2026-11-24T23:46:17Z";

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
        "{{\"ts\": \"{ts}\", \"session\": \"{}\", \"pane\": \"\", \"kind\": \"{}\", \"fingerprint\": \"{}\"}}\n",
        k.session, k.kind, k.fingerprint
    )
}

fn wake_row(ts: &str, k: &EventKey) -> String {
    let id = alert_id(&k.session, &k.kind, &k.fingerprint, ts);
    format!(
        "{{\"ts\": \"{ts}\", \"id\": \"{id}\", \"kind\": \"{}\", \"session\": \"{}\", \"decision\": \"wake\", \"path\": \"rule:always_wake\"}}\n",
        k.kind, k.session
    )
}

fn t(iso: &str) -> TestResult<i64> {
    Ok(parse_iso(iso).ok_or(iso.to_owned())?)
}

/// One shadow record per minute from `from` to `to` (inclusive); `events`
/// puts (alerts, wakes) on the record whose slot is the given time.
fn shadow_log(
    path: &Path,
    from: i64,
    to: i64,
    events: &[(i64, Vec<EventKey>, Vec<EventKey>)],
) -> TestResult {
    let mut out = String::new();
    let mut slot = from;
    while slot <= to {
        let (a, w) = events
            .iter()
            .find(|(at, ..)| *at == slot)
            .map(|(_, a, w)| (a.clone(), w.clone()))
            .unwrap_or_default();
        out += &format!("{}\n", serde_json::to_string(&record(slot, a, w))?);
        slot += 60;
    }
    fs::write(path, out)?;
    Ok(())
}

fn root() -> TestResult<(tempfile::TempDir, trusty_architect::sched::Layout)> {
    let dir = tempfile::tempdir()?;
    let l = layout(dir.path());
    fs::create_dir_all(dir.path().join("inbox"))?;
    Ok((dir, l))
}

// --- Fix 1: the shadow reseeds its token-watch state at every start ---------

/// Window 6, class A: the shadow restarted after 13 h with a token state that
/// lacked every live fire since, and replayed 30 token alerts in one cycle.
/// Red before the fix: the stale shadow token state was kept.
#[test]
fn window6_shadow_restart_reseeds_the_token_state() -> TestResult {
    let (dir, l) = root()?;
    let live = dir.path().join("inbox/token-watch-state.json");
    let fired = r#"{"fired": {"tm-bta-crm|hit_low:main:2026112420": 1795553360.0}}"#;
    fs::write(&live, fired)?;
    fs::create_dir_all(l.shadow_dir())?;
    let own = l.shadow_dir().join("token-watch-state.json");
    fs::write(&own, r#"{"fired": {"old|ctx_main:200k": 1795516415.0}}"#)?;
    shadow::plan_with_seed(&l, &PollJob::default(), vec![], T0, true)?;
    let seeded: serde_json::Value = serde_json::from_str(&fs::read_to_string(&own)?)?;
    let keys: Vec<&String> = seeded["fired"].as_object().ok_or("fired")?.keys().collect();
    assert_eq!(keys, ["tm-bta-crm|hit_low:main:2026112420"], "{seeded}");
    // A later run of the same daemon keeps what the shadow wrote.
    fs::write(&own, r#"{"fired": {"own": 1.0}}"#)?;
    shadow::plan_with_seed(&l, &PollJob::default(), vec![], T0 + 60, false)?;
    assert!(fs::read_to_string(&own)?.contains("\"own\""));
    Ok(())
}

// --- Fix 2: kinds a dry-run poll never raises ---------------------------------

/// Window 6, classes B and H: `meta_due` rows from `meta_due.py` and one
/// `factory_send_maxed` from a live auto-send were poller-only.
#[test]
fn window6_excludes_kinds_a_dry_run_poll_never_raises() -> TestResult {
    let (dir, l) = root()?;
    shadow_log(
        &l.shadow_log(),
        t("2026-11-24T23:56:00Z")?,
        t("2026-11-25T00:48:00Z")?,
        &[],
    )?;
    let mut rows = String::new();
    for id in ["M0c", "M0e", "M0g", "M0h", "M0i"] {
        let k = key(
            "architect",
            "meta_due",
            &format!("{id}@2026-11-24T23:56:38Z"),
        );
        rows += &alert_row("2026-11-24T23:56:38Z", &k);
    }
    let maxed = key("tm-dogfood", "factory_send_maxed", "tm-dogfood@20782.0");
    rows += &alert_row("2026-11-25T00:47:44Z", &maxed);
    fs::write(l.alerts(), rows)?;
    fs::write(dir.path().join("inbox/triage.jsonl"), "")?;
    let rep = parity::load(&l, t("2026-11-24T23:56:00Z")?)?;
    assert_eq!(rep.alerts.poller_only, 0, "{rep:?}");
    let text = parity::render(&rep);
    assert!(
        text.contains("never raised by a dry-run poll, excluded: 6 (meta_due 5, meta_miss 0, factory_send_maxed 1:"),
        "{text}"
    );
    Ok(())
}

// --- Fix 4: an idle episode counts once ---------------------------------------

/// Window 6, class D: tm-jira-linear-01's episode had two screens on the
/// shadow side and one live; tm-trusty-computer's had three live screens and
/// one shadow. Red before the fix: 1 scheduler-only and 2 poller-only.
#[test]
fn window6_counts_an_idle_episode_once() -> TestResult {
    let (dir, l) = root()?;
    let tc = |h: &str| {
        key(
            "tm-trusty-computer",
            "idle",
            &format!("since-1795563933-{h}"),
        )
    };
    let jl = |h: &str| {
        key(
            "tm-jira-linear-01",
            "idle",
            &format!("since-1795565712-{h}"),
        )
    };
    let events = [
        (t("2026-11-24T23:55:36Z")?, vec![tc("5d6aa0b232ef")], vec![]),
        (t("2026-11-25T00:25:36Z")?, vec![jl("5c36747b6f16")], vec![]),
        (t("2026-11-25T00:36:36Z")?, vec![jl("1310bb590cb3")], vec![]),
    ];
    let from = t("2026-11-24T23:55:36Z")?;
    shadow_log(&l.shadow_log(), from, t("2026-11-25T00:40:36Z")?, &events)?;
    let rows = alert_row("2026-11-24T23:56:33Z", &tc("5d6aa0b232ef"))
        + &alert_row("2026-11-25T00:13:59Z", &tc("cb3420588b7e"))
        + &alert_row("2026-11-25T00:15:12Z", &tc("3d13fbe09eaf"))
        + &alert_row("2026-11-25T00:26:18Z", &jl("1310bb590cb3"));
    fs::write(l.alerts(), rows)?;
    fs::write(dir.path().join("inbox/triage.jsonl"), "")?;
    let rep = parity::load(&l, from)?;
    let a = rep.alerts;
    assert_eq!(
        (a.matched, a.scheduler_only, a.poller_only),
        (2, 0, 0),
        "{rep:?}"
    );
    Ok(())
}

// --- Fix 5: session_exited without the first-seen time --------------------------

/// Window 6, class E: one exit, `S19@23:59:44Z` in the shadow and
/// `S19@23:59:01Z` live. Red before the fix: one mismatch on each side.
#[test]
fn window6_pairs_session_exited_without_first_seen_time() -> TestResult {
    let (dir, l) = root()?;
    let shadow_key = key("", "session_exited", "S19@2026-11-24T23:59:44Z");
    let live_key = key("", "session_exited", "S19@2026-11-24T23:59:01Z");
    let at = t("2026-11-24T23:59:44Z")?;
    let events = [(at, vec![shadow_key.clone()], vec![shadow_key])];
    let from = t("2026-11-24T23:57:44Z")?;
    shadow_log(&l.shadow_log(), from, t("2026-11-25T00:02:44Z")?, &events)?;
    let ts = "2026-11-24T23:59:01Z";
    fs::write(l.alerts(), alert_row(ts, &live_key))?;
    fs::write(
        dir.path().join("inbox/triage.jsonl"),
        wake_row(ts, &live_key),
    )?;
    let rep = parity::load(&l, from)?;
    assert_eq!((rep.alerts.matched, rep.wakes.matched), (1, 1), "{rep:?}");
    assert_eq!(rep.alerts.scheduler_only + rep.alerts.poller_only, 0);
    // Identity alone pairs it: the suffix is not part of the event.
    assert_eq!(rep.alerts_identity.matched, 1);
    Ok(())
}

// --- Fix 6: a token event under the Python fallback label -----------------------

/// Window 6, class F: `ctx_main:350k` of transcript accded0b, labelled
/// `tm-ai-power-rankings` by Rust and `trusty-mpm-projects-bobm:accded0b` by
/// the Python scan after the session exited. Red before the fix.
#[test]
fn window6_pairs_a_token_event_under_the_python_fallback_label() -> TestResult {
    let (dir, l) = root()?;
    let shadow_key = key("tm-ai-power-rankings", "token", "ctx_main:350k");
    let live_key = key(
        "trusty-mpm-projects-bobm:accded0b",
        "token",
        "ctx_main:350k",
    );
    let at = t("2026-11-24T23:58:42Z")?;
    let events = [(at, vec![shadow_key.clone()], vec![shadow_key])];
    let from = t("2026-11-24T23:56:42Z")?;
    shadow_log(&l.shadow_log(), from, t("2026-11-25T00:01:42Z")?, &events)?;
    let ts = "2026-11-24T23:59:01Z";
    fs::write(l.alerts(), alert_row(ts, &live_key))?;
    fs::write(
        dir.path().join("inbox/triage.jsonl"),
        wake_row(ts, &live_key),
    )?;
    let rep = parity::load(&l, from)?;
    assert_eq!((rep.alerts.matched, rep.wakes.matched), (1, 1), "{rep:?}");
    assert_eq!(rep.alert_pairs, (0, 1));
    // Another session's fingerprint under a named session never pairs.
    let other = key("tm-other", "token", "ctx_main:350k");
    fs::write(l.alerts(), alert_row(ts, &other))?;
    fs::write(dir.path().join("inbox/triage.jsonl"), "")?;
    assert_eq!(parity::load(&l, from)?.alerts.matched, 0);
    Ok(())
}

// --- The whole window -------------------------------------------------------------

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/parity6")
}

/// The window-6 rows: 82/40/13 and 47/6/2 before the parity-side fixes
/// (2, 4, 5, 6). After them the token reseed (1) and the watchdog rule (3)
/// remain, which only a new window can show.
#[test]
fn window6_fixture_moves_toward_the_projection() -> TestResult {
    let l = layout(&fixture());
    let rep = parity::load(&l, t(START)?)?;
    assert_eq!((rep.cycles, rep.failed_cycles), (122, 1));
    let c = |x: parity::Counts| (x.matched, x.scheduler_only, x.poller_only);
    assert_eq!(c(rep.alerts), (83, 37, 3), "{}", parity::render(&rep));
    assert_eq!(c(rep.wakes), (49, 4, 0), "{}", parity::render(&rep));
    Ok(())
}
