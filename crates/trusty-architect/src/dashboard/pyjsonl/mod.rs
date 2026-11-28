//! The port of `dashboard_jsonl.py`: the canonical JSONL encoding, its parser and its change log (#12029 P2b).
//!
//! Why: the frozen Python store and its tools read and write these bytes,
//! and the Rust port must give the same bytes and the same error text for
//! every input, including values `serde_json` refuses (`NaN`, integers past
//! 64 bits, a repeated key). The crate's [`super::store`] keeps its own
//! stricter change log by design (#10063); this module is the reference
//! port and changes no store path.
//! What: [`canonical`] (sorted keys, compact separators, raw UTF-8, no
//! `NaN`), [`render_line`] / [`render_file`] / [`sort_ids`] / [`sha256`],
//! [`parse_file`] (tolerant of formatting, strict about shape, file order),
//! [`read_coll`], [`collections`] (case-sensitive change-log names),
//! [`changed_keys`]; the change log and `fsck` live in `changes` and `fsck`.
//! Values are [`PyValue`]s, so Python's types survive. Paths join as
//! `os.path.join` does, so messages name the path the caller gave.
//! Known gap: a lone surrogate (`"\ud800"`) cannot be held, so the
//! `render_line` UnicodeEncodeError arm does not exist here.
//! Test: `dashboard_jsonl_recordings_replay_clean_against_the_port`.

mod cases;
mod changes;
mod fsck;

use crate::poller::pyfmt::py_float_repr;
use crate::pyjson_load::{PyInt, PyValue, loads};
use crate::pyval::{cmp_int, get, int_text, json_str, repr};
use crate::spec_drift::py::{decode_error, os_error};

pub use cases::replay_cases;
pub(crate) use changes::{append_changes, read_changes};
pub use fsck::{fsck, run};

/// The line schema version this reader writes and accepts (`SV`).
pub(crate) const SV: i64 = 1;

/// `JsonlError` (a `ValueError`) or an `OSError` the module lets through.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum JsonlError {
    /// `JsonlError(msg)`: the bytes are not a canonical, readable collection.
    #[error("{0}")]
    Bad(String),
    /// An `OSError` the Python does not catch: its `str()`.
    #[error("{0}")]
    Os(String),
}

/// One parsed line: `(id, version, data)`.
pub(crate) type Line = (String, PyInt, PyValue);

/// `os.path.join(root, name)` for a relative `name`.
pub(crate) fn join(root: &str, name: &str) -> String {
    match root {
        "" => name.to_owned(),
        r if r.ends_with('/') => format!("{r}{name}"),
        r => format!("{r}/{name}"),
    }
}

/// `canonical(obj)`: `json.dumps(sort_keys=True, separators=(",", ":"),
/// ensure_ascii=False, allow_nan=False)`.
///
/// # Errors
/// [`JsonlError::Bad`] for a non-finite float, in Python's words.
pub(crate) fn canonical(v: &PyValue) -> Result<String, JsonlError> {
    let mut out = String::new();
    write_canonical(v, &mut out)?;
    Ok(out)
}

fn write_canonical(v: &PyValue, out: &mut String) -> Result<(), JsonlError> {
    match v {
        PyValue::Null => out.push_str("null"),
        PyValue::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        PyValue::Int(i) => out.push_str(&int_text(i)),
        PyValue::Float(f) if !f.is_finite() => {
            return Err(JsonlError::Bad(format!(
                "not canonically encodable: Out of range float values are not JSON compliant: {}",
                py_float_repr(*f)
            )));
        }
        PyValue::Float(f) => out.push_str(&py_float_repr(*f)),
        PyValue::Str(s) => json_str(s, false, out),
        PyValue::List(items) => {
            out.push('[');
            for (n, item) in items.iter().enumerate() {
                if n > 0 {
                    out.push(',');
                }
                write_canonical(item, out)?;
            }
            out.push(']');
        }
        PyValue::Dict(pairs) => {
            let mut sorted: Vec<&(String, PyValue)> = pairs.iter().collect();
            sorted.sort_by(|a, b| a.0.cmp(&b.0));
            out.push('{');
            for (n, (k, item)) in sorted.into_iter().enumerate() {
                if n > 0 {
                    out.push(',');
                }
                json_str(k, false, out);
                out.push(':');
                write_canonical(item, out)?;
            }
            out.push('}');
        }
    }
    Ok(())
}

