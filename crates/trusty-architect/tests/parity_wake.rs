//! #9171 A3b-1 parity: `wake()` and `dispatch_wake()` against goldens the
//! frozen prototype (`fleet-poll.py` at 988152c) writes.
//!
//! What: reads the `wake` and `dispatch` scenarios from
//! `tests/fixtures/a3/corpus_a3.json`, replays each through the Rust port over
//! a `ScriptedTmux` scripted exactly as `regen_a3.py` scripted the prototype's
//! `tmux()`, and compares the tmux argv log and the return phrase with
//! `wake.jsonl` / `dispatch_wake.jsonl` byte for byte. Divergences:
//! - Rulings (`wake_rulings.jsonl`): for a scenario an Architect ruling
//!   changes (A3b Q1, A3 Q3 rule 4), `wake.jsonl` holds the ruled argv and
//!   phrase and `wake_rulings.jsonl` what the prototype does instead.
//! - A3a plan finding 4: the port types the pointer as `send-keys -t P -l --
//!   MSG`; the prototype's `-l MSG` has no `--`. The replay drops that one
//!   `--` before comparing; nothing else is normalised.

use std::collections::{BTreeMap, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::{Value, json};
use trusty_architect::poller::{Pane, PollState, Waker, dispatch_wake};
use trusty_architect::{FakeClock, PaneId, ScriptedTmux, WakeError, to_python_json};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/a3")
}

fn read(name: &str) -> Result<String, Box<dyn std::error::Error>> {
    Ok(fs::read_to_string(fixture_dir().join(name))?)
}

fn corpus() -> Result<Value, Box<dyn std::error::Error>> {
    Ok(serde_json::from_str(&read("corpus_a3.json")?)?)
}

fn arr(v: &Value) -> &[Value] {
    v.as_array().map_or(&[], Vec::as_slice)
}

fn s(v: &Value) -> &str {
    v.as_str().unwrap_or_default()
}

/// Compares `rows` with the golden file line by line, naming the first diff.
fn assert_golden(name: &str, rows: &[String]) -> TestResult {
    let golden = read(name)?;
    let got: String = rows.iter().map(|r| format!("{r}\n")).collect();
    for (i, (g, r)) in golden.lines().zip(got.lines()).enumerate() {
        assert_eq!(
            r,
            g,
            "{name} line {}: Rust (left) differs from Python (right)",
            i + 1
        );
    }
    assert_eq!(got, golden, "{name}: row count or trailing bytes differ");
    Ok(())
}

/// A tmux scripted like `regen_a3.py`'s `tmux()`: per-verb FIFOs, then the
/// verb's fallback, else an error.
fn scripted(case: &Value) -> ScriptedTmux {
    let mut tmux = ScriptedTmux::new();
    let script: BTreeMap<String, Value> = case["script"]
        .as_object()
        .map(|m| m.clone().into_iter().collect())
        .unwrap_or_default();
    for (verb, outs) in &script {
        for out in arr(outs) {
            tmux = match out.get("err") {
                Some(err) => tmux.fail(verb, s(err)),
                None => tmux.script(verb, s(out)),
            };
        }
    }
    if let Some(fallback) = case["fallback"].as_object() {
        for (verb, out) in fallback {
            tmux = tmux.fallback(verb, s(out));
        }
    }
    tmux
}

fn panes(case: &Value) -> Result<Vec<Pane>, Box<dyn std::error::Error>> {
    arr(&case["panes"])
        .iter()
        .map(|p| {
            Ok(Pane {
                session: s(&p[0]).to_owned(),
                pane: PaneId::parse(s(&p[1]))?,
                cmd: "claude".to_owned(),
                active: s(&p[2]).to_owned(),
                path: String::new(),
                dead: "0".to_owned(),
            })
        })
        .collect()
}

/// The argv log with the port's `--` after `-l` dropped (finding 4).
fn python_argv(calls: Vec<Vec<String>>) -> Vec<Vec<String>> {
    calls
        .into_iter()
        .map(|mut c| {
            if c.first().is_some_and(|v| v == "send-keys")
                && c.get(3..5) == Some(&["-l".to_owned(), "--".to_owned()][..])
            {
                c.remove(4);
            }
            c
        })
        .collect()
}

