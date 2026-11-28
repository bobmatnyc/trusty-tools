//! `dashboard_store.py`'s value rules: names, bodies, timestamps, the file bytes, the list order and the request refusals.
//!
//! Why: each of these decides a byte or an error text the frozen prototype
//! writes, so each follows Python's own semantics (`re` with `$` matching
//! before a final newline, `\d` matching any Unicode digit, `==` across
//! `int` and `float`).
//! What: [`check_id`], [`check_body`], [`normalize_timestamps`], [`render`]
//! (`json.dumps(indent=2, sort_keys=True, ensure_ascii=False)` plus LF),
//! [`sort_key`] / [`cmp_keys`] (the `order_by` key), [`refuse_duplicate_request`]
//! and [`restore_on_brief`].
//! Test: `dashboard_store_recordings_replay_clean_against_the_port` (`dir`, `jsonl`).

use std::cmp::Ordering;

use super::PyStoreError;
use crate::dashboard::pyderive::py_eq;
use crate::pyjson_load::{PyInt, PyValue};
use crate::pystr::{is_digit, strip};
use crate::pyval::{cmp_int, dumps, get, int_text, repr, truthy};

const ID_HINT: &str = "use letters, digits, '_', '-', '.', max 80, no leading '.'";

/// `s` without one final `\n`: the text a Python `$` anchors before.
pub(crate) fn before_final_newline(s: &str) -> &str {
    s.strip_suffix('\n').unwrap_or(s)
}

/// `_check_id(kind, value)`: `^[A-Za-z0-9][A-Za-z0-9_.-]{0,79}$`.
///
/// # Errors
/// [`PyStoreError::Store`] naming the kind and `repr(value)`.
pub(crate) fn check_id<'a>(kind: &str, value: &'a PyValue) -> Result<&'a str, PyStoreError> {
    if let PyValue::Str(s) = value {
        let body = before_final_newline(s);
        let mut chars = body.chars();
        let first_ok = chars.next().is_some_and(|c| c.is_ascii_alphanumeric());
        let rest_ok = chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'));
        if first_ok && rest_ok && body.chars().count() <= 80 {
            return Ok(s);
        }
    }
    Err(PyStoreError::Store(format!(
        "invalid {kind} {}: {ID_HINT}",
        repr(value)
    )))
}

/// `_check_body(data)`: a dict without `version`.
///
/// # Errors
/// [`PyStoreError::Store`] in the prototype's words.
pub(crate) fn check_body(data: Option<&PyValue>) -> Result<&[(String, PyValue)], PyStoreError> {
    let Some(PyValue::Dict(pairs)) = data else {
        return Err(PyStoreError::Store(
            "document body must be a JSON object".into(),
        ));
    };
    if pairs.iter().any(|(k, _)| k == "version") {
        return Err(PyStoreError::Store(
            "'version' is reserved for the store".into(),
        ));
    }
    Ok(pairs)
}

const TS_KEYS: [&str; 8] = [
    "created", "updated", "due", "at", "until", "eta", "eta_low", "eta_high",
];

/// `_MINUTE_TS.match(s)`: `^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}Z$`.
fn minute_ts(s: &str) -> bool {
    let c: Vec<char> = before_final_newline(s).chars().collect();
    let shape = "dddd-dd-ddTdd:ddZ";
    c.len() == shape.len()
        && shape.chars().zip(&c).all(|(want, got)| match want {
            'd' => is_digit(*got),
            lit => lit == *got,
        })
}

/// `normalize_timestamps(value, key)`: a minute-precision timestamp under a
/// timestamp key gets seconds (`value[:-1] + ":00Z"`), recursively.
pub(crate) fn normalize_timestamps(value: &PyValue, key: Option<&str>) -> PyValue {
    match value {
        PyValue::Dict(pairs) => PyValue::Dict(
            pairs
                .iter()
                .map(|(k, v)| (k.clone(), normalize_timestamps(v, Some(k))))
                .collect(),
        ),
        PyValue::List(items) => {
            PyValue::List(items.iter().map(|v| normalize_timestamps(v, key)).collect())
        }
        PyValue::Str(s)
            if key.is_some_and(|k| TS_KEYS.contains(&k) || k.ends_with("_at")) && minute_ts(s) =>
        {
            let mut cut = s.clone();
            cut.pop();
            PyValue::Str(cut + ":00Z")
        }
        other => other.clone(),
    }
}

/// `value` with every dict's keys sorted, as `sort_keys=True` writes it.
pub(crate) fn sorted(value: &PyValue) -> PyValue {
    match value {
        PyValue::Dict(pairs) => {
            let mut out: Vec<(String, PyValue)> =
                pairs.iter().map(|(k, v)| (k.clone(), sorted(v))).collect();
            out.sort_by(|a, b| a.0.cmp(&b.0));
            PyValue::Dict(out)
        }
        PyValue::List(items) => PyValue::List(items.iter().map(sorted).collect()),
        other => other.clone(),
    }
}

/// `dict(pairs)` with `key` set: replaced in place, or appended.
pub(crate) fn with_key(
    pairs: &[(String, PyValue)],
    key: &str,
    v: PyValue,
) -> Vec<(String, PyValue)> {
    let mut out = pairs.to_vec();
    match out.iter_mut().find(|(k, _)| k == key) {
        Some(slot) => slot.1 = v,
        None => out.push((key.to_owned(), v)),
    }
    out
}

