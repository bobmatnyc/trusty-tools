//! `ilio collapse`: archive done items, then check nothing live was lost.
//!
//! Why: a session clears its context only after the ledger holds everything it
//! still needs. `collapse` prunes through the ledger lock, then proves the
//! invariant: every id that was open, running, answered or owed before is still
//! live or archived, and the session record is not older than the last applied
//! turn. A failed invariant refuses the clear.
//! What: [`collapse`] returns `(ok, message)`; on success and not `--dry-run` it
//! writes `<ILIO_DIR>/epoch.json {epoch, at, rows}` atomically.
//! Test: `golden_steps_match_python_byte_for_byte` (collapse steps and `epoch.json`),
//! `collapse_refuses_a_stale_session_record`, `collapse_dry_run_writes_no_epoch`.

use super::{Lib, ledger_ids, table_rows};
use crate::il::Ledger;
use crate::il::error::IlResult;
use crate::il::json::{Json, obj};
use crate::il::ops::Ctx;
use crate::il::store::{atomic_write, load_state};
use crate::il::time::fmt_min;
use crate::pystr::py_repr;
use std::collections::BTreeSet;
use std::path::Path;

fn live(st: &Json) -> (BTreeSet<String>, BTreeSet<String>) {
    let ids = ledger_ids(st)
        .into_iter()
        .filter(|(_, _, s, _)| s != "done")
        .map(|(i, ..)| i)
        .collect();
    let owed = st
        .get("session")
        .map_or(&[][..], |s| s.arr("owed"))
        .iter()
        .map(|o| format!("{} = {}", o.s("check"), o.s("expect")))
        .collect();
    (ids, owed)
}

/// Ids with a valid (non-orphan) archive line.
fn archived_ids(ledger: &Ledger, v: Option<i64>) -> BTreeSet<String> {
    let text = std::fs::read_to_string(&ledger.paths.archive).unwrap_or_default();
    text.lines()
        .filter_map(|l| Json::parse(l).ok())
        .filter(|r| !matches!((v, r.i("cv")), (Some(v), Some(cv)) if cv > v))
        .filter_map(|r| r.get("item").map(|i| i.s("id").to_owned()))
        .collect()
}

fn list(items: &BTreeSet<String>) -> String {
    format!(
        "[{}]",
        items
            .iter()
            .map(|i| py_repr(i))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

/// `collapse`: `(ok, message)`.
///
/// # Errors
/// The prune's failures (lock, corrupt state, failed write); an invariant failure is `Ok((false, ..))`.
pub fn collapse(
    ledger: &Ledger,
    ctx: &Ctx,
    ilio_dir: Option<&Path>,
    dry_run: bool,
) -> IlResult<(bool, String)> {
    let (before_ids, before_owed) = live(&load_state(&ledger.paths)?);
    if !dry_run {
        ledger.prune(ctx, false, 24.0, false)?;
    }
    let st = load_state(&ledger.paths)?;
    let (after_ids, after_owed) = live(&st);
    let applied = ilio_dir
        .and_then(|d| std::fs::read_to_string(d.join("last-apply.json")).ok())
        .and_then(|t| Json::parse(&t).ok())
        .and_then(|j| j.i("epoch"))
        .unwrap_or(0);
    let written = st
        .get("session")
        .and_then(|s| s.i("written_epoch"))
        .unwrap_or(0);
    if applied != 0 && written < applied {
        return Ok((false, "INVARIANT FAILED: session record older than the last applied turn; write session next, then retry; clear refused".to_owned()));
    }
    let gone: BTreeSet<String> = before_ids.difference(&after_ids).cloned().collect();
    let lost: BTreeSet<String> = if gone.is_empty() {
        gone
    } else {
        let archived = archived_ids(ledger, st.i("v"));
        gone.into_iter().filter(|i| !archived.contains(i)).collect()
    };
    let lost_owed: BTreeSet<String> = before_owed.difference(&after_owed).cloned().collect();
    if !lost.is_empty() || !lost_owed.is_empty() {
        return Ok((
            false,
            format!(
                "INVARIANT FAILED: lost ids {} lost owed {}; clear refused",
                list(&lost),
                list(&lost_owed)
            ),
        ));
    }
    if let (false, Some(dir)) = (dry_run, ilio_dir) {
        std::fs::create_dir_all(dir).map_err(crate::il::error::IlError::io("create", dir))?;
        let ep = dir.join("epoch.json");
        let n = std::fs::read_to_string(&ep)
            .ok()
            .and_then(|t| Json::parse(&t).ok())
            .and_then(|j| j.i("epoch"))
            .unwrap_or(0)
            + 1;
        let rows = table_rows(&st, &Lib::load(Some(dir))).len() as i64;
        let doc = obj([
            ("epoch", n.into()),
            ("at", fmt_min(ctx.epoch()).into()),
            ("rows", rows.into()),
        ]);
        atomic_write(&ep, doc.dump_spaced().as_bytes(), false)?;
    }
    Ok((
        true,
        format!(
            "invariant ok: {} live ids kept, {} owed kept",
            before_ids.len(),
            before_owed.len()
        ),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_renders_like_a_python_list() {
        let s: BTreeSet<String> = ["b".to_owned(), "a".to_owned()].into_iter().collect();
        assert_eq!(list(&s), "['a', 'b']");
        assert_eq!(list(&BTreeSet::new()), "[]");
    }
}
