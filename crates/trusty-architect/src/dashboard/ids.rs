//! Dashboard names and the recycled id space.
//!
//! Why: ids are a prefix letter plus two hex digits, 256 per prefix, so the
//! next id must come from the store itself and wrap past `ff` (LDG-2.2.3),
//! and done items must be retired before the space runs out (LDG-2.2.4).
//! Separately, every collection name and doc id becomes a path component, so
//! it must be path-safe (#10062 gap G2: that check is path safety only).
//! What: [`check_name`] (`^[A-Za-z0-9][A-Za-z0-9_.-]{0,79}$`), and over a
//! [`Snapshot`] of [`ID_COLLECTIONS`]: [`used_ids`], [`free_count`],
//! [`next_id`] (first free after the highest used, wrapping to the lowest
//! free; `None` when full) and [`retire_candidates`] (oldest `done` by
//! `done_at`, then `updated`, then `created`). The bare id grammar is
//! [`crate::ledger::is_bare_id`], defined once (LDG-1.1). Pure: no clock, no
//! filesystem.
//! Test: `ids_next_wraps_past_ff_to_lowest_free`,
//! `ids_retire_oldest_done_until_keep_free`, `ids_names_are_path_safe`.

use std::collections::BTreeSet;

use serde_json::Value;

use super::error::DashError;
use super::store::{Doc, Snapshot};
use crate::ledger::is_bare_id;
use crate::pystr::py_repr;

/// Collections whose doc ids share the per-prefix id space.
pub const ID_COLLECTIONS: [&str; 5] = ["items", "goals", "priorities", "sessions", "questions"];
/// Ids per prefix (`00`..`ff`).
pub const SPACE: usize = 256;
/// Free ids [`retire_candidates`] keeps by default (LDG-2.2.4).
pub const KEEP_FREE: usize = 64;

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

/// The id number of a bare id with this prefix.
fn number(id: &str, prefix: char) -> Option<u8> {
    let mut chars = id.chars();
    (is_bare_id(id) && chars.next() == Some(prefix))
        .then(|| u8::from_str_radix(chars.as_str(), 16).ok())
        .flatten()
}

fn id_docs<'a>(snap: &'a Snapshot) -> impl Iterator<Item = &'a Doc> + 'a {
    ID_COLLECTIONS.iter().filter_map(|c| snap.get(*c)).flatten()
}

/// The numbers in use for `prefix` across [`ID_COLLECTIONS`].
pub fn used_ids(snap: &Snapshot, prefix: char) -> BTreeSet<u8> {
    id_docs(snap)
        .filter_map(|d| number(&d.id, prefix))
        .collect()
}

/// Ids of `prefix` still free.
pub fn free_count(snap: &Snapshot, prefix: char) -> usize {
    SPACE - used_ids(snap, prefix).len()
}

/// The first free id after the highest used, wrapping past `ff` to the lowest
/// free; `None` when all 256 are used.
pub fn next_id(snap: &Snapshot, prefix: char) -> Option<String> {
    let used = used_ids(snap, prefix);
    let top = used.last().map_or(-1, |n| i32::from(*n));
    (1..=SPACE as i32)
        .map(|k| (top + k).rem_euclid(SPACE as i32))
        .find(|c| u8::try_from(*c).is_ok_and(|c| !used.contains(&c)))
        .map(|c| format!("{prefix}{c:02x}"))
}

/// The oldest `done` ids of `prefix` (by `done_at`, else `updated`, else
/// `created`, then number) to retire so at least `keep_free` stay free.
pub fn retire_candidates(snap: &Snapshot, prefix: char, keep_free: usize) -> Vec<String> {
    let need = keep_free.saturating_sub(free_count(snap, prefix));
    let mut done: Vec<(String, u8)> = id_docs(snap)
        .filter(|d| d.data.get("status").and_then(Value::as_str) == Some("done"))
        .filter_map(|d| {
            let when = ["done_at", "updated", "created"]
                .iter()
                .find_map(|k| d.data.get(*k).filter(|v| truthy(v)))
                .map_or_else(String::new, py_str);
            number(&d.id, prefix).map(|n| (when, n))
        })
        .collect();
    done.sort();
    done.into_iter()
        .take(need)
        .map(|(_, n)| format!("{prefix}{n:02x}"))
        .collect()
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

    #[test]
    fn ids_next_wraps_past_ff_to_lowest_free() {
        let empty = snap(&[]);
        assert_eq!(next_id(&empty, 'D').as_deref(), Some("D00"));
        let s = snap(&[
            ("D01", json!({})),
            ("D05", json!({})),
            ("Dzz", json!({})),
            ("G09", json!({})),
        ]);
        assert_eq!(next_id(&s, 'D').as_deref(), Some("D06"));
        assert_eq!(next_id(&s, 'G').as_deref(), Some("G0a"));
        assert_eq!(free_count(&s, 'D'), 254);
        let top = snap(&[("D00", json!({})), ("D01", json!({})), ("Dff", json!({}))]);
        assert_eq!(next_id(&top, 'D').as_deref(), Some("D02"));
        let all: Vec<(String, Value)> = (0..=255)
            .map(|n| (format!("T{n:02x}"), json!({})))
            .collect();
        let refs: Vec<(&str, Value)> = all.iter().map(|(i, v)| (i.as_str(), v.clone())).collect();
        assert_eq!(next_id(&snap(&refs), 'T'), None);
    }

    #[test]
    fn ids_retire_oldest_done_until_keep_free() {
        let mut docs: Vec<(String, Value)> = (0..200)
            .map(|n| (format!("A{n:02x}"), json!({"status": "open"})))
            .collect();
        docs[3].1 = json!({"status": "done", "done_at": "2026-10-03T00:00:00Z"});
        docs[1].1 = json!({"status": "done", "done_at": "2026-10-01T00:00:00Z"});
        docs[2].1 = json!({"status": "done", "done_at": "", "updated": "2026-10-02T00:00:00Z"});
        let refs: Vec<(&str, Value)> = docs.iter().map(|(i, v)| (i.as_str(), v.clone())).collect();
        let s = snap(&refs);
        assert_eq!(retire_candidates(&s, 'A', 58), vec!["A01", "A02"]);
        assert_eq!(
            retire_candidates(&s, 'A', KEEP_FREE),
            vec!["A01", "A02", "A03"]
        );
        assert!(retire_candidates(&s, 'A', 56).is_empty());
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
