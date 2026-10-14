//! #9801 slice 1: the prototype's `scripts/test_usage.py`, ported by name.
//!
//! Each `// Python: <name>` test is the prototype test of that name, run
//! against the Rust port with the prototype's own inputs. The list it is held
//! to, and the tests left out with the reason, are in
//! `tests/fixtures/9801/python_tests_9801.txt`.

use std::fs;
use std::path::Path;

use serde_json::{Map, Value, json};
use trusty_architect::classify::{DETAIL_MAX, alert};
use trusty_architect::poller::steps::usage::{
    Pct, ProbeLauncher, Reading, RecordingProbe, UsagePaths, UtcTime, Window, append_ledger,
    burn_rate, effective, plan, poll_step, project, rows_from_values, runway, sample_cache,
    UsageError, usage_alerts, usage_step,
};
use trusty_architect::{Clock, SystemClock};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const GM: &str = "a2@example.com";
const MASA: &str = "a3@example.com";
const DUETTO: &str = "a5@example.com";

fn t(s: &str) -> TestResult<UtcTime> {
    Ok(UtcTime::parse_iso(s)?)
}

fn row(ts: &str, acct: &str, p5: Value, r5: Value, pw: Value, rw: Value) -> Value {
    json!({"ts": ts, "account": acct, "pct5h": p5, "reset5h": r5, "pctwk": pw, "resetwk": rw,
           "weekly_models": {}, "source": "manual"})
}

/// test_usage.py's `fleet()`: the 16:1xZ readings of 2026-10-07 plus masa's 13:09Z.
fn fleet(extra: Vec<Value>) -> Vec<Value> {
    let mut rows = vec![
        row(
            "2026-10-07T13:09:00Z",
            MASA,
            json!(3),
            json!("2026-10-07T17:59:00Z"),
            json!(1),
            json!("2026-10-13T10:59:00Z"),
        ),
        row(
            "2026-10-07T16:15:00Z",
            MASA,
            json!(100),
            json!("2026-10-07T18:00:00Z"),
            json!(27),
            json!("2026-10-13T11:00:00Z"),
        ),
        row(
            "2026-10-07T16:12:00Z",
            GM,
            json!(9),
            json!("2026-10-07T20:50:00Z"),
            json!(84),
            json!("2026-10-09T07:00:00Z"),
        ),
        row(
            "2026-10-07T16:17:00Z",
            "a4@example.com",
            json!(0),
            Value::Null,
            json!(89),
            json!("2026-10-12T15:00:00Z"),
        ),
        row(
            "2026-10-07T16:17:00Z",
            "a1@example.com",
            json!(0),
            Value::Null,
            json!(93),
            json!("2026-10-10T15:00:00Z"),
        ),
        row(
            "2026-10-07T16:16:00Z",
            "a6@example.com",
            json!(0),
            Value::Null,
            json!(96),
            json!("2026-10-11T09:00:00Z"),
        ),
        row(
            "2026-10-07T16:18:00Z",
            DUETTO,
            json!(0),
            Value::Null,
            json!(50),
            json!("2026-10-12T11:00:00Z"),
        ),
    ];
    rows.extend(extra);
    rows
}

fn rows(values: Vec<Value>) -> TestResult<Vec<Reading>> {
    Ok(rows_from_values(values)?)
}

fn gm_6030() -> Value {
    row(
        "2026-10-07T14:30:00Z",
        GM,
        json!(1),
        json!("2026-10-07T20:50:00Z"),
        json!(60),
        json!("2026-10-09T07:00:00Z"),
    )
}

// Python: test_projection_math_masa_pair
#[test]
fn test_projection_math_masa_pair() -> TestResult {
    let masa: Vec<Value> = fleet(vec![])
        .into_iter()
        .filter(|r| r["account"] == MASA)
        .collect();
    let p = project(&rows(masa)?, MASA, t("2026-10-07T16:20:00Z")?, Window::Week)?
        .ok_or("no projection")?;
    // 1% at 13:09 -> 27% at 16:15 = 26 points in 3.1 h; 73 left at that rate
    let want = 26.0 / (186.0 / 60.0);
    assert!((p.rate - want).abs() <= want * 1e-6, "{}", p.rate);
    assert_eq!(p.at, t("2026-10-07T16:15:00Z")?.plus_hours(73.0 / p.rate)?);
    assert!(p.before_reset);
    assert_eq!(p.pct, 27.0);
    Ok(())
}

