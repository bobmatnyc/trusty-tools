//! #9822 (#9171 A4): each `tm architect` verb's stdout against what the
//! prototype's Python printed for the same input.
//!
//! What: the goldens in `tests/fixtures/a4/` (`regen_a4.py`, over the
//! Architect's scrubbed live panes) and `tests/fixtures/parity/` (`regen.py`,
//! the A2 corpus) are the Python's recorded output. Each test runs the verb
//! and compares bytes. Where the Python prints a default and exits 0, the
//! verb must fail instead; those cases assert the error, and each would pass
//! against a fail-open verb only by printing the default, which they reject.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;
use trusty_architect::ScriptedTmux;
use trusty_architect::verbs::{self, InputTarget, VerbError};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn a4() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/a4")
}

fn parity() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/parity")
}

fn corpus() -> Result<Value, Box<dyn std::error::Error>> {
    Ok(serde_json::from_str(&fs::read_to_string(
        parity().join("corpus.json"),
    )?)?)
}

/// The `*.ansi` panes, in name order (the order `regen_a4.py` writes).
fn panes() -> Result<Vec<PathBuf>, Box<dyn std::error::Error>> {
    let mut out: Vec<PathBuf> = fs::read_dir(a4().join("panes"))?
        .map(|e| e.map(|e| e.path()))
        .collect::<Result<_, _>>()?;
    out.retain(|p| p.extension().is_some_and(|x| x == "ansi"));
    out.sort();
    Ok(out)
}

fn str_of(v: &Value) -> &str {
    v.as_str().unwrap_or_default()
}

#[test]
fn input_state_cli_matches_python() -> TestResult {
    let map = fs::read_to_string(a4().join("panes/targets.tsv"))?;
    let mut tmux = ScriptedTmux::new();
    let mut targets = Vec::new();
    for line in map.lines() {
        let (target, file) = line.split_once('\t').ok_or("bad targets.tsv row")?;
        tmux = tmux.script(
            "capture-pane",
            &fs::read_to_string(a4().join("panes").join(file))?,
        );
        targets.push(InputTarget::parse(target)?);
    }
    for gone in ["%99", "=tm-gone:"] {
        tmux = tmux.fail("capture-pane", &format!("can't find pane: {gone}"));
        targets.push(InputTarget::parse(gone)?);
    }
    let out = verbs::input_state(&tmux, &targets);
    assert_eq!(
        out.stdout,
        fs::read_to_string(a4().join("input_state_cli.txt"))?
    );
    // The script exits 0 here (cli_exit.json); the verb fails closed.
    let exit: Value = serde_json::from_str(&fs::read_to_string(a4().join("cli_exit.json"))?)?;
    assert_eq!(exit["input_state"], 0);
    assert_eq!(out.error.map(|e| e.exit_code()), Some(1));
    // The script's argv, with each target passed through exactly.
    let calls = tmux.calls();
    assert_eq!(calls.len(), targets.len());
    for (call, t) in calls.iter().zip(&targets) {
        assert_eq!(call, &["capture-pane", "-t", t.as_target(), "-p", "-e"]);
    }
    Ok(())
}

#[test]
fn classify_matches_python_on_live_panes() -> TestResult {
    let mut got = String::new();
    for pane in panes()? {
        got.push_str(&verbs::classify(&fs::read(&pane)?)?);
    }
    assert_eq!(got, fs::read_to_string(a4().join("classify.jsonl"))?);
    Ok(())
}

/// Every A2 capture: the golden row is `json.dumps([name, facts])`, so the
/// facts' own `json.dumps` is the row less `["<name>", ` and `]`.
#[test]
fn classify_matches_python_on_the_a2_corpus() -> TestResult {
    let c = corpus()?;
    let golden = fs::read_to_string(parity().join("parse_pane.jsonl"))?;
    let rows: Vec<&str> = golden.lines().collect();
    let caps = c["captures"].as_array().ok_or("no captures")?;
    assert_eq!(caps.len(), rows.len());
    for (cap, row) in caps.iter().zip(rows) {
        let name = serde_json::to_string(&cap[0])?;
        let facts = row
            .strip_prefix(&format!("[{name}, "))
            .and_then(|r| r.strip_suffix(']'))
            .ok_or("golden row shape")?;
        let capture = str_of(&cap[1]);
        if capture.is_empty() {
            assert!(verbs::classify(b"").is_err(), "{name}");
            continue;
        }
        assert_eq!(
            verbs::classify(capture.as_bytes())?,
            format!("{facts}\n"),
            "{name}"
        );
    }
    Ok(())
}

