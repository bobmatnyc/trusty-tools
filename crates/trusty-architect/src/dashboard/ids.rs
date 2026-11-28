//! Dashboard names and the recycled id space.
//!
//! Why: ids are a prefix letter plus two base-34 digits, 1,156 per prefix
//! (`00` is never issued), so the next id must come from the store itself
//! and wrap past `zz` (LDG-2.2.3), and finished items must be retired before
//! the space runs out (LDG-2.2.4). Separately, every collection name and doc
//! id becomes a path component, so it must be path-safe (#10062 gap G2: that
//! check is path safety only).
//! What: [`check_name`] (`^[A-Za-z0-9][A-Za-z0-9_.-]{0,79}$`), and over a
//! [`Snapshot`] of [`ID_COLLECTIONS`]: [`used_ids`], [`free_count`],
//! [`next_id`] and [`retire_candidates`]. These four are the
//! `dashboard_derive.py` rules of [`super::pyderive`], run on the serde
//! snapshot through [`py_snap`]; this module holds no id rule of its own
//! (#12029 P2d). A hex-era id (`D9c`, `Dff`) is two valid base-34 digits, so
//! it stays used and is never issued again. Pure: no clock, no filesystem.
//! Test: `ids_next_wraps_past_zz_to_lowest_free`, `ids_next_counts_media_ids`,
//! `ids_retire_follows_the_derive_rules`, `ids_names_are_path_safe`,
//! `ids_verb_issues_base34_and_never_collides_with_old_hex_ids`.

use std::collections::BTreeSet;

use serde_json::Value;

use super::error::DashError;
use super::pyderive::ids as rules;
use super::pyderive::{PySnap, ids::Retire};
use super::store::{Doc, Snapshot};
use crate::FakeClock;
use crate::ids36;
use crate::pyjson_load::{PyInt, PyValue};
use crate::pystr::py_repr;
use crate::tasks::store_read::PyDoc;

/// Collections whose doc ids share the per-prefix id space.
// #10063 PR 2: `media` as in `dashboard_derive.py:17`.
pub const ID_COLLECTIONS: [&str; 6] = rules::ID_COLLECTIONS;
/// Ids per prefix (`00`..`zz`, base 34).
// #12029: base 34, as `dashboard_derive.SPACE` (was 256 hex ids).
pub const SPACE: usize = ids36::SPACE;
/// Free ids [`retire_candidates`] keeps by default: the 25% floor (LDG-2.2.4).
// #12029: `dashboard_derive.KEEP_FREE` (was 64 of 256).
pub const KEEP_FREE: usize = SPACE / 4;

/// `value` when it is a path-safe collection name or doc id.
///
/// # Errors
/// [`DashError::Invalid`] naming `kind` and the rule, in the prototype's words.
pub fn check_name<'a>(kind: &str, value: &'a str) -> Result<&'a str, DashError> {
    let mut chars = value.chars();
    let safe = |c: char| c.is_ascii_alphanumeric() || "_.-".contains(c);
    let ok = chars.next().is_some_and(|c| c.is_ascii_alphanumeric())
        && chars.all(safe)
        && value.len() <= 80;
    if ok {
        return Ok(value);
    }
    Err(DashError::Invalid(format!(
        "invalid {kind} {}: use letters, digits, '_', '-', '.', max 80, no leading '.'",
        py_repr(value)
    )))
}

/// A serde JSON value as the [`PyValue`] `json.load` gives (keys in map order).
pub(crate) fn py_value(v: &Value) -> PyValue {
    match v {
        Value::Null => PyValue::Null,
        Value::Bool(b) => PyValue::Bool(*b),
        Value::Number(n) => match (n.as_i64(), n.as_u64()) {
            (Some(i), _) => PyValue::Int(PyInt::Small(i)),
            (None, Some(u)) => PyValue::Int(PyInt::Big(u.to_string())),
            _ => PyValue::Float(n.as_f64().unwrap_or(f64::NAN)),
        },
        Value::String(s) => PyValue::Str(s.clone()),
        Value::Array(a) => PyValue::List(a.iter().map(py_value).collect()),
        Value::Object(o) => py_map(o),
    }
}