/// `render_line(doc_id, version, data, sv)` as text, with its trailing LF.
///
/// # Errors
/// [`canonical`]'s.
pub(crate) fn render_line(
    id: &str,
    version: &PyValue,
    data: &PyValue,
    sv: &PyValue,
) -> Result<String, JsonlError> {
    let line = PyValue::Dict(vec![
        ("data".into(), data.clone()),
        ("id".into(), PyValue::Str(id.to_owned())),
        ("sv".into(), sv.clone()),
        ("version".into(), version.clone()),
    ]);
    Ok(canonical(&line)? + "\n")
}

/// `sort_ids(ids)`: UTF-8 byte order, which is code point order.
pub(crate) fn sort_ids(ids: &mut [String]) {
    ids.sort();
}

/// `render_file(docs)`: every line in id order; empty for no documents.
///
/// # Errors
/// [`canonical`]'s.
pub(crate) fn render_file(docs: &[(String, PyValue, PyValue)]) -> Result<String, JsonlError> {
    let mut order: Vec<&(String, PyValue, PyValue)> = docs.iter().collect();
    order.sort_by(|a, b| a.0.cmp(&b.0));
    let sv = PyValue::Int(PyInt::Small(SV));
    order
        .into_iter()
        .map(|(id, version, data)| render_line(id, version, data, &sv))
        .collect()
}

/// `sha256(blob)`: lowercase hex.
pub(crate) fn sha256(blob: &[u8]) -> String {
    crate::sha256::sha256_hex(blob)
}

/// A positive `int` (not a `bool`): `sv` and `version`.
fn positive(v: Option<&PyValue>) -> Option<&PyInt> {
    match v {
        Some(PyValue::Int(i)) if cmp_int(i, &PyInt::Small(1)).is_ge() => Some(i),
        _ => None,
    }
}

fn shown(v: Option<&PyValue>) -> String {
    v.map_or_else(|| "None".to_owned(), repr)
}

/// Why: every reader of a collection file goes through this one parser.
/// What: `parse_file(blob, path)`: lines in file order, blank lines skipped;
/// each is a JSON object with a string `id`, a dict `data`, `sv` 1 and a
/// positive integer `version`, and an id appears once.
/// Test: `dashboard_jsonl_recordings_replay_clean_against_the_port` (`parse`).
///
/// # Errors
/// [`JsonlError::Bad`] with Python's message for the first bad line.
pub(crate) fn parse_file(blob: &[u8], path: &str) -> Result<Vec<Line>, JsonlError> {
    let bad = |m: String| JsonlError::Bad(m);
    let text = std::str::from_utf8(blob)
        .map_err(|e| bad(format!("{path}: not UTF-8: {}", decode_error(blob, &e).msg)))?;
    let mut out: Vec<Line> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (n, raw) in text.split('\n').enumerate() {
        if raw.is_empty() {
            continue;
        }
        let at = format!("{path}:{}", n + 1);
        let line = loads(raw).map_err(|e| bad(format!("{at}: invalid JSON: {}", e.py_str())))?;
        let (Some(PyValue::Str(id)), Some(data @ PyValue::Dict(_))) =
            (get(&line, "id"), get(&line, "data"))
        else {
            return Err(bad(format!("{at}: a line is {{data, id, sv, version}}")));
        };
        let sv = get(&line, "sv");
        let Some(svi) = positive(sv) else {
            return Err(bad(format!("{at}: bad sv {}", shown(sv))));
        };
        if cmp_int(svi, &PyInt::Small(SV)).is_gt() {
            return Err(bad(format!(
                "{at}: sv {} is newer than this reader ({SV})",
                int_text(svi)
            )));
        }
        let version = get(&line, "version");
        let Some(v) = positive(version) else {
            return Err(bad(format!("{at}: bad version {}", shown(version))));
        };
        if !seen.insert(id.clone()) {
            return Err(bad(format!("{at}: duplicate id {id}")));
        }
        out.push((id.clone(), v.clone(), data.clone()));
    }
    Ok(out)
}

