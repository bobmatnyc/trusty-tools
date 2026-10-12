//! #9171 A2 parity: the Rust ports against goldens written by the live
//! prototype's Python.
//!
//! What: reads `tests/fixtures/parity/corpus.json`, runs every case through
//! the Rust port, renders each result with `to_python_json` exactly as
//! `regen.py` renders the Python result, and compares the whole golden file
//! byte for byte. Regenerate the goldens with the command in
//! `tests/fixtures/parity/README.md`; never edit them by hand.

use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;
use trusty_architect::classify::{
    Alert, DedupState, PaneInfo, PaneState, alert, dedup, evaluate, fleet_plan_alerts, parse_pane,
};
use trusty_architect::{classify_input, quiet, self_ctx, to_python_json};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/parity")
}

fn corpus() -> Result<Value, Box<dyn std::error::Error>> {
    Ok(serde_json::from_str(&fs::read_to_string(
        fixture_dir().join("corpus.json"),
    )?)?)
}

/// Compares `rows` with the golden file line by line, naming the first diff.
fn assert_golden(name: &str, rows: &[String]) -> TestResult {
    let golden = fs::read_to_string(fixture_dir().join(name))?;
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

fn str_of(v: &Value) -> &str {
    v.as_str().unwrap_or_default()
}

/// The `[name, capture]` pairs, in corpus order.
fn captures(c: &Value) -> Vec<(String, String)> {
    c["captures"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|p| (str_of(&p[0]).to_owned(), str_of(&p[1]).to_owned()))
        .collect()
}

fn capture<'a>(caps: &'a [(String, String)], name: &str) -> &'a str {
    caps.iter().find(|(n, _)| n == name).map_or("", |(_, c)| c)
}

#[test]
fn input_state_matches_python_goldens() -> TestResult {
    let c = corpus()?;
    let rows = captures(&c)
        .iter()
        .map(|(n, cap)| {
            let (state, text) = classify_input(cap);
            to_python_json(&(n, [state.as_str(), &text]), true)
        })
        .collect::<Result<Vec<_>, _>>()?;
    assert_golden("input_state.jsonl", &rows)
}

#[test]
fn parse_pane_matches_python_goldens() -> TestResult {
    let c = corpus()?;
    let rows = captures(&c)
        .iter()
        .map(|(n, cap)| to_python_json(&(n, parse_pane(cap)), true))
        .collect::<Result<Vec<_>, _>>()?;
    assert_golden("parse_pane.jsonl", &rows)
}

#[derive(Serialize)]
struct EvalRow<'a> {
    case: &'a str,
    step: usize,
    alerts: Vec<Alert>,
    ps: &'a PaneState,
}

#[test]
fn evaluate_matches_python_goldens() -> TestResult {
    let c = corpus()?;
    let caps = captures(&c);
    let mut rows = Vec::new();
    for sc in c["evaluate"].as_array().into_iter().flatten() {
        let mut ps = PaneState::default();
        for (step, s) in sc["steps"].as_array().into_iter().flatten().enumerate() {
            let info = parse_pane(capture(&caps, str_of(&s[0])));
            let alerts = evaluate(
                &info,
                &mut ps,
                s[1].as_f64().unwrap_or_default(),
                "tm-x",
                "%1",
            );
            let row = EvalRow {
                case: str_of(&sc["case"]),
                step,
                alerts,
                ps: &ps,
            };
            rows.push(to_python_json(&row, false)?);
        }
    }
    assert_golden("evaluate.jsonl", &rows)
}

#[derive(Serialize)]
struct DedupRow<'a> {
    case: &'a str,
    step: usize,
    new: Vec<Alert>,
    suppressed: usize,
    state: &'a DedupState,
}

#[test]
fn dedup_matches_python_goldens() -> TestResult {
    let c = corpus()?;
    let mut rows = Vec::new();
    for sc in c["dedup"].as_array().into_iter().flatten() {
        let mut state = DedupState::default();
        for (step, s) in sc["steps"].as_array().into_iter().flatten().enumerate() {
            let cands: Vec<Alert> = s[0]
                .as_array()
                .into_iter()
                .flatten()
                .map(|a| {
                    let t = a[5].as_f64().unwrap_or_default();
                    alert(
                        str_of(&a[0]),
                        str_of(&a[1]),
                        str_of(&a[2]),
                        str_of(&a[3]),
                        str_of(&a[4]),
                        t,
                    )
                })
                .collect();
            let (new, suppressed) = dedup(&mut state, &cands, s[1].as_f64().unwrap_or_default());
            let row = DedupRow {
                case: str_of(&sc["case"]),
                step,
                new,
                suppressed,
                state: &state,
            };
            rows.push(to_python_json(&row, true)?);
        }
    }
    assert_golden("dedup.jsonl", &rows)
}