/// A document body as a [`PyValue`] dict.
pub(crate) fn py_map(m: &serde_json::Map<String, Value>) -> PyValue {
    PyValue::Dict(m.iter().map(|(k, v)| (k.clone(), py_value(v))).collect())
}

/// A serde [`Doc`] as a [`PyDoc`]; #12029 P2e: the tally and the poller read through it.
pub(crate) fn py_doc(d: &Doc) -> PyDoc {
    PyDoc {
        id: d.id.clone(),
        data: py_map(&d.data),
        version: d.version,
    }
}

/// Why: the live verbs read a serde [`Snapshot`]; the id rules read Python values.
/// What: the snapshot's [`ID_COLLECTIONS`] as a [`PySnap`]; other collections are left out.
/// Test: `ids_next_wraps_past_zz_to_lowest_free`.
pub(crate) fn py_snap(snap: &Snapshot) -> PySnap {
    ID_COLLECTIONS
        .iter()
        .filter_map(|c| snap.get(*c).map(|docs| ((*c).to_owned(), docs)))
        .map(|(c, docs)| (c, docs.iter().map(py_doc).collect()))
        .collect()
}

/// The numbers in use for `prefix` across [`ID_COLLECTIONS`].
pub fn used_ids(snap: &Snapshot, prefix: char) -> BTreeSet<usize> {
    rules::used_ids(&py_snap(snap), &prefix.to_string())
        .into_iter()
        .filter_map(|n| usize::try_from(n).ok())
        .collect()
}

/// Ids of `prefix` still free (`00` is never free).
pub fn free_count(snap: &Snapshot, prefix: char) -> usize {
    usize::try_from(rules::free_count(&py_snap(snap), &prefix.to_string())).unwrap_or(0)
}

/// The lowest free id after the highest used, wrapping past `zz` to the
/// lowest free and never `00`; `None` when all 1,155 are used.
pub fn next_id(snap: &Snapshot, prefix: char) -> Option<String> {
    rules::next_id(&py_snap(snap), &prefix.to_string(), None)
}

/// Why: done items must be retired before the space runs out (LDG-2.2.4).
/// What: the oldest finished ids of `prefix` to retire so at least
/// `keep_free` stay free, by the `dashboard_derive.retire_candidates` rules
/// at `now` (Unix seconds): `done`, answered and relayed, `cancelled` and
/// `wont_do` items; never one finished or updated in the last 24 h, never
/// one with `keep` true, never `00` for the floor.
/// Test: `ids_retire_follows_the_derive_rules`, `ids_verb_retires_by_the_derive_rules`.
pub fn retire_candidates(snap: &Snapshot, prefix: char, keep_free: usize, now: f64) -> Vec<String> {
    let r = Retire {
        keep_free: i64::try_from(keep_free).unwrap_or(i64::MAX),
        age_days: None,
        now: Some(now),
    };
    // `now` is given, so the clock is never read.
    let clock = FakeClock::new(now);
    rules::retire_candidates(&py_snap(snap), &prefix.to_string(), r, &clock)
}

