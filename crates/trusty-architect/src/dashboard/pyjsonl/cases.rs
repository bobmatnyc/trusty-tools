//! The replay driver's mirror for `dashboard_jsonl` (#12029 P2b).
//!
//! Why: the recordings run `tests/fixtures/replay/dashboard_jsonl/driver.py`
//! against the frozen module; the port is replayed through the same case
//! lists, and its values ([`PyValue`]) stay inside the crate.
//! What: [`replay_cases`] is the driver: `cli ARGS...` runs [`super::run`];
//! `CASES.json` runs each `{"op": ...}` and prints `case <i> <op>` and the
//! answer, indented, or `JsonlError: <message>`. Roots are relative to the
//! working directory, the replay sandbox.
//! Test: `dashboard_jsonl_recordings_replay_clean_against_the_port`.

use std::path::Path;

use super::{
    JsonlError, Line, append_changes, canonical, changed_keys, collections, parse_file,
    read_changes, read_coll, render_file, render_line, sha256, sort_ids,
};
use crate::pyjson_file::read_text;
use crate::pyjson_load::{PyInt, PyValue, loads};
use crate::pystr::py_repr;
use crate::pyval::{dumps, get, int_text, py_str, repr};
use crate::spec_drift::Outcome;

const USAGE: &str = "usage: driver.py cli ARGS... | driver.py CASES.json\n";

fn s<'a>(c: &'a PyValue, k: &str) -> &'a str {
    match get(c, k) {
        Some(PyValue::Str(v)) => v,
        _ => "",
    }
}

fn str_list(items: Vec<String>) -> String {
    repr(&PyValue::List(
        items.into_iter().map(PyValue::Str).collect(),
    ))
}

fn docs_lines(docs: Vec<Line>) -> Vec<String> {
    if docs.is_empty() {
        return vec!["empty".into()];
    }
    docs.into_iter()
        .map(|(id, v, data)| {
            format!(
                "{} {} {}",
                py_repr(&id),
                int_text(&v),
                dumps(&data, None, false)
            )
        })
        .collect()
}

fn hex(text: &str) -> Vec<u8> {
    (0..text.len() / 2)
        .filter_map(|i| u8::from_str_radix(text.get(i * 2..i * 2 + 2)?, 16).ok())
        .collect()
}

fn answer(c: &PyValue, stderr: &mut String) -> Result<Vec<String>, JsonlError> {
    let null = PyValue::Null;
    let val = |k: &str| get(c, k).unwrap_or(&null);
    Ok(match s(c, "op") {
        "canonical" => vec![canonical(val("value"))?],
        "render_line" => {
            let sv = PyValue::Int(PyInt::Small(super::SV));
            let line = render_line(
                s(c, "id"),
                val("version"),
                val("data"),
                get(c, "sv").unwrap_or(&sv),
            )?;
            vec![py_repr(&line)]
        }
        "render_file" => {
            let docs = match val("docs") {
                PyValue::Dict(p) => p
                    .iter()
                    .map(|(k, v)| match v {
                        PyValue::List(l) if l.len() == 2 => (k.clone(), l[0].clone(), l[1].clone()),
                        _ => (k.clone(), PyValue::Null, PyValue::Null),
                    })
                    .collect(),
                _ => Vec::new(),
            };
            vec![py_repr(&render_file(&docs)?)]
        }
        "sort_ids" => {
            let mut ids: Vec<String> = match val("ids") {
                PyValue::List(l) => l.iter().map(py_str).collect(),
                _ => Vec::new(),
            };
            sort_ids(&mut ids);
            vec![str_list(ids)]
        }
        "sha256" => vec![sha256(s(c, "text").as_bytes())],
        "parse" => {
            let blob = match get(c, "hex") {
                Some(PyValue::Str(h)) => hex(h),
                _ => s(c, "text").as_bytes().to_vec(),
            };
            let path = get(c, "path").map_or("<jsonl>".to_owned(), py_str);
            docs_lines(parse_file(&blob, &path)?)
        }
        "read_coll" => docs_lines(read_coll(s(c, "root"), s(c, "coll"))?),
        "collections" => vec![str_list(collections(s(c, "root"))?)],
        "append" => {
            let entries = match val("entries") {
                PyValue::List(l) => l.clone(),
                _ => Vec::new(),
            };
            let done = append_changes(s(c, "root"), &entries, s(c, "ts"))?;
            stderr.push_str(done.warning.as_deref().unwrap_or(""));
            vec![format!("appended {}", done.written)]
        }
        "changed_keys" => vec![str_list(changed_keys(get(c, "old"), get(c, "new"))?)],
        "read_changes" => {
            let all = read_changes(s(c, "root"))?;
            if all.is_empty() {
                vec!["empty".into()]
            } else {
                all.iter().map(|e| dumps(e, None, false)).collect()
            }
        }
        "fsck" => {
            let probs = super::fsck(s(c, "root"))?;
            if probs.is_empty() {
                vec!["ok".into()]
            } else {
                probs
            }
        }
        other => return Err(JsonlError::Os(format!("KeyError: {}", py_repr(other)))),
    })
}

/// Why: the one entry `examples/replay_port.rs dashboard_jsonl` dispatches to.
/// What: see the module doc.
/// Test: `dashboard_jsonl_recordings_replay_clean_against_the_port`.
pub fn replay_cases(args: &[String]) -> Outcome {
    let fail = |stdout: String, stderr: String, code: i32| Outcome {
        stdout,
        stderr,
        code,
    };
    if args.first().is_some_and(|a| a == "cli") {
        return super::run(&args[1..]);
    }
    let [path] = args else {
        return fail(String::new(), USAGE.into(), 2);
    };
    let cases = match read_text(Path::new(path)).map(|t| loads(&t)) {
        Ok(Ok(PyValue::List(cases))) => cases,
        _ => return fail(String::new(), format!("cannot read {path}\n"), 1),
    };
    let (mut stdout, mut stderr) = (String::new(), String::new());
    for (i, case) in cases.iter().enumerate() {
        stdout.push_str(&format!("case {i} {}\n", s(case, "op")));
        let lines = match answer(case, &mut stderr) {
            Ok(lines) => lines,
            Err(JsonlError::Bad(m)) => vec![format!("JsonlError: {m}")],
            Err(JsonlError::Os(m)) => return fail(stdout, format!("{m}\n"), 1),
        };
        for line in lines {
            stdout.push_str(&format!("  {line}\n"));
        }
    }
    fail(stdout, stderr, 0)
}
