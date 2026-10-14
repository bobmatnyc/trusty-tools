//! #9801 slice 1 parity: the usage step and `notify_macos` against goldens
//! written by the prototype's Python (`tests/fixtures/9801/regen_9801.py`).
//!
//! What: reads `corpus_9801.json`, runs every case through the Rust port,
//! renders each result in the shape `regen_9801.py` renders the Python one,
//! and compares each golden line byte for byte. A golden `{"raises": ...}`
//! row requires a Rust error (the Python exception class is not compared).
//! `rulings.jsonl` pins the set of cases where the port differs from the
//! unpatched prototype; see the README beside the goldens.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use serde::Serialize;
use serde_json::{Map, Value};
use trusty_architect::classify::Alert;
use trusty_architect::poller::steps::notify_macos::{NOTIFY_TIMEOUT, Osascript, applescript};
use trusty_architect::poller::steps::usage::{
    Effective, Plan, Projection, RecordingProbe, UsagePaths, UtcTime, Window, effective, plan,
    poll_step, project, read_cache, rows_from_values, usage_alerts, usage_step,
};
use trusty_architect::to_python_json;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/9801")
}

fn corpus() -> TestResult<Value> {
    Ok(serde_json::from_str(&fs::read_to_string(
        fixture_dir().join("corpus_9801.json"),
    )?)?)
}

/// Golden lines of `name`, parsed, with their raw text.
fn golden(name: &str) -> TestResult<Vec<(String, Value)>> {
    let text = fs::read_to_string(fixture_dir().join(name))?;
    text.lines()
        .map(|l| Ok((l.to_owned(), serde_json::from_str(l)?)))
        .collect()
}

fn t(s: &str) -> TestResult<UtcTime> {
    Ok(UtcTime::parse_iso(s)?)
}

fn s(v: &Value) -> &str {
    v.as_str().unwrap_or_default()
}

fn arr(v: &Value) -> Vec<Value> {
    v.as_array().cloned().unwrap_or_default()
}

/// `{"ok": value}`, as `regen_9801.py`'s `outcome` writes it.
#[derive(Serialize)]
struct Ok_<T: Serialize> {
    ok: T,
}

/// Compares one golden line: an error for a `raises` row, else the bytes.
fn check_row<T: Serialize, E: std::fmt::Debug>(
    file: &str,
    line: &str,
    want: &Value,
    name: &str,
    got: Result<T, E>,
    ensure_ascii: bool,
) -> TestResult {
    if want[1].get("raises").is_some() {
        assert!(got.is_err(), "{file} {name}: Python raised, Rust did not");
        return Ok(());
    }
    let got = got.map_err(|e| format!("{file} {name}: Rust failed: {e:?}"))?;
    let text = to_python_json(&(name, Ok_ { ok: got }), ensure_ascii)?;
    assert_eq!(text, line, "{file} {name}: Rust (left) differs from Python (right)");
    Ok(())
}

fn case_rows(case: &Value) -> TestResult<Vec<trusty_architect::poller::steps::usage::Reading>> {
    Ok(rows_from_values(arr(&case["rows"]))?)
}

fn named<'a>(c: &'a Value, key: &str, name: &str) -> TestResult<&'a Value> {
    c[key]
        .as_array()
        .and_then(|a| a.iter().find(|x| x["name"] == name))
        .ok_or_else(|| format!("{key}: no case {name}").into())
}

#[test]
fn usage_alerts_match_python_goldens() -> TestResult {
    let c = corpus()?;
    for (line, want) in golden("usage_alerts.jsonl")? {
        let name = s(&want[0]);
        let case = named(&c, "alerts", name)?;
        let got = case_rows(case).and_then(|rows| {
            Ok(usage_alerts(&rows, t(s(&case["now"]))?, s(&case["current"]))?)
        });
        check_row("usage_alerts.jsonl", &line, &want, name, got, false)?;
    }
    Ok(())
}

#[derive(Serialize)]
struct RunwayRow {
    left: f64,
    rate: Option<f64>,
    rate_account: Option<String>,
    missing: Vec<String>,
    first_reset: Option<(String, String)>,
    runway_h: Option<f64>,
    gap_h: Option<f64>,
    covers_gap: Option<bool>,
}

