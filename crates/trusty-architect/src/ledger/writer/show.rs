//! The writer's read-only side: `show` and the CLI's result lines (#9758).
//!
//! Why: `tm dbatg show`, `add`, `update`, `close`, `batch` and `prune` must
//! print the prototype's exact stdout, from the same store the writer writes.
//! What: [`show`] renders the summary line or the whole store; [`minute`],
//! [`render_ids`] and [`render_prune`] format a clock reading or a
//! [`BatchResponse`]. Nothing here takes the lock or writes.
//! Test: `ledger_writer_matches_prototype_bytes`.

use std::fs;

use serde::Serialize;

use super::bad;
use crate::clock::Clock;
use crate::error::LedgerError;
use crate::ledger::model::{GoalStatus, ItemStatus, State};
use crate::ledger::ops::Now;
use crate::ledger::rpc::{BatchResponse, StoreRef};

/// `show`: the summary line (`brief`) or the whole store as the prototype's
/// `json.dumps(state, indent=1, ensure_ascii=False)`. Reads only.
///
/// # Errors
/// [`LedgerError::Invalid`] for a missing or malformed store.
pub fn show(store: &StoreRef, brief: bool) -> Result<String, LedgerError> {
    let paths = store.paths()?;
    let raw = fs::read_to_string(&paths.state).map_err(|_| bad("state.json missing; run init"))?;
    let st = State::from_json(&raw).map_err(|e| bad(format!("state.json malformed ({e})")))?;
    if brief {
        let s = summary(&st);
        return Ok(if s.is_empty() {
            "no open items".into()
        } else {
            s
        });
    }
    let mut out = Vec::new();
    let fmt = serde_json::ser::PrettyFormatter::with_indent(b" ");
    st.serialize(&mut serde_json::Serializer::with_formatter(&mut out, fmt))?;
    String::from_utf8(out).map_err(|e| bad(e.to_string()))
}

/// The prototype's `summary`: top active goal, open and answered item ids.
fn summary(st: &State) -> String {
    let mut goals: Vec<_> = st
        .goals
        .iter()
        .filter(|g| g.status == GoalStatus::Active)
        .collect();
    goals.sort_by_key(|g| g.rank);
    let ids = |s: ItemStatus| {
        st.items
            .iter()
            .filter(|i| i.status == s)
            .take(6)
            .map(|i| i.id.as_str())
            .collect::<Vec<_>>()
    };
    let mut parts = Vec::new();
    if let Some(g) = goals.first() {
        parts.push(format!(
            "TOP {} {}",
            g.id,
            g.title.chars().take(60).collect::<String>()
        ));
    }
    for (label, s) in [
        ("OPEN", ItemStatus::Open),
        ("ANSWERED", ItemStatus::Answered),
    ] {
        let found = ids(s);
        if !found.is_empty() {
            parts.push(format!("{label} {}", found.join(",")));
        }
    }
    parts.join("; ")
}

/// `clock`'s time as the prototype stamps it: `YYYY-MM-DDTHH:MMZ`.
pub fn minute(clock: &dyn Clock) -> String {
    Now::at(clock.now_unix()).minute
}

/// The event ids, as `add`, `update`, `close` and `batch` print them.
pub fn render_ids(resp: &BatchResponse) -> String {
    resp.events
        .iter()
        .map(|e| e.id.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// The `prune` line; `before` and `after` are `state.json` sizes.
pub fn render_prune(resp: &BatchResponse, dry_run: bool, before: u64, after: u64) -> String {
    let n = resp.archived.len();
    if n == 0 {
        "nothing to prune".into()
    } else if dry_run {
        format!("dry-run: would archive {n}: {}", resp.archived.join(", "))
    } else {
        format!("archived {n} items; state.json {before} -> {after} B")
    }
}