// Python: test_no_rate_from_one_sample_or_falling_or_gap
#[test]
fn test_no_rate_from_one_sample_or_falling_or_gap() -> TestResult {
    let now = t("2026-10-08T00:00:00Z")?;
    let one = row(
        "2026-10-07T16:12:00Z",
        "a",
        json!(9),
        Value::Null,
        json!(84),
        json!("2026-10-09T07:00:00Z"),
    );
    assert!(burn_rate(&rows(vec![one.clone()])?, "a", now, Window::Week)?.is_none());
    let far = row(
        "2026-10-07T23:50:00Z",
        "a",
        json!(9),
        Value::Null,
        json!(90),
        json!("2026-10-09T07:00:00Z"),
    ); // 7.6 h gap
    assert!(burn_rate(&rows(vec![one.clone(), far])?, "a", now, Window::Week)?.is_none());
    let other = row(
        "2026-10-07T16:40:00Z",
        "a",
        json!(9),
        Value::Null,
        json!(90),
        json!("2026-10-16T07:00:00Z"),
    );
    assert!(burn_rate(&rows(vec![one, other])?, "a", now, Window::Week)?.is_none());
    Ok(())
}

// Python: test_reset_passed_counts_as_zero_and_is_flagged_inferred
#[test]
fn test_reset_passed_counts_as_zero_and_is_flagged_inferred() -> TestResult {
    let rec = rows(vec![row(
        "2026-10-06T18:58:00Z",
        "d",
        json!(91),
        json!("2026-10-06T21:00:00Z"),
        json!(25),
        json!("2026-10-12T11:00:00Z"),
    )])?;
    let e = effective(&rec[0], t("2026-10-07T16:00:00Z")?)?;
    assert_eq!(
        (e.pct5h, e.inferred5h, e.pctwk, e.inferredwk),
        (Some(Pct::Int(0)), true, Some(Pct::Int(25)), false)
    );
    let e2 = effective(&rec[0], t("2026-10-12T12:00:00Z")?)?;
    assert_eq!((e2.pctwk, e2.inferredwk), (Some(Pct::Int(0)), true));
    let started = rows(vec![row(
        "2026-10-07T16:17:00Z",
        "r",
        json!(0),
        Value::Null,
        json!(89),
        json!("2026-10-12T15:00:00Z"),
    )])?;
    let started = effective(&started[0], t("2026-10-07T16:30:00Z")?)?;
    // window not started: 0, not inferred
    assert_eq!(started.pct5h, Some(Pct::Int(0)));
    assert!(!started.inferred5h);
    Ok(())
}

// Python: test_plan_names_best_next_account_and_skips_exhausted_5h
#[test]
fn test_plan_names_best_next_account_and_skips_exhausted_5h() -> TestResult {
    let now = t("2026-10-07T16:20:00Z")?;
    let p = plan(&rows(fleet(vec![gm_6030()]))?, now, Some(GM))?; // 60% -> 84% in 1.7 h
    assert!(p.need);
    assert_eq!(p.next.as_ref().map(|(a, _)| a.as_str()), Some(DUETTO)); // lowest weekly (50%)
    assert!(
        p.skipped
            .contains(&(MASA.to_owned(), "5h window at 100%".to_owned()))
    );
    let alts: Vec<&str> = p.alternatives.iter().map(|(a, _)| a.as_str()).collect();
    // 93/96 tie within 5 points: later reset first
    assert_eq!(alts, ["a4@example.com", "a6@example.com", "a1@example.com"]);
    // #9801: format_plan (the `plan` CLI's text) is not ported; see python_tests_9801.txt.
    Ok(())
}

// Python: test_plan_no_cutover_when_healthy_and_limit_reached_means_now
#[test]
fn test_plan_no_cutover_when_healthy_and_limit_reached_means_now() -> TestResult {
    let now = t("2026-10-07T16:20:00Z")?;
    let healthy: Vec<Value> = fleet(vec![])
        .into_iter()
        .filter(|r| r["account"] != GM)
        .collect();
    assert!(!plan(&rows(healthy)?, now, Some(DUETTO))?.need);
    let p = plan(&rows(fleet(vec![]))?, now, Some(MASA))?;
    assert!(p.need);
    assert_eq!(p.switch_at, Some(now));
    assert!(
        p.triggers[0].1.contains("5-hour limit reached"),
        "{:?}",
        p.triggers
    );
    Ok(())
}

// Python: test_runway_capacity_hours_and_gap
#[test]
fn test_runway_capacity_hours_and_gap() -> TestResult {
    let now = t("2026-10-07T16:20:00Z")?;
    let rw = runway(&rows(fleet(vec![]))?, now)?;
    let close = |a: f64, b: f64| (a - b).abs() <= b.abs() * 1e-6;
    // masa, gmail, r, support, bob, duetto
    assert!(
        close(rw.left, 73.0 + 16.0 + 11.0 + 4.0 + 7.0 + 50.0),
        "{}",
        rw.left
    );
    assert_eq!(rw.rate_account.as_deref(), Some(MASA));
    let rate = rw.rate.ok_or("no rate")?;
    assert!(close(rw.runway_h.ok_or("no runway")?, rw.left / rate));
    assert_eq!(rw.first_reset.as_ref().map(|(_, a)| a.as_str()), Some(GM));
    let gap = t("2026-10-09T07:00:00Z")?.seconds_since(now) / 3600.0;
    assert!(close(rw.gap_h.ok_or("no gap")?, gap));
    assert_eq!(rw.covers_gap, Some(false));
    Ok(())
}

