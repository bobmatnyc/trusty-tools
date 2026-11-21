//! df-T7i P1b parity: the sensors and wake helpers `fleet-poll.py` gained
//! after 988152c, against goldens the live prototype wrote
//! (`tests/fixtures/p1b/regen_p1b.py`).
//!
//! What: each test replays one corpus surface through the Rust port and
//! compares every row with the golden as JSON values (key order is checked
//! byte for byte by the A3 alert goldens). Rows where the prototype raises
//! are ruled: the port keeps the cycle and the test asserts what it does.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};
use trusty_architect::classify::{PaneState, evaluate, parse_pane};
use trusty_architect::poller::ctx_bar::bar_pct;
use trusty_architect::poller::digest::cap_wake;
use trusty_architect::poller::zone::zone_alerts;
use trusty_architect::poller::{
    completion_alerts, ctx_cycle_alerts, il_task_complete_for, selftest_alerts,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/p1b")
}

fn corpus() -> Result<Value, Box<dyn std::error::Error>> {
    Ok(serde_json::from_str(&fs::read_to_string(
        dir().join("corpus_p1b.json"),
    )?)?)
}

fn golden(name: &str) -> Result<Vec<Value>, Box<dyn std::error::Error>> {
    fs::read_to_string(dir().join(name))?
        .lines()
        .map(|l| Ok(serde_json::from_str(l)?))
        .collect()
}

fn arr(v: &Value) -> &[Value] {
    v.as_array().map_or(&[], Vec::as_slice)
}

fn s(v: &Value) -> &str {
    v.as_str().unwrap_or_default()
}

fn assert_rows(name: &str, got: &[Value]) -> TestResult {
    let want = golden(name)?;
    for (i, (g, w)) in got.iter().zip(&want).enumerate() {
        assert_eq!(
            g,
            w,
            "{name} row {}: Rust (left) differs from Python (right)",
            i + 1
        );
    }
    assert_eq!(got.len(), want.len(), "{name}: row count");
    Ok(())
}

fn parity_captures() -> Result<Map<String, Value>, Box<dyn std::error::Error>> {
    let c: Value = serde_json::from_str(&fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/parity/corpus.json"),
    )?)?;
    Ok(arr(&c["captures"])
        .iter()
        .map(|p| (s(&p[0]).to_owned(), p[1].clone()))
        .collect())
}

#[test]
fn ctx_bar_matches_python_goldens() -> TestResult {
    let c = corpus()?;
    let tmp = tempfile::tempdir()?;
    let mut rows = Vec::new();
    for case in arr(&c["ctx_bar"]) {
        let path = tmp.path().join(format!("{}.json", s(&case["case"])));
        match &case["schedule"] {
            Value::Null => {}
            Value::String(text) => fs::write(&path, text)?,
            v => fs::write(&path, v.to_string())?,
        }
        let window = case["window"].as_i64().unwrap_or_default();
        let pct = bar_pct(s(&case["session"]), window, &path)?;
        rows.push(json!({"case": case["case"], "bar_pct": pct}));
    }
    assert_rows("ctx_bar.jsonl", &rows)
}

#[test]
fn cap_wake_matches_python_goldens() -> TestResult {
    let c = corpus()?;
    let rows: Vec<Value> = arr(&c["cap_wake"])
        .iter()
        .map(|r| json!([r[0], cap_wake(s(&r[1]), s(&r[2]))]))
        .collect();
    assert_rows("cap_wake.jsonl", &rows)
}

#[test]
fn inbox_alerts_match_python_goldens() -> TestResult {
    let c = corpus()?;
    let now = c["now"].as_f64().unwrap_or_default();
    let tmp = tempfile::tempdir()?;
    let mut rows = Vec::new();
    for case in arr(&c["ctx_cycle"]) {
        let path = tmp.path().join(format!("cc-{}.jsonl", s(&case["case"])));
        let mut other = Map::new();
        for (i, st) in arr(&case["steps"]).iter().enumerate() {
            if let Some(text) = st["content"].as_str() {
                fs::write(&path, text)?;
            }
            if let Some(text) = st["append"].as_str().filter(|t| !t.is_empty()) {
                let mut old = fs::read(&path)?;
                old.extend_from_slice(text.as_bytes());
                fs::write(&path, old)?;
            }
            let dry = st["dry_run"].as_bool().unwrap_or_default();
            let alerts = ctx_cycle_alerts(&mut other, &path, now, dry);
            rows.push(json!({"case": case["case"], "step": i, "alerts": alerts, "state": other}));
        }
    }
    assert_rows("ctx_cycle.jsonl", &rows)?;
    let mut rows = Vec::new();
    for case in arr(&c["selftest"]) {
        let path = tmp.path().join(format!("st-{}.json", s(&case[0])));
        match &case[1] {
            Value::Null => {}
            Value::String(text) => fs::write(&path, text)?,
            v => fs::write(&path, v.to_string())?,
        }
        rows.push(json!({"case": case[0], "alerts": selftest_alerts(&path, now)}));
    }
    assert_rows("selftest.jsonl", &rows)
}