#[derive(Serialize)]
struct PlanRow {
    need: bool,
    switch_at: Option<String>,
    triggers: Vec<(String, String)>,
    next: Option<String>,
    alternatives: Vec<String>,
    skipped: Vec<(String, String)>,
    note: Option<String>,
    runway: Option<RunwayRow>,
}

fn plan_row(p: &Plan) -> PlanRow {
    PlanRow {
        need: p.need,
        switch_at: p.switch_at.map(UtcTime::iso),
        triggers: p.triggers.iter().map(|(at, x)| (at.iso(), x.clone())).collect(),
        next: p.next.as_ref().map(|(a, _)| a.clone()),
        alternatives: p.alternatives.iter().map(|(a, _)| a.clone()).collect(),
        skipped: p.skipped.clone(),
        note: p.note.clone(),
        runway: p.runway.as_ref().map(|r| RunwayRow {
            left: r.left,
            rate: r.rate,
            rate_account: r.rate_account.clone(),
            missing: r.missing.clone(),
            first_reset: r.first_reset.as_ref().map(|(at, a)| (at.iso(), a.clone())),
            runway_h: r.runway_h,
            gap_h: r.gap_h,
            covers_gap: r.covers_gap,
        }),
    }
}

#[test]
fn plan_and_runway_match_python_goldens() -> TestResult {
    let c = corpus()?;
    for (line, want) in golden("plan.jsonl")? {
        let name = s(&want[0]);
        let case = named(&c, "alerts", name)?;
        let got = case_rows(case).and_then(|rows| {
            let p = plan(&rows, t(s(&case["now"]))?, Some(s(&case["current"])))?;
            Ok(plan_row(&p))
        });
        check_row("plan.jsonl", &line, &want, name, got, false)?;
    }
    Ok(())
}

#[derive(Serialize)]
struct WrapperRow {
    alerts: Vec<Alert>,
    log: Vec<String>,
}

/// The fleet-poll wrapper: one `usage` alert per tuple, `now` from the case,
/// and one `usage sampler ERROR` log line (and no alert) on a failure.
#[test]
fn usage_rows_match_python_goldens() -> TestResult {
    let c = corpus()?;
    for (line, want) in golden("usage_rows.jsonl")? {
        let name = s(&want[0]);
        let case = named(&c, "alerts", name)?;
        let dir = tempfile::tempdir()?;
        let paths = paths_in(dir.path());
        let account = serde_json::json!({"oauthAccount": {"emailAddress": s(&case["current"])}});
        fs::write(&paths.claude_json, account.to_string())?;
        let ledger: String = arr(&case["rows"])
            .iter()
            .map(|r| format!("{r}\n"))
            .collect();
        fs::write(&paths.ledger, ledger)?;
        let now = t(s(&case["now"]))?.timestamp();
        let mut state = Map::new();
        // Dry run: no cache sample and no probe, so only the ledger decides.
        let (alerts, log) = usage_step(
            &paths,
            &RecordingProbe::default(),
            &mut state,
            now,
            &[],
            true,
        );
        let python_log = arr(&want[1]["log"]);
        if python_log.is_empty() {
            let text = to_python_json(&(name, WrapperRow { alerts, log }), false)?;
            assert_eq!(text, line, "usage_rows.jsonl {name}");
        } else {
            assert!(alerts.is_empty(), "{name}: a failed step yields no alert");
            assert_eq!(log.len(), 1, "{name}: {log:?}");
            assert!(log[0].starts_with("usage sampler ERROR "), "{name}: {log:?}");
            assert!(s(&python_log[0]).starts_with("usage sampler ERROR "));
        }
    }
    Ok(())
}

#[derive(Serialize)]
struct EffectiveRow {
    pct5h: Value,
    pctwk: Value,
    reset5h: Option<String>,
    resetwk: Option<String>,
    inferred5h: bool,
    inferredwk: bool,
    stale: bool,
    age: f64,
}

fn effective_row(e: &Effective) -> TestResult<EffectiveRow> {
    Ok(EffectiveRow {
        pct5h: serde_json::to_value(e.pct5h)?,
        pctwk: serde_json::to_value(e.pctwk)?,
        reset5h: e.reset5h.map(UtcTime::iso),
        resetwk: e.resetwk.map(UtcTime::iso),
        inferred5h: e.inferred5h,
        inferredwk: e.inferredwk,
        stale: e.stale,
        age: e.age,
    })
}

