//! `dashboard_jsonl.fsck` and the module's command line (`fsck [ROOT]`) (#12029 P2b).
//!
//! Why: the owner's state fingerprint holds only if every line is canonical
//! and the last logged hash of every document matches its line.
//! What: [`fsck`] reports, in Python's order: per collection a parse error or
//! non-canonical bytes; every invalid change line; then, per `(coll, id)`
//! sorted by `str()` of each, the last entry's mismatch (`logged deleted
//! but present`, or `file line hash differs`). Only `changes.jsonl` is
//! read; rotated months are not. [`run`] is `main()`: the first argument
//! (else `.`) is the root, each problem a line, exit 1 when there is one.
//! Known gap: `(coll, id)` keys compare by type and value, so Python's
//! `1 == 1.0 == True` key merging is not reproduced.
//! Test: `dashboard_jsonl_recordings_replay_clean_against_the_port` (`fsck`,
//! `cli-*`), `shipped_dashboard_jsonl_fsck`.

use std::collections::HashMap;

use super::changes::scan;
use super::{JsonlError, coll_path, collections, parse_file, render_file, render_line, sha256};
use crate::pyjson_load::{PyInt, PyValue};
use crate::pyval::{get, int_text, py_str, repr};
use crate::spec_drift::Outcome;
use crate::spec_drift::py::os_error;

fn shown(v: Option<&PyValue>) -> String {
    v.map_or_else(|| "None".to_owned(), py_str)
}

/// Why: the store's own consistency check, as the frozen Python runs it.
/// What: see the module doc; `root` is used as given in every message.
/// Test: `dashboard_jsonl_recordings_replay_clean_against_the_port` (`fsck`).
///
/// # Errors
/// [`JsonlError::Os`] when a file cannot be read (Python raises there), and
/// [`JsonlError::Bad`] when a parsed document cannot be re-rendered.
pub fn fsck(root: &str) -> Result<Vec<String>, JsonlError> {
    let mut problems = Vec::new();
    let mut lines: HashMap<(String, String), String> = HashMap::new();
    let sv = PyValue::Int(PyInt::Small(super::SV));
    for coll in collections(root)? {
        let path = coll_path(root, &coll);
        let blob =
            std::fs::read(&path).map_err(|e| JsonlError::Os(os_error(path.as_ref(), &e).msg))?;
        let docs = match parse_file(&blob, &path) {
            Ok(d) => d,
            Err(JsonlError::Bad(m)) => {
                problems.push(m);
                continue;
            }
            Err(e) => return Err(e),
        };
        let docs: Vec<(String, PyValue, PyValue)> = docs
            .into_iter()
            .map(|(id, v, data)| (id, PyValue::Int(v), data))
            .collect();
        if blob != render_file(&docs)?.into_bytes() {
            problems.push(format!(
                "{path}: bytes are not canonical (re-render differs)"
            ));
        }
        for (id, version, data) in &docs {
            let line = render_line(id, version, data, &sv)?;
            lines.insert((coll.clone(), id.clone()), sha256(line.as_bytes()));
        }
    }
    let (changes, bad) = scan(root)?;
    problems.extend(bad);
    // dict insertion order with the last entry per key, as `last[key] = e` keeps it.
    let mut order: Vec<(Option<PyValue>, Option<PyValue>)> = Vec::new();
    let mut last: HashMap<String, (usize, PyValue)> = HashMap::new();
    for e in changes {
        let key = (get(&e, "coll").cloned(), get(&e, "id").cloned());
        let k = format!(
            "{}\u{0}{}",
            key_repr(key.0.as_ref()),
            key_repr(key.1.as_ref())
        );
        match last.get_mut(&k) {
            Some(slot) => slot.1 = e,
            None => {
                last.insert(k, (order.len(), e));
                order.push(key);
            }
        }
    }
    let mut keyed: Vec<(usize, &PyValue)> = last.values().map(|(i, e)| (*i, e)).collect();
    keyed.sort_by(|a, b| {
        let ka = &order[a.0];
        let kb = &order[b.0];
        (shown(ka.0.as_ref()), shown(ka.1.as_ref()))
            .cmp(&(shown(kb.0.as_ref()), shown(kb.1.as_ref())))
            .then(a.0.cmp(&b.0))
    });
    for (i, e) in keyed {
        let (coll, id) = &order[i];
        let have = match (coll, id) {
            (Some(PyValue::Str(c)), Some(PyValue::Str(d))) => lines.get(&(c.clone(), d.clone())),
            _ => None,
        };
        let want = get(e, "sha256");
        let seq = get(e, "seq").map_or_else(|| "None".to_owned(), py_str);
        let name = format!("{}/{}", shown(coll.as_ref()), shown(id.as_ref()));
        if matches!(get(e, "op"), Some(PyValue::Str(op)) if op == "delete") {
            if have.is_some() {
                problems.push(format!(
                    "{name}: logged deleted (seq {seq}) but present in the file"
                ));
            }
        } else if !same_hash(have, want) {
            problems.push(format!(
                "{name}: file line hash differs from last logged hash (seq {seq})"
            ));
        }
    }
    Ok(problems)
}

/// A dict key's identity: its type and `repr()` (`None` for a missing key).
fn key_repr(v: Option<&PyValue>) -> String {
    match v {
        None | Some(PyValue::Null) => "None".into(),
        Some(PyValue::Int(i)) => format!("i{}", int_text(i)),
        Some(other) => repr(other),
    }
}

/// `have == want`: a hash string against the logged value (`None` equals a missing or null one).
fn same_hash(have: Option<&String>, want: Option<&PyValue>) -> bool {
    match (have, want) {
        (None, None | Some(PyValue::Null)) => true,
        (Some(h), Some(PyValue::Str(w))) => h == w,
        _ => false,
    }
}

/// Why: the drop-in for `python3 dashboard_jsonl.py [ROOT]`.
/// What: `main()`: `fsck` of the first argument (else `.`), one problem per
/// line on stdout, exit 1 when there is a problem; extra arguments are ignored.
/// Test: `dashboard_jsonl_recordings_replay_clean_against_the_port` (`cli-*`),
/// `shipped_dashboard_jsonl_fsck`.
pub fn run(args: &[String]) -> Outcome {
    let root = args.first().map_or(".", String::as_str);
    match fsck(root) {
        Ok(probs) => Outcome {
            stdout: probs.iter().map(|p| format!("{p}\n")).collect(),
            stderr: String::new(),
            code: i32::from(!probs.is_empty()),
        },
        Err(e) => Outcome {
            stdout: String::new(),
            stderr: format!("{e}\n"),
            code: 1,
        },
    }
}