// Python: test_alerts_week_thresholds_and_5h_exhaustion_name_next_account
#[test]
fn test_alerts_week_thresholds_and_5h_exhaustion_name_next_account() -> TestResult {
    let out: Map<String, Value> = usage_alerts(
        &rows(fleet(vec![gm_6030()]))?,
        t("2026-10-07T16:20:00Z")?,
        GM,
    )?
    .into_iter()
    .map(|(k, d)| (k, Value::String(d)))
    .collect();
    let wk: Vec<&Value> = out
        .iter()
        .filter(|(k, _)| k.starts_with("wk>=80"))
        .map(|(_, d)| d)
        .collect();
    assert!(!wk.is_empty() && wk[0].as_str().unwrap_or_default().contains(DUETTO));
    assert!(!out.keys().any(|k| k.starts_with("wk>=90")));
    assert!(out.keys().any(|k| k.starts_with("cutover:")));
    let exhaust = fleet(vec![
        row(
            "2026-10-07T15:40:00Z",
            GM,
            json!(30),
            json!("2026-10-07T20:50:00Z"),
            json!(84),
            json!("2026-10-09T07:00:00Z"),
        ),
        row(
            "2026-10-07T16:20:00Z",
            GM,
            json!(70),
            json!("2026-10-07T20:50:00Z"),
            json!(84),
            json!("2026-10-09T07:00:00Z"),
        ),
    ]);
    let out = usage_alerts(&rows(exhaust)?, t("2026-10-07T16:21:00Z")?, GM)?;
    let exh: Vec<&String> = out
        .iter()
        .filter(|(k, _)| k.starts_with("5h-exhaust"))
        .map(|(_, d)| d)
        .collect();
    // 60 %/h from 70%: 30 min
    assert!(
        !exh.is_empty() && exh[0].contains("projected to hit 100% at 2026-10-07T16:"),
        "{exh:?}"
    );
    let far = fleet(vec![
        row(
            "2026-10-07T14:00:00Z",
            GM,
            json!(1),
            json!("2026-10-07T20:50:00Z"),
            json!(84),
            json!("2026-10-09T07:00:00Z"),
        ),
        row(
            "2026-10-07T16:20:00Z",
            GM,
            json!(10),
            json!("2026-10-07T20:50:00Z"),
            json!(84),
            json!("2026-10-09T07:00:00Z"),
        ),
    ]);
    let out = usage_alerts(&rows(far)?, t("2026-10-07T16:21:00Z")?, GM)?;
    assert!(!out.iter().any(|(k, _)| k.starts_with("5h-")));
    Ok(())
}

// Python: test_alert_detail_fits_poller_cap
#[test]
fn test_alert_detail_fits_poller_cap() -> TestResult {
    for (k, d) in usage_alerts(
        &rows(fleet(vec![gm_6030()]))?,
        t("2026-10-07T16:20:00Z")?,
        GM,
    )? {
        let head: String = d.chars().take(DETAIL_MAX).collect();
        assert!(head.contains(DUETTO), "{k}");
        // The poller's own alert() keeps the same head.
        assert!(
            alert("usage", &k, &d, "", "", 0.0).detail.contains(DUETTO),
            "{k}"
        );
    }
    Ok(())
}

/// test_usage.py's `_env`: every usage path under `dir`, the account in claude.json.
fn env(dir: &Path, account: &str) -> TestResult<UsagePaths> {
    fs::create_dir_all(dir.join("inbox"))?;
    let paths = UsagePaths {
        ledger: dir.join("usage").join("ledger.jsonl"),
        cache: dir.join("inbox").join("statusline-usage.json"),
        claude_json: dir.join("claude.json"),
        probe_script: dir.join("usage-probe.sh"),
        probe_log: dir.join("inbox").join("usage-probe.log"),
    };
    fs::write(
        &paths.claude_json,
        json!({"oauthAccount": {"emailAddress": account}}).to_string(),
    )?;
    Ok(paths)
}

/// test_usage.py's `_cache`.
fn cache(paths: &UsagePaths, account: &str) -> TestResult {
    let payload = json!({"rate_limits": {"five_hour": {"used_percentage": 12.5, "resets_at": 1_791_400_000},
                                         "seven_day": {"used_percentage": 84, "resets_at": 1_791_500_000}}});
    fs::write(&paths.cache, format!("{account}\n{payload}\n"))?;
    Ok(())
}

fn wall_now() -> TestResult<UtcTime> {
    Ok(UtcTime::from_unix(SystemClock.now_unix())?)
}

