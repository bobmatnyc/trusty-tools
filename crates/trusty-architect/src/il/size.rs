//! The size bars of `state.json` and the report that names what to shorten.
//!
//! Why: `state.json` over 32 KB logs a `size_warning`, and a write that leaves it
//! over 64 KB (and larger than before) is refused after one prune. Both messages
//! carry `size_report`, so its text is part of the log and the error contract.
//! What: [`SIZE_WARN`], [`SIZE_REFUSE`], [`size_report`] and [`largest`], with
//! `datg.py`'s sort orders (ties broken by name, descending).
//! Test: `size_report_names_collections_field_and_largest`.

use super::json::Json;
use std::collections::BTreeMap;

/// Bytes above which a crossing is logged.
pub const SIZE_WARN: usize = 32 * 1024;
/// Bytes above which a growing write is refused.
pub const SIZE_REFUSE: usize = 64 * 1024;

fn bytes(o: &Json) -> usize {
    o.dump_spaced().len()
}

/// `_largest`: the `n` biggest goals, items and tasks as `id (N B)`.
pub fn largest(st: &Json, n: usize) -> String {
    let mut objs: Vec<(usize, String)> = ["goals", "items", "tasks"]
        .iter()
        .flat_map(|k| st.arr(k))
        .map(|o| {
            (
                bytes(o),
                o.get("id").map_or_else(|| "?".to_owned(), Json::py_str),
            )
        })
        .collect();
    objs.sort_by(|a, b| b.cmp(a));
    objs.iter()
        .take(n)
        .map(|(b, i)| format!("{i} ({b} B)"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// `size_report`: the biggest collections with their status mix, the biggest field of the
/// biggest one, the biggest session field, and the largest items.
pub fn size_report(st: &Json, n: usize) -> String {
    let mut cols: Vec<(usize, &str)> = ["goals", "items", "tasks"]
        .into_iter()
        .map(|k| (st.get(k).map_or(2, bytes), k))
        .collect();
    cols.sort_by(|a, b| b.cmp(a));
    let mut parts = Vec::new();
    for (size, k) in cols.iter().take(2) {
        let rows = st.arr(k);
        let mut mix: BTreeMap<String, usize> = BTreeMap::new();
        for o in rows {
            *mix.entry(o.get("status").map_or_else(|| "?".to_owned(), Json::py_str))
                .or_default() += 1;
        }
        let mix = mix
            .iter()
            .map(|(s, v)| format!("{v} {s}"))
            .collect::<Vec<_>>()
            .join(", ");
        parts.push(format!("{k} {size} B ({} entries: {mix})", rows.len()));
    }
    let top = cols[0].1;
    let mut fields: Vec<(&str, usize)> = Vec::new();
    for o in st.arr(top) {
        for (f, v) in o.entries() {
            match fields.iter_mut().find(|(n, _)| n == f) {
                Some(slot) => slot.1 += bytes(v),
                None => fields.push((f, bytes(v))),
            }
        }
    }
    if let Some((f, size)) = first_max(&fields) {
        parts.push(format!("biggest field {top}[].{f} {size} B"));
    }
    let ses: Vec<(&str, usize)> = st
        .get("session")
        .map(|s| {
            s.entries()
                .iter()
                .map(|(f, v)| (f.as_str(), bytes(v)))
                .collect()
        })
        .unwrap_or_default();
    if let Some((f, size)) = first_max(&ses) {
        parts.push(format!("session.{f} {size} B"));
    }
    format!("{}; largest: {}", parts.join("; "), largest(st, n))
}

/// Python `max(items, key=size)`: the first maximal entry.
fn first_max<'a>(v: &[(&'a str, usize)]) -> Option<(&'a str, usize)> {
    v.iter().fold(None, |best, &(f, s)| match best {
        Some((_, bs)) if bs >= s => best,
        _ => Some((f, s)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn size_report_names_collections_field_and_largest() {
        let st = Json::parse(
            r#"{"goals":[{"id":"g1","status":"active","title":"x"}],"items":[],"tasks":[{"id":"t1","status":"open","title":"aaaaaaaaaaaa"},{"id":"t2","status":"done","title":"b"}],"session":{"task":"abc","next":"abcdef"}}"#,
        )
        .unwrap();
        let r = size_report(&st, 2);
        assert_eq!(
            r,
            "tasks 103 B (2 entries: 1 done, 1 open); goals 48 B (1 entries: 1 active); biggest field tasks[].title 17 B; session.next 8 B; largest: t1 (55 B), g1 (46 B)"
        );
    }
}