#[test]
fn effective_matches_python_goldens() -> TestResult {
    let c = corpus()?;
    let cases = arr(&c["effective"]);
    let gold = golden("effective.jsonl")?;
    assert_eq!(cases.len(), gold.len());
    for (case, (line, _)) in cases.iter().zip(&gold) {
        let rows = rows_from_values(vec![case[1].clone()])?;
        let rec = rows.first().ok_or("row dropped")?;
        let e = effective(rec, t(s(&case[2]))?)?;
        let text = to_python_json(&(s(&case[0]), effective_row(&e)?), true)?;
        assert_eq!(&text, line, "effective.jsonl {}", case[0]);
    }
    Ok(())
}

#[derive(Serialize)]
struct ProjectRow {
    rate: f64,
    at: String,
    reset: Option<String>,
    before_reset: bool,
    pct: f64,
}

fn project_row(p: Option<Projection>) -> Option<ProjectRow> {
    p.map(|p| ProjectRow {
        rate: p.rate,
        at: p.at.iso(),
        reset: p.reset.map(UtcTime::iso),
        before_reset: p.before_reset,
        pct: p.pct,
    })
}

#[test]
fn project_matches_python_goldens() -> TestResult {
    let c = corpus()?;
    let cases = arr(&c["project"]);
    for (case, (line, want)) in cases.iter().zip(golden("project.jsonl")?) {
        let name = s(&case[0]);
        let window = if s(&case[4]) == "5h" {
            Window::FiveHour
        } else {
            Window::Week
        };
        let got = rows_from_values(arr(&case[1])).map_err(Into::into).and_then(
            |rows| -> TestResult<Option<ProjectRow>> {
                Ok(project_row(project(&rows, s(&case[2]), t(s(&case[3]))?, window)?))
            },
        );
        check_row("project.jsonl", &line, &want, name, got, true)?;
    }
    Ok(())
}

fn set_mtime(path: &Path, unix: f64) -> TestResult {
    let when = UNIX_EPOCH + Duration::from_secs_f64(unix);
    File::options().write(true).open(path)?.set_modified(when)?;
    Ok(())
}

fn paths_in(dir: &Path) -> UsagePaths {
    UsagePaths {
        ledger: dir.join("ledger.jsonl"),
        cache: dir.join("cache.json"),
        claude_json: dir.join("claude.json"),
        probe_script: dir.join("usage-probe.sh"),
        probe_log: dir.join("usage-probe.log"),
    }
}

#[test]
fn read_cache_matches_python_goldens() -> TestResult {
    let c = corpus()?;
    let now_ts = c["cache_now"].as_f64().ok_or("cache_now")?;
    let now = UtcTime::from_unix(now_ts)?;
    for (line, want) in golden("cache.jsonl")? {
        let name = s(&want[0]);
        let case = named(&c, "cache", name)?;
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("cache.json");
        if let Some(text) = case["text"].as_str() {
            fs::write(&path, text)?;
            set_mtime(&path, now_ts - case["age"].as_f64().ok_or("age")?)?;
        }
        let got = read_cache(now, &path, 1800.0).map(|r| {
            r.map(|r| (r.account, r.mtime.timestamp(), r.data))
        });
        check_row("cache.jsonl", &line, &want, name, got, false)?;
    }
    Ok(())
}

#[derive(Serialize)]
struct PollRow {
    alerts: Vec<(String, String)>,
    has_usage: bool,
    launched: Vec<[String; 3]>,
    ledger: Option<String>,
    now: f64,
    state: Option<BTreeMap<String, Value>>,
    step: usize,
}