#[derive(Serialize)]
struct WakeRow<'a> {
    case: &'a str,
    argv: Vec<Vec<String>>,
    result: Option<String>,
    raises: Option<String>,
}

#[test]
fn wake_matches_python_goldens() -> TestResult {
    let corpus = corpus()?;
    let now = corpus["now"].as_f64().unwrap_or_default();
    let alerts = s(&corpus["wake_alerts"]).to_owned();
    let mut rows = Vec::new();
    for case in arr(&corpus["wake"]) {
        let tmux = scripted(case);
        let clock = FakeClock::new(now);
        let waker = Waker {
            tmux: &tmux,
            clock: &clock,
            alerts: Path::new(&alerts),
            dry_run: case["dry_run"].as_bool().unwrap_or_default(),
            messages: &trusty_architect::poller::NoMessages,
        };
        let out = waker.wake(
            &panes(case)?,
            case["n"].as_u64().unwrap_or_default(),
            case["logged"].as_u64(),
        );
        let (result, raises) = match out {
            Ok(r) => (Some(r), None),
            Err(e) => (None, Some(e.to_string())),
        };
        let row = WakeRow {
            case: s(&case["case"]),
            argv: python_argv(tmux.calls()),
            result,
            raises,
        };
        rows.push(to_python_json(&row, false)?);
    }
    assert_golden("wake.jsonl", &rows)
}

#[test]
fn wake_rulings_name_real_divergences() -> TestResult {
    let golden = read("wake.jsonl")?;
    let ruled: BTreeMap<String, Value> = golden
        .lines()
        .map(|l| serde_json::from_str::<Value>(l).map(|v| (s(&v["case"]).to_owned(), v)))
        .collect::<Result<_, _>>()?;
    let rulings = read("wake_rulings.jsonl")?;
    let mut names = Vec::new();
    for line in rulings.lines() {
        let r: Value = serde_json::from_str(line)?;
        let case = s(&r["case"]);
        let ours = ruled
            .get(case)
            .ok_or_else(|| format!("{case}: ruled row missing"))?;
        assert!(s(&r["ruling"]).starts_with("A3"), "{case}: unnamed ruling");
        let python = &r["python"];
        assert!(
            python["argv"] != ours["argv"] || python["result"] != ours["result"],
            "{case}: the ruled row equals the prototype's, so it is no divergence"
        );
        names.push(case.to_owned());
    }
    assert_eq!(
        names,
        [
            "empty_box_after_typing",
            "message_plus_extra_text",
            "draft_then_message_left_draft",
            "draft_then_message_box_emptied"
        ]
    );
    Ok(())
}

#[test]
fn dispatch_wake_matches_python_goldens() -> TestResult {
    let corpus = corpus()?;
    let now = corpus["now"].as_f64().unwrap_or_default();
    let mut rows = Vec::new();
    for case in arr(&corpus["dispatch"]) {
        let mut state = PollState::default();
        for (step, args) in arr(&case["steps"]).iter().enumerate() {
            let clock = FakeClock::new(now);
            let mut calls: VecDeque<Value> = VecDeque::new();
            let scripted = args[3].clone();
            let result = dispatch_wake(
                &mut state,
                args[0].as_u64().unwrap_or_default(),
                args[1].as_u64(),
                args[2].as_bool().unwrap_or_default(),
                &clock,
                |n, logged| {
                    calls.push_back(json!([n, logged]));
                    match scripted.get("raise") {
                        Some(_) => Err(WakeError::NotOwner {
                            pane: "%1".to_owned(),
                            session: "tm-api".to_owned(),
                        }),
                        None => Ok(s(&scripted).to_owned()),
                    }
                },
            );
            if let Some(text) = args[3].get("raise") {
                assert_eq!(
                    format!("failed: {}", s(text)),
                    result,
                    "the raise text must match"
                );
            }
            let row = json!({
                "calls": Vec::from(calls),
                "case": s(&case["case"]),
                "result": result,
                "state": serde_json::to_value(&state)?,
                "step": step,
            });
            rows.push(to_python_json(&row, true)?);
        }
    }
    assert_golden("dispatch_wake.jsonl", &rows)
}