/// `coll_path(root, coll)`.
pub(crate) fn coll_path(root: &str, coll: &str) -> String {
    join(root, &format!("{coll}.jsonl"))
}

/// `read_coll(root, coll)`: the collection's lines; empty when the file is absent.
///
/// # Errors
/// [`JsonlError::Os`] when it cannot be read; [`parse_file`]'s errors.
pub(crate) fn read_coll(root: &str, coll: &str) -> Result<Vec<Line>, JsonlError> {
    let path = coll_path(root, coll);
    match std::fs::read(&path) {
        Ok(blob) => parse_file(&blob, &path),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(JsonlError::Os(os_error(path.as_ref(), &e).msg)),
    }
}

/// `RESERVED.match(name)`: `changes` or `changes-YYYY-MM`, letter case exact.
fn reserved(name: &str) -> bool {
    let Some(rest) = name.strip_prefix("changes") else {
        return false;
    };
    let digits = |s: &str, n: usize| s.len() == n && s.bytes().all(|b| b.is_ascii_digit());
    rest.is_empty()
        || rest
            .strip_prefix('-')
            .and_then(|r| r.split_once('-'))
            .is_some_and(|(y, m)| digits(y, 4) && digits(m, 2))
}

/// `collections(root)`: names of `*.jsonl` under `root`, hidden names and
/// change logs excluded, sorted; empty when `root` is absent.
///
/// # Errors
/// [`JsonlError::Os`] when `root` exists but cannot be listed.
pub(crate) fn collections(root: &str) -> Result<Vec<String>, JsonlError> {
    let rd = match std::fs::read_dir(root) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(JsonlError::Os(os_error(root.as_ref(), &e).msg)),
    };
    let mut out = Vec::new();
    for entry in rd {
        let entry = entry.map_err(|e| JsonlError::Os(os_error(root.as_ref(), &e).msg))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if let Some(coll) = name.strip_suffix(".jsonl")
            && !name.starts_with('.')
            && !reserved(coll)
        {
            out.push(coll.to_owned());
        }
    }
    out.sort();
    Ok(out)
}

/// `changed_keys(old, new)`: top-level keys whose canonical text differs; a
/// missing (or non-dict) body reads as `{}`.
///
/// # Errors
/// [`canonical`]'s.
pub(crate) fn changed_keys(
    old: Option<&PyValue>,
    new: Option<&PyValue>,
) -> Result<Vec<String>, JsonlError> {
    let pairs = |v: Option<&PyValue>| match v {
        Some(PyValue::Dict(p)) => p.clone(),
        _ => Vec::new(),
    };
    let (old, new) = (pairs(old), pairs(new));
    let find =
        |p: &[(String, PyValue)], k: &str| p.iter().find(|(x, _)| x == k).map(|(_, v)| v.clone());
    let mut keys: Vec<String> = old
        .iter()
        .chain(new.iter())
        .map(|(k, _)| k.clone())
        .collect();
    keys.sort();
    keys.dedup();
    let mut out = Vec::new();
    for k in keys {
        let differs = match (find(&old, &k), find(&new, &k)) {
            (Some(a), Some(b)) => canonical(&a)? != canonical(&b)?,
            _ => true,
        };
        if differs {
            out.push(k);
        }
    }
    Ok(out)
}