#[test]
fn quiet_verb_matches_python_goldens() -> TestResult {
    let c = corpus()?;
    let golden = fs::read_to_string(parity().join("quiet.jsonl"))?;
    let tmp = tempfile::tempdir()?;
    let mut checked = 0;
    for (q, row) in c["quiet"]
        .as_array()
        .ok_or("no quiet")?
        .iter()
        .zip(golden.lines())
    {
        let row: Value = serde_json::from_str(row)?;
        let path = tmp.path().join(format!("{}.txt", str_of(&q["case"])));
        match q["content"].as_str() {
            Some(content) => {
                fs::write(&path, content)?;
                let want: String = row["sessions"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|s| format!("{}\n", str_of(s)))
                    .collect();
                assert_eq!(verbs::quiet(&path)?, want, "{}", q["case"]);
            }
            // The Python reads a missing file as `[]`; the verb fails.
            None => {
                assert_eq!(row["sessions"], Value::Array(Vec::new()));
                let err = verbs::quiet(&path).expect_err("a missing list must fail");
                assert_eq!(err.exit_code(), 1);
            }
        }
        checked += 1;
    }
    assert!(checked >= 4, "only {checked} quiet cases");
    Ok(())
}

fn materialize(dir: &Path, files: &Value) -> TestResult {
    for f in files.as_array().into_iter().flatten() {
        let mut body = String::new();
        for l in f["lines"].as_array().into_iter().flatten() {
            for _ in 0..l[1].as_u64().unwrap_or_default() {
                body.push_str(str_of(&l[0]));
                body.push('\n');
            }
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

/// `print(self_ctx_tokens(dir))` per A2 case: the tokens, or, where the
/// Python prints `None`, a failure.
#[test]
fn self_ctx_verb_matches_python_goldens() -> TestResult {
    let c = corpus()?;
    let golden = fs::read_to_string(parity().join("self_ctx.jsonl"))?;
    let (mut measured, mut none) = (0, 0);
    for (s, row) in c["self_ctx"]
        .as_array()
        .ok_or("no self_ctx")?
        .iter()
        .zip(golden.lines())
    {
        let row: Value = serde_json::from_str(row)?;
        let tmp = tempfile::tempdir()?;
        materialize(tmp.path(), &s["files"])?;
        match row["tokens"].as_i64() {
            Some(tokens) => {
                assert_eq!(
                    verbs::self_ctx(tmp.path())?,
                    format!("{tokens}\n"),
                    "{}",
                    row["case"]
                );
                measured += 1;
            }
            None => {
                let err = verbs::self_ctx(tmp.path()).expect_err("None must fail");
                assert!(matches!(err, VerbError::Failed(_)), "{}", row["case"]);
                none += 1;
            }
        }
    }
    assert!(
        measured >= 3 && none >= 3,
        "measured {measured}, none {none}"
    );
    Ok(())
}

#[test]
fn classify_fails_closed_on_input_it_cannot_read() {
    for bad in [&b""[..], &b"\xff\xfe not utf-8"[..]] {
        let err = verbs::classify(bad).expect_err("must fail");
        assert_eq!(err.exit_code(), 1);
    }
}

#[test]
fn self_ctx_fails_closed_on_a_missing_directory() -> TestResult {
    let tmp = tempfile::tempdir()?;
    let err = verbs::self_ctx(&tmp.path().join("absent")).expect_err("must fail");
    assert_eq!(err.exit_code(), 1);
    Ok(())
}