/// `_render(version, data)`: the dir file's bytes.
pub(crate) fn render(version: &PyInt, data: &[(String, PyValue)]) -> String {
    let body = PyValue::Dict(with_key(data, "version", PyValue::Int(version.clone())));
    dumps(&sorted(&body), Some(2), false) + "\n"
}

/// `version + 1`.
pub(crate) fn next_version(v: &PyInt) -> PyInt {
    crate::dashboard::pyjsonl::bump(v)
}

/// `_sort_key(value)`: rank, then the value of that rank.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum SortKey {
    /// `(0, number)`.
    Num(PyValue),
    /// `(1, str)`.
    Str(String),
    /// `(2, json.dumps(value, sort_keys=True))`.
    Json(String),
    /// `(3, str(value))` for a bool or `None`.
    Other(String),
}

pub(crate) fn sort_key(v: &PyValue) -> SortKey {
    match v {
        PyValue::Bool(_) | PyValue::Null => SortKey::Other(repr(v)),
        PyValue::Int(_) | PyValue::Float(_) => SortKey::Num(v.clone()),
        PyValue::Str(s) => SortKey::Str(s.clone()),
        other => SortKey::Json(dumps(&sorted(other), None, true)),
    }
}

/// An `int` or `float` as `f64` (a big int through its text).
fn as_f64(v: &PyValue) -> f64 {
    match v {
        PyValue::Int(PyInt::Small(n)) => *n as f64,
        PyValue::Int(i) => int_text(i).parse().unwrap_or(f64::NAN),
        PyValue::Float(f) => *f,
        _ => f64::NAN,
    }
}

/// Tuple order of two [`SortKey`]s; numbers compare by value (`NaN` as equal).
pub(crate) fn cmp_keys(a: &SortKey, b: &SortKey) -> Ordering {
    let rank = |k: &SortKey| match k {
        SortKey::Num(_) => 0,
        SortKey::Str(_) => 1,
        SortKey::Json(_) => 2,
        SortKey::Other(_) => 3,
    };
    match (a, b) {
        (SortKey::Num(PyValue::Int(x)), SortKey::Num(PyValue::Int(y))) => cmp_int(x, y),
        (SortKey::Num(x), SortKey::Num(y)) => {
            as_f64(x).partial_cmp(&as_f64(y)).unwrap_or(Ordering::Equal)
        }
        (SortKey::Str(x), SortKey::Str(y))
        | (SortKey::Json(x), SortKey::Json(y))
        | (SortKey::Other(x), SortKey::Other(y)) => x.cmp(y),
        _ => rank(a).cmp(&rank(b)),
    }
}

/// `_open_status_updates(d)`: unacked `status_update` owner actions.
fn open_status_updates(d: &PyValue) -> usize {
    match get(d, "owner_actions") {
        Some(PyValue::List(acts)) => acts
            .iter()
            .filter(|a| {
                matches!(a, PyValue::Dict(_))
                    && matches!(get(a, "action"), Some(PyValue::Str(s)) if s == "status_update")
                    && get(a, "at").is_some_and(truthy)
                    && !get(a, "ack_at").is_some_and(truthy)
            })
            .count(),
        _ => 0,
    }
}

/// `_open_brief(d)`: an unfulfilled (or refresh) `brief_request`.
fn open_brief(d: &PyValue) -> Option<&PyValue> {
    let req = get(d, "brief_request")?;
    let refresh = matches!(get(req, "refresh"), Some(PyValue::Bool(true)));
    (matches!(req, PyValue::Dict(_))
        && get(req, "at").is_some_and(truthy)
        && (!get(d, "brief").is_some_and(truthy) || refresh))
        .then_some(req)
}

/// `_refuse_duplicate_request(coll, doc_id, old, new)`: an owner request
/// already waiting is not requested twice.
///
/// # Errors
/// [`PyStoreError::Conflict`] in the prototype's words.
pub(crate) fn refuse_duplicate_request(
    coll: &str,
    id: &str,
    old: &PyValue,
    new: &PyValue,
) -> Result<(), PyStoreError> {
    if !matches!(coll, "items" | "goals") || !truthy(old) {
        return Ok(());
    }
    if coll == "items" && open_status_updates(new) > open_status_updates(old).max(1) {
        return Err(PyStoreError::Conflict(format!(
            "{coll}/{id}: a status update is already requested and waiting for the Architect"
        )));
    }
    if let (Some(ob), Some(nb)) = (open_brief(old), open_brief(new))
        && !py_eq(get(ob, "at"), get(nb, "at"))
    {
        return Err(PyStoreError::Conflict(format!(
            "{coll}/{id}: a briefing is already requested and waiting for the Architect"
        )));
    }
    Ok(())
}

/// `_restore_on_brief(data)`: a `brief_pending` item whose `brief` names a
/// `.md` path returns to `open`.
pub(crate) fn restore_on_brief(data: Vec<(String, PyValue)>) -> Vec<(String, PyValue)> {
    let d = PyValue::Dict(data);
    let pending = matches!(get(&d, "status"), Some(PyValue::Str(s)) if s == "brief_pending");
    let linked = matches!(get(&d, "brief"),
        Some(PyValue::Str(b)) if strip(b).ends_with(".md") && b.contains('/'));
    let PyValue::Dict(pairs) = d else {
        return Vec::new();
    };
    if pending && linked {
        with_key(&pairs, "status", PyValue::Str("open".into()))
    } else {
        pairs
    }
}