#[test]
fn zone_alerts_match_python_goldens() -> TestResult {
    let c = corpus()?;
    let tmp = tempfile::tempdir()?;
    let want = golden("zone.jsonl")?;
    let mut i = 0;
    for case in arr(&c["zone"]) {
        let path = tmp.path().join(format!("zone-{}.jsonl", s(&case["case"])));
        let mut other = Map::new();
        for (step, st) in arr(&case["steps"]).iter().enumerate() {
            let (now, el, bt) = (
                st[0].as_f64().unwrap_or_default(),
                st[1].as_u64(),
                st[2].as_i64(),
            );
            let (alerts, _) = zone_alerts(&mut other, now, || (el, bt), &path);
            let log = fs::read_to_string(&path).unwrap_or_default();
            let w = &want[i];
            i += 1;
            assert_eq!(
                log,
                s(&w["log"]),
                "{} step {step}: zone log",
                s(&case["case"])
            );
            if w["alerts"].get("raises").is_some() {
                // df-T7i P1b ruling: no boot time starts an empty fired list
                // (the prototype raises KeyError and loses the cycle).
                let fps: Vec<&str> = alerts.iter().map(|a| a.fingerprint.as_str()).collect();
                let want_fps: &[&str] = if step == 0 { &["warn-None"] } else { &[] };
                assert_eq!(fps, want_fps, "ruled row {step}");
                continue;
            }
            let row = json!({"case": case["case"], "step": step, "alerts": alerts, "state": other});
            assert_eq!(
                row["alerts"],
                w["alerts"],
                "{} step {step}",
                s(&case["case"])
            );
            assert_eq!(row["state"], w["state"], "{} step {step}", s(&case["case"]));
        }
    }
    assert_eq!(i, want.len(), "row count");
    Ok(())
}

#[test]
fn completion_matches_python_goldens() -> TestResult {
    let c = corpus()?;
    let caps = parity_captures()?;
    let now = c["now"].as_f64().unwrap_or_default();
    let mut rows = Vec::new();
    for case in arr(&c["completion"]) {
        let mut other = Map::new();
        for (i, st) in arr(&case["steps"]).iter().enumerate() {
            let mut info = parse_pane(caps.get(s(&st[0])).and_then(Value::as_str).unwrap_or(""));
            info.ctx = st[1].as_u64();
            let il = st[2].as_bool().unwrap_or_default();
            let alerts = completion_alerts(&mut other, "tm-x", "%1", &info, now, &|_| il);
            rows.push(json!({"case": case["case"], "step": i, "alerts": alerts, "state": other}));
        }
    }
    assert_rows("completion.jsonl", &rows)
}

#[test]
fn il_complete_matches_python_goldens() -> TestResult {
    let c = corpus()?;
    let tmp = tempfile::tempdir()?;
    let mut rows = Vec::new();
    for case in arr(&c["il"]) {
        let project = tmp.path().join(format!("proj-{}", s(&case["case"])));
        let base = project.join(".trusty-mpm/datg");
        let sdir = if case["per_session"].as_bool() == Some(true) {
            base.join("tm-x")
        } else {
            base
        };
        fs::create_dir_all(&sdir)?;
        if !case["state"].is_null() {
            fs::write(sdir.join("state.json"), case["state"].to_string())?;
        }
        let sched = tmp.path().join(format!("il-{}.json", s(&case["case"])));
        let name = case["name"].as_str().unwrap_or("tm-x");
        let entry = json!({"sessions": [{"name": name, "project": project}]});
        fs::write(&sched, entry.to_string())?;
        let done = il_task_complete_for(&sched, "tm-x");
        rows.push(json!({"case": case["case"], "complete": done}));
    }
    assert_rows("il_complete.jsonl", &rows)
}

#[test]
fn rc_active_and_idle_exempt_match_python_goldens() -> TestResult {
    let c = corpus()?;
    let rows: Vec<Value> = arr(&c["rc_lines"])
        .iter()
        .map(|l| json!([l, parse_pane(&format!("⏺ done\n{}\n", s(l))).rc_active]))
        .collect();
    assert_rows("rc_active.jsonl", &rows)?;
    let caps = parity_captures()?;
    let mut rows = Vec::new();
    for session in ["tm-architect-questions", "tm-a"] {
        let mut ps = PaneState::default();
        let mut info = parse_pane(caps.get("box_empty").and_then(Value::as_str).unwrap_or(""));
        info.ctx = Some(40);
        evaluate(&info, &mut ps, 0.0, session, "%1");
        let kinds: Vec<String> = evaluate(&info, &mut ps, 700.0, session, "%1")
            .into_iter()
            .map(|a| a.kind)
            .collect();
        rows.push(json!({"session": session, "kinds": kinds}));
    }
    assert_rows("idle_exempt.jsonl", &rows)
}