#[test]
fn poll_step_matches_python_goldens() -> TestResult {
    let c = corpus()?;
    let now0 = c["poll_now"].as_f64().ok_or("poll_now")?;
    let gold = golden("poll_step.jsonl")?;
    let mut at = 0;
    for sc in arr(&c["poll"]) {
        let dir = tempfile::tempdir()?;
        let paths = paths_in(dir.path());
        for (file, key) in [(&paths.claude_json, "claude"), (&paths.ledger, "ledger")] {
            if let Some(text) = sc[key].as_str() {
                fs::write(file, text)?;
            }
        }
        if let Some(cache) = sc["cache"].as_object() {
            fs::write(&paths.cache, s(&cache["text"]))?;
            set_mtime(&paths.cache, now0 - cache["age"].as_f64().ok_or("age")?)?;
        }
        let mut state: Map<String, Value> = sc["state"].as_object().cloned().unwrap_or_default();
        for (i, step) in arr(&sc["steps"]).iter().enumerate() {
            let (line, want) = gold.get(at).ok_or("golden ran out")?;
            at += 1;
            let now = step["now"]
                .as_f64()
                .unwrap_or_else(|| now0 + step["dt"].as_f64().unwrap_or_default());
            let idle: Vec<(String, String)> = arr(&step["idle"])
                .iter()
                .map(|p| (s(&p[0]).to_owned(), s(&p[1]).to_owned()))
                .collect();
            let probe = RecordingProbe::default();
            let dry = step["dry_run"].as_bool().unwrap_or(false);
            let polled = poll_step(&paths, &probe, &mut state, now, &idle, dry)?;
            assert!(polled.launch_error.is_none());
            let row = PollRow {
                alerts: polled.alerts?,
                has_usage: state.contains_key("usage"),
                launched: probe.launched(),
                ledger: fs::read_to_string(&paths.ledger).ok(),
                now,
                state: state
                    .get("usage")
                    .and_then(Value::as_object)
                    .map(|m| m.clone().into_iter().collect()),
                step: i,
            };
            let name = s(&sc["name"]);
            assert_eq!(s(&want[0]), name);
            let text = to_python_json(&(name, row), false)?;
            assert_eq!(&text, line, "poll_step.jsonl {name} step {i}");
        }
    }
    assert_eq!(at, gold.len(), "every golden step ran");
    Ok(())
}

/// `notify_macos` runs `osascript -e <script>` with a 10 s timeout; the
/// script escapes `\` and `"` in both strings.
#[test]
fn notify_argv_matches_python_goldens() -> TestResult {
    let program = Osascript::default().program;
    assert_eq!(program, Path::new("osascript"));
    assert_eq!(NOTIFY_TIMEOUT, Duration::from_secs(10));
    for (line, want) in golden("notify.jsonl")? {
        let (title, message) = (s(&want[0]), s(&want[1]));
        let argv = [
            program.to_string_lossy().into_owned(),
            "-e".to_owned(),
            applescript(title, message),
        ];
        let kwargs = (("capture_output", true), ("timeout", 10));
        let text = to_python_json(&(title, message, argv, kwargs), false)?;
        assert_eq!(text, line);
    }
    Ok(())
}

/// The cases where the port differs from the unpatched prototype, each a
/// named ruling (README). A new divergence fails here until it is named.
#[test]
fn every_divergence_from_the_prototype_is_named() -> TestResult {
    let got: Vec<(String, String)> = golden("rulings.jsonl")?
        .into_iter()
        .map(|(_, v)| (s(&v["case"]).to_owned(), s(&v["ruling"]).to_owned()))
        .collect();
    let empty_run = "burn_rate_empty_run_is_no_rate";
    let want: Vec<(String, String)> = [
        ("usage_alerts:other_account_over_48h_old", empty_run),
        ("usage_alerts:current_5h_reading_over_3h_old", empty_run),
        ("usage_alerts:inferred_windows", empty_run),
        ("project:newest_older_than_span", empty_run),
        ("poll_step:probe_launch_fails", "failed_launch_is_not_a_probe"),
        ("notify_macos:spawn_fails", "notify_failure_is_logged"),
        ("notify_macos:timeout", "notify_failure_is_logged"),
        ("notify_macos:nonzero_exit", "notify_failure_is_logged"),
    ]
    .iter()
    .map(|(c, r)| ((*c).to_owned(), (*r).to_owned()))
    .collect();
    assert_eq!(got, want);
    // The port's side of each burn_rate case is the golden the tests above
    // already compare; the prototype side raised.
    for (_, v) in golden("rulings.jsonl")? {
        if v["ruling"] == empty_run {
            assert_eq!(s(&v["prototype"]["raises"]), "IndexError", "{}", v["case"]);
        }
    }
    Ok(())
}