#[derive(Serialize)]
struct PlanRow<'a> {
    case: &'a str,
    alerts: Vec<Alert>,
}

#[test]
fn plan_alerts_match_python_goldens() -> TestResult {
    let c = corpus()?;
    let caps = captures(&c);
    let mut rows = Vec::new();
    for p in c["plan"].as_array().into_iter().flatten() {
        let infos: Vec<(String, PaneInfo)> = p["panes"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|x| {
                (
                    str_of(&x[0]).to_owned(),
                    parse_pane(capture(&caps, str_of(&x[1]))),
                )
            })
            .collect();
        let alerts = fleet_plan_alerts(&infos, p["now"].as_f64().unwrap_or_default());
        rows.push(to_python_json(
            &PlanRow {
                case: str_of(&p["case"]),
                alerts,
            },
            false,
        )?);
    }
    assert_golden("plan.jsonl", &rows)
}

#[test]
fn clean_and_fingerprint_match_python_goldens() -> TestResult {
    let c = corpus()?;
    let rows = c["clean"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|t| {
            // `alert` applies `clean`; a fingerprint equal to the text exposes `fp`.
            let a = alert("k", "", str_of(t), "", "", 0.0);
            let fp = trusty_architect::classify::alerts::fingerprint(str_of(t));
            to_python_json(&[str_of(t), &a.detail, &fp], false)
        })
        .collect::<Result<Vec<_>, _>>()?;
    assert_golden("clean.jsonl", &rows)
}

#[derive(Serialize)]
struct QuietRow<'a> {
    case: &'a str,
    sessions: Vec<String>,
}

#[test]
fn quiet_matches_python_goldens() -> TestResult {
    let c = corpus()?;
    let tmp = tempfile::tempdir()?;
    let mut rows = Vec::new();
    for q in c["quiet"].as_array().into_iter().flatten() {
        let path = tmp.path().join(format!("{}.txt", str_of(&q["case"])));
        if let Some(content) = q["content"].as_str() {
            fs::write(&path, content)?;
        }
        let sessions = quiet::read_quiet_sessions(&path).into_iter().collect();
        rows.push(to_python_json(
            &QuietRow {
                case: str_of(&q["case"]),
                sessions,
            },
            true,
        )?);
    }
    for n in c["fixture_names"].as_array().into_iter().flatten() {
        rows.push(to_python_json(
            &(str_of(n), quiet::is_test_fixture_session(str_of(n))),
            true,
        )?);
    }
    assert_golden("quiet.jsonl", &rows)
}

/// `json.dumps(..., sort_keys=True)`: fields in alphabetical order.
#[derive(Serialize)]
struct SelfCtxRow<'a> {
    case: &'a str,
    newest: Option<String>,
    tokens: Option<i64>,
    usage: Option<Value>,
}

fn materialize(dir: &Path, files: &Value) -> TestResult {
    for f in files.as_array().into_iter().flatten() {
        let mut body = String::new();
        for l in f["lines"].as_array().into_iter().flatten() {
            let count = l[1].as_u64().unwrap_or_default();
            (0..count).for_each(|_| {
                body.push_str(str_of(&l[0]));
                body.push('\n');
            });
        }
        let path = dir.join(str_of(&f["name"]));
        fs::write(&path, body)?;
        let mtime = std::time::UNIX_EPOCH
            + std::time::Duration::from_secs(f["mtime"].as_u64().unwrap_or_default());
        fs::File::options()
            .write(true)
            .open(&path)?
            .set_modified(mtime)?;
    }
    Ok(())
}

#[test]
fn self_ctx_matches_python_goldens() -> TestResult {
    let c = corpus()?;
    let mut rows = Vec::new();
    for s in c["self_ctx"].as_array().into_iter().flatten() {
        let tmp = tempfile::tempdir()?;
        materialize(tmp.path(), &s["files"])?;
        let limit = s["limit"]
            .as_u64()
            .map_or(self_ctx::CANDIDATE_LIMIT, |l| l as usize);
        let start = s["start"].as_u64().unwrap_or(self_ctx::USAGE_TAIL_START);
        let newest = self_ctx::newest_transcript(tmp.path(), limit);
        let name = |p: &Path| p.file_name().and_then(|n| n.to_str()).map(str::to_owned);
        let target = s["usage_of"]
            .as_str()
            .map(str::to_owned)
            .or_else(|| newest.as_deref().and_then(name));
        let usage = match target {
            Some(t) => self_ctx::tail_last_assistant_usage(&tmp.path().join(t), start)?,
            None => None,
        };
        let row = SelfCtxRow {
            case: str_of(&s["case"]),
            newest: newest.as_deref().and_then(name),
            tokens: self_ctx::self_ctx_tokens(tmp.path())?,
            usage,
        };
        rows.push(to_python_json(&row, true)?);
    }
    assert_golden("self_ctx.jsonl", &rows)
}
