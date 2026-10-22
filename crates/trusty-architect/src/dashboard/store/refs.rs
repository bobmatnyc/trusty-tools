//! Dangling references between live records (DOC-82 LDG-2.2.5).
//!
//! Why: ids recycle (LDG-2.2.4), so a record that names a retired id would
//! silently point at whatever reuses it. `fsck` must report every live
//! record that names an id no document holds (#10063 PR 3, Architect Dbb).
//! What: [`dangling`] reads the id collections ([`ID_COLLECTIONS`]) and, for
//! each live record (status not `done`, `wont_do` or `cancelled`, lifecycle
//! not `done` or `removed`), checks `goal` against the goal ids, and each
//! entry of `depends_on` (or `depends-on`, as `il-archive.py` reads it) and
//! of `links` (a string, or an object's `item`) against every id. Only a
//! bare id (`^[A-Z][0-9a-f]{2}$`) is a reference: free text, URLs and
//! qualified ids are not checked. When the id collections cannot be read
//! (a case variant or the other format, which the file checks accept), the
//! one line says the references were not checked, so `fsck` is never clean
//! without the check (#10063).
//! Test: `fsck_reports_dangling_links_depends_on_and_goal`,
//! `fsck_reports_references_not_checked_when_the_snapshot_fails`.

use std::collections::BTreeSet;

use serde_json::{Map, Value};

use super::super::ids::ID_COLLECTIONS;
use super::Store;
use crate::ledger::is_bare_id;

/// Statuses and lifecycles of a record that is no longer live.
const ENDED: [&str; 4] = ["done", "wont_do", "cancelled", "removed"];

/// One line per dangling reference, in collection and id order.
pub(super) fn dangling(store: &Store) -> Vec<String> {
    // #10063: a skipped check is a problem, never a clean result.
    let snap = match store.snapshot(&ID_COLLECTIONS) {
        Ok(snap) => snap,
        Err(e) => return vec![format!("references not checked: {e}")],
    };
    let all: BTreeSet<&str> = snap.values().flatten().map(|d| d.id.as_str()).collect();
    let goals: BTreeSet<&str> = snap
        .get("goals")
        .into_iter()
        .flatten()
        .map(|d| d.id.as_str())
        .collect();
    let mut out = Vec::new();
    for (coll, docs) in &snap {
        for d in docs.iter().filter(|d| live(&d.data)) {
            let mut miss = |at: String, r: &str, set: &BTreeSet<&str>, what: &str| {
                if is_bare_id(r) && !set.contains(r) {
                    out.push(format!(
                        "{coll}/{}: {at}: {r} is not {what} in the store",
                        d.id
                    ));
                }
            };
            if let Some(Value::String(g)) = d.data.get("goal") {
                miss("/goal".into(), g, &goals, "a goal id");
            }
            for key in ["depends_on", "depends-on"] {
                match d.data.get(key) {
                    Some(Value::String(r)) => miss(format!("/{key}"), r, &all, "an id"),
                    Some(Value::Array(rs)) => {
                        for (i, r) in rs.iter().enumerate() {
                            if let Value::String(r) = r {
                                miss(format!("/{key}/{i}"), r, &all, "an id");
                            }
                        }
                    }
                    _ => {}
                }
            }
            for (i, link) in links(&d.data).iter().enumerate() {
                match link {
                    Value::String(r) => miss(format!("/links/{i}"), r, &all, "an id"),
                    Value::Object(o) => {
                        if let Some(Value::String(r)) = o.get("item") {
                            miss(format!("/links/{i}/item"), r, &all, "an id");
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    out
}

fn links(data: &Map<String, Value>) -> &[Value] {
    match data.get("links") {
        Some(Value::Array(l)) => l,
        _ => &[],
    }
}

/// A record is live until its `status` or `lifecycle` ends it.
fn live(data: &Map<String, Value>) -> bool {
    ["status", "lifecycle"]
        .iter()
        .filter_map(|k| data.get(*k).and_then(Value::as_str))
        .all(|s| !ENDED.contains(&s))
}
