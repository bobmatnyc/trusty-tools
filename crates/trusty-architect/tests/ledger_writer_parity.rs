//! #9758: the Rust writer leaves the prototype's bytes (LDG-1.5, LDG-3.3).
//!
//! `tests/fixtures/ledger/writer/corpus.jsonl` is written by
//! `regen_writer.py`, which runs `datg.py` on the four scrubbed stores with a
//! pinned clock. Each step here runs the same ops through [`Writer::batch`]
//! (or [`writer::show`]) with the same clock and compares stdout, the bytes
//! appended to `log.jsonl` and `archive.jsonl`, the `state.json` length and
//! `v`, and, after the last step, the whole `state.json`.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;
use trusty_architect::FakeClock;
use trusty_architect::ledger::rpc::{BatchRequest, Op, StoreRef};
use trusty_architect::ledger::writer::{self, Writer};

type R = Result<(), Box<dyn std::error::Error>>;

/// 2026-10-21T10:00:00Z, the regen script's `START`.
const START: f64 = 1_792_576_800.0;

fn ledger_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ledger")
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_default()
}

fn no_secrets(_: &str) -> Option<String> {
    None
}

/// Copies the scrubbed `store` into `root/.trusty-mpm/dbatg/<store>/`.
fn seed(root: &Path, store: &str) -> std::io::Result<PathBuf> {
    let dir = root.join(".trusty-mpm/dbatg").join(store);
    fs::create_dir_all(&dir)?;
    for entry in fs::read_dir(ledger_dir().join(store))? {
        let entry = entry?;
        fs::copy(entry.path(), dir.join(entry.file_name()))?;
    }
    Ok(dir)
}

#[test]
fn ledger_writer_matches_prototype_bytes() -> R {
    let corpus = read(&ledger_dir().join("writer/corpus.jsonl"));
    assert!(!corpus.is_empty(), "corpus.jsonl is missing");
    let tmp = tempfile::tempdir()?;
    let mut steps = 0;
    let mut last: Option<(String, PathBuf)> = None;
    for line in corpus.lines() {
        let case: Value = serde_json::from_str(line)?;
        let store = case["store"].as_str().ok_or("store")?.to_owned();
        let at = format!("{store} step {}", case["step"]);
        let dir = tmp.path().join(&store);
        if last.as_ref().is_none_or(|(s, _)| s != &store) {
            if let Some((prev, prev_dir)) = last.take() {
                compare_final(&prev, &prev_dir)?;
            }
            fs::create_dir_all(&dir)?;
            last = Some((store.clone(), seed(&dir, &store)?));
        }
        let files = dir.join(".trusty-mpm/dbatg").join(&store);
        let verb = case["verb"].as_str().ok_or("verb")?;
        let minute = case["minute"].as_f64().ok_or("minute")?;
        let late = if matches!(verb, "prune" | "show") {
            2.0 * 86_400.0
        } else {
            0.0
        };
        let clock = FakeClock::new(START + late + 60.0 * minute);
        let scan = no_secrets;
        let w = Writer::new(&clock, &scan);
        let store_ref = StoreRef {
            project: dir.clone(),
            session: store.clone(),
        };
        let (log0, arch0) = (
            read(&files.join("log.jsonl")),
            read(&files.join("archive.jsonl")),
        );
        let ops: Vec<Op> = serde_json::from_value(case["ops"].clone())?;
        let (exit, stdout) = if verb == "show" {
            let brief = case["stdout"].as_str().is_some_and(|s| !s.starts_with('{'));
            match writer::show(&store_ref, brief) {
                Ok(text) => (0, text + "\n"),
                Err(e) => (e.code().exit_code(), String::new()),
            }
        } else {
            let before = fs::metadata(files.join("state.json"))?.len();
            let req = BatchRequest {
                store: store_ref,
                request_id: String::new(),
                if_v: None,
                ops,
            };
            let dry_run = verb == "prune" && line.contains("\"dry_run\": true");
            match w.batch(&req) {
                Ok(resp) if verb == "prune" => {
                    let after = fs::metadata(files.join("state.json"))?.len();
                    (
                        0,
                        writer::render_prune(&resp, dry_run, before, after) + "\n",
                    )
                }
                Ok(resp) => (0, writer::render_ids(&resp) + "\n"),
                Err(e) => (e.code().exit_code(), String::new()),
            }
        };
        assert_eq!(
            i64::from(exit),
            case["exit"].as_i64().ok_or("exit")?,
            "{at}: exit"
        );
        assert_eq!(
            stdout,
            case["stdout"].as_str().ok_or("stdout")?,
            "{at}: stdout"
        );
        let log = read(&files.join("log.jsonl"));
        let arch = read(&files.join("archive.jsonl"));
        assert_eq!(
            &log[log0.len().min(log.len())..],
            case["log"].as_str().ok_or("log")?,
            "{at}: log.jsonl"
        );
        assert_eq!(
            &arch[arch0.len().min(arch.len())..],
            case["archive"].as_str().ok_or("archive")?,
            "{at}: archive.jsonl"
        );
        let state = read(&files.join("state.json"));
        assert_eq!(
            state.len() as u64,
            case["state_len"].as_u64().ok_or("len")?,
            "{at}: state.json length"
        );
        let v: Value = serde_json::from_str(&state)?;
        assert_eq!(v["v"], case["v"], "{at}: v");
        steps += 1;
    }
    if let Some((store, dir)) = last {
        compare_final(&store, &dir)?;
    }
    assert!(steps >= 60, "only {steps} corpus steps ran");
    Ok(())
}

fn compare_final(store: &str, files: &Path) -> R {
    let want = read(&ledger_dir().join(format!("writer/{store}.state.json")));
    assert_eq!(
        read(&files.join("state.json")),
        want,
        "{store}: final state.json"
    );
    Ok(())
}