// Python: test_sample_cache_records_matching_account_only
#[test]
fn test_sample_cache_records_matching_account_only() -> TestResult {
    let dir = tempfile::tempdir()?;
    let paths = env(dir.path(), GM)?;
    let now = wall_now()?;
    cache(&paths, "a1@example.com")?;
    assert!(sample_cache(&paths, now, None)?.is_none()); // cache written under another account
    cache(&paths, GM)?;
    let rec = sample_cache(&paths, now, None)?.ok_or("no sample")?;
    assert_eq!(rec.source, "statusline-cache");
    assert_eq!(rec.pctwk, json!(84));
    assert_eq!(
        rec.resetwk,
        Some(UtcTime::from_unix(1_791_500_000.0)?.iso())
    );
    assert!(sample_cache(&paths, now, None)?.is_none()); // unchanged reading is not re-recorded
    Ok(())
}

// Python: test_cache_rejected_when_windows_equal_previous_accounts
#[test]
fn test_cache_rejected_when_windows_equal_previous_accounts() -> TestResult {
    let dir = tempfile::tempdir()?;
    let paths = env(dir.path(), GM)?;
    let now = wall_now()?;
    let old = UtcTime::from_unix(1_791_500_000.0)?.iso();
    let five = UtcTime::from_unix(1_791_400_000.0)?.iso();
    let hour_ago = UtcTime::from_unix(now.timestamp() - 3600.0)?.iso();
    append_ledger(
        &paths.ledger,
        &row(
            &hour_ago,
            MASA,
            json!(5),
            json!(five),
            json!(27),
            json!(old),
        ),
    )?;
    cache(&paths, GM)?; // stale carry-over right after /login
    assert!(sample_cache(&paths, now, None)?.is_none());
    Ok(())
}

// Python: test_poll_step_probes_on_account_change_and_failure_cannot_break_cycle
#[test]
fn test_poll_step_probes_on_account_change_and_failure_cannot_break_cycle() -> TestResult {
    let dir = tempfile::tempdir()?;
    let paths = env(dir.path(), GM)?;
    let probe = RecordingProbe::default();
    let mut state = json!({"usage": {"account": MASA, "since": 0}})
        .as_object()
        .cloned()
        .unwrap_or_default();
    let now = 1_791_380_000.0;
    let idle = [("tm-x".to_owned(), "%1".to_owned())];
    poll_step(&paths, &probe, &mut state, now, &idle, false)?;
    assert_eq!(probe.launched(), [["tm-x", "%1", GM].map(str::to_owned)]);
    assert_eq!(state["usage"]["pending"], json!(true));
    poll_step(&paths, &probe, &mut state, now + 600.0, &idle, false)?;
    assert_eq!(probe.launched().len(), 2); // still pending: retry (max 3)
    poll_step(&paths, &probe, &mut state, now + 700.0, &[], false)?;
    assert_eq!(probe.launched().len(), 2); // no idle pane: never types

    // The prototype injects a raising sampler; here the ledger is not UTF-8.
    fs::create_dir_all(paths.ledger.parent().ok_or("ledger dir")?)?;
    fs::write(&paths.ledger, b"\xff\xfe\n")?;
    let (alerts, log) = usage_step(
        &paths,
        &probe as &dyn ProbeLauncher,
        &mut Map::new(),
        0.0,
        &[],
        false,
    );
    assert!(alerts.is_empty());
    assert!(
        log.iter().any(|l| l.starts_with("usage sampler ERROR ")),
        "{log:?}"
    );
    Ok(())
}

// Python: test_poll_step_probes_hourly_not_more
#[test]
fn test_poll_step_probes_hourly_not_more() -> TestResult {
    let dir = tempfile::tempdir()?;
    let paths = env(dir.path(), GM)?;
    let probe = RecordingProbe::default();
    let mut state = Map::new();
    let now = 1_791_380_000.0;
    let idle = [("tm-x".to_owned(), "%1".to_owned())];
    for dt in [0.0, 600.0, 1200.0, 3700.0] {
        poll_step(&paths, &probe, &mut state, now + dt, &idle, false)?;
    }
    assert_eq!(probe.launched().len(), 2); // at t=0 and t>=3600
    Ok(())
}

/// #9801: `from_unix` multiplied the seconds by 1e6 unchecked, which panics in
/// a debug build for a time above about 9.2e12 s. Every such time is outside
/// years 1-9999, so it is `OutOfRange`.
#[test]
fn from_unix_beyond_the_i64_microsecond_range_is_out_of_range_not_a_panic() {
    for t in [1e13, -1e13, 9.3e12, -9.3e12, 5e14, 1e15, f64::MAX] {
        assert!(
            matches!(UtcTime::from_unix(t), Err(UsageError::OutOfRange)),
            "{t}"
        );
    }
}