/// Python truthiness of a JSON value.
pub(crate) fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// Python's `str()` of a JSON scalar (containers use the JSON text, which is
/// enough to order them).
pub(crate) fn py_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Null => "None".into(),
        other => super::canonical::python_dumps(other, false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 2026-10-21T10:00:00Z.
    const NOW: f64 = 1_792_576_800.0;

    fn snap(ids: &[(&str, Value)]) -> Snapshot {
        let mut s = Snapshot::new();
        for (id, data) in ids {
            let coll = if id.starts_with('G') {
                "goals"
            } else {
                "items"
            };
            s.entry(coll.to_owned()).or_default().push(Doc {
                id: (*id).to_owned(),
                version: 1,
                data: data.as_object().cloned().unwrap_or_default(),
            });
        }
        s
    }

    fn b34(prefix: char, n: usize) -> String {
        format!("{prefix}{}", ids36::enc(n as i64).unwrap_or_default())
    }

    // #12029: base 34, wrapping past `zz`, never `00`.
    #[test]
    fn ids_next_wraps_past_zz_to_lowest_free() {
        let empty = snap(&[]);
        assert_eq!(next_id(&empty, 'D').as_deref(), Some("D01"));
        let s = snap(&[
            ("D01", json!({})),
            ("D05", json!({})),
            ("D0f", json!({})),
            ("G09", json!({})),
        ]);
        assert_eq!(next_id(&s, 'D').as_deref(), Some("D0g"));
        assert_eq!(next_id(&s, 'G').as_deref(), Some("G0a"));
        assert_eq!(free_count(&s, 'D'), SPACE - 1 - 3);
        let top = snap(&[("D00", json!({})), ("D01", json!({})), ("Dzz", json!({}))]);
        assert_eq!(next_id(&top, 'D').as_deref(), Some("D02"));
        assert_eq!(free_count(&top, 'D'), SPACE - 1 - 2, "00 is never free");
        let all: Vec<(String, Value)> = (1..SPACE).map(|n| (b34('T', n), json!({}))).collect();
        let refs: Vec<(&str, Value)> = all.iter().map(|(i, v)| (i.as_str(), v.clone())).collect();
        assert_eq!(next_id(&snap(&refs), 'T'), None);
        assert_eq!(used_ids(&snap(&refs), 'T').len(), SPACE - 1);
    }

    // #10063 PR 2: `media` docs use the same per-prefix id space.
    #[test]
    fn ids_next_counts_media_ids() {
        let mut s = Snapshot::new();
        s.entry("media".to_owned()).or_default().push(Doc {
            id: "B01".to_owned(),
            version: 1,
            data: serde_json::Map::new(),
        });
        assert_eq!(next_id(&s, 'B').as_deref(), Some("B02"));
        assert_eq!(free_count(&s, 'B'), SPACE - 2);
    }

    // #12029: the floor, the 24 h guard, `keep` and the finished kinds.
    #[test]
    fn ids_retire_follows_the_derive_rules() {
        let old = "2026-10-01T00:00:00Z";
        let mut docs: Vec<(String, Value)> = (1..=880)
            .map(|n| (b34('A', n), json!({"status": "open"})))
            .collect();
        docs[2].1 = json!({"status": "done", "done_at": "2026-10-03T00:00:00Z"});
        docs[0].1 = json!({"status": "done", "done_at": old});
        docs[1].1 =
            json!({"status": "answered", "relayed": "x", "relayed_at": "2026-10-02T00:00:00Z"});
        docs[3].1 = json!({"status": "done", "done_at": old, "keep": true});
        docs[4].1 = json!({"status": "done", "done_at": "2026-10-21T09:00:00Z"});
        docs[5].1 = json!({"status": "cancelled", "cancelled_at": "2026-10-04T00:00:00Z"});
        let refs: Vec<(&str, Value)> = docs.iter().map(|(i, v)| (i.as_str(), v.clone())).collect();
        let s = snap(&refs);
        assert_eq!(free_count(&s, 'A'), 275);
        assert_eq!(retire_candidates(&s, 'A', 277, NOW), vec!["A01", "A02"]);
        assert_eq!(
            retire_candidates(&s, 'A', KEEP_FREE, NOW),
            vec!["A01", "A02", "A03", "A06"]
        );
        assert!(retire_candidates(&s, 'A', 275, NOW).is_empty());
    }

    #[test]
    fn ids_names_are_path_safe() {
        for good in ["D01", "a.b", "0x", "a_b-c", &"x".repeat(80)] {
            assert!(check_name("doc id", good).is_ok(), "{good}");
        }
        for bad in ["", ".hidden", "../x", "a/b", "a b", "é", &"x".repeat(81)] {
            assert!(check_name("doc id", bad).is_err(), "{bad}");
        }
        let msg = check_name("doc id", "../x").map_err(|e| e.to_string());
        assert_eq!(
            msg,
            Err(
                "invalid doc id '../x': use letters, digits, '_', '-', '.', max 80, no leading '.'"
                    .into()
            )
        );
    }
}
