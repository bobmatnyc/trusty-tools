//! `tm doctor` row `worktree_registry`: count and GiB per project, read from
//! the worktree ledger alone (#8994).
//!
//! Why: the `worktrees` and `worktree_disk` rows derive their answers from git
//! and a filesystem walk. This row reports what the ledger records, so it costs
//! one file read and shows exactly what reclaim will later act on.
//! What: [`check_worktree_registry`] folds `~/.trusty-mpm/worktrees.jsonl` and
//! prints one `<project> N (X.XX GiB)` entry per project. It never runs git and
//! never walks a worktree: the sizes are the last `measured` events.
//! Test: `worktree_registry_row_reports_count_and_gib_from_the_ledger_alone`,
//! `worktree_registry_row_with_no_ledger_is_ok_and_names_the_backfill`,
//! `worktree_registry_row_warns_on_malformed_lines`,
//! `worktree_registry_row_drops_a_tree_reconciled_as_removed`,
//! `worktree_registry_row_warns_on_a_hook_ingest_failure`.

use std::path::Path;

use crate::core::doctor::{CheckStatus, DoctorCheck};
use crate::core::worktree_ledger::WorktreeLedger;
use crate::core::worktree_ledger::fold::{fold, gib};
use crate::core::worktree_ledger::hook_ingest::read_ingest_failure;

/// The `tm doctor` row name.
pub(crate) const CHECK_NAME: &str = "worktree_registry";

/// The `worktree_registry` row for the ledger under `home`.
///
/// Why: see the module doc.
/// What: absent ledger → `Ok` naming `tm worktrees` as the backfill; an
/// unreadable one → `Unknown`; malformed lines → `Warn`; otherwise `Ok` with
/// the total and the per-project count and GiB. Unmeasured trees are counted
/// and named, never sized. A hook-ingest failure record beside the ledger
/// turns `Ok` into `Warn` and is appended to the message (#8994 S2a).
/// Test: see the module doc.
pub(crate) fn check_worktree_registry(home: &Path) -> DoctorCheck {
    let ledger = WorktreeLedger::under_home(home);
    let row = ledger_row(&ledger);
    // #8994 S2a: a hook-ingest write failure is never silent — the daemon
    // records it beside the ledger and this row reports it.
    let failure = match read_ingest_failure(&ledger) {
        Ok(None) => return row,
        Ok(Some(f)) => format!(
            "hook ingest failed to write the ledger {} time(s), last at {}: {}",
            f.failures,
            f.ts.to_rfc3339(),
            f.error
        ),
        Err(e) => format!("hook-ingest failure record unreadable: {e}"),
    };
    let status = match row.status {
        CheckStatus::Ok => CheckStatus::Warn,
        other => other,
    };
    let message = format!(
        "{} — {failure}; `tm worktrees` backfills what was missed",
        row.message
    );
    DoctorCheck::new(CHECK_NAME, status, message)
}

/// The row from the ledger alone, before the hook-ingest failure record.
fn ledger_row(ledger: &WorktreeLedger) -> DoctorCheck {
    let read = match ledger.read() {
        Ok(read) => read,
        Err(e) => return DoctorCheck::new(CHECK_NAME, CheckStatus::Unknown, e.to_string()),
    };
    if !read.present {
        return DoctorCheck::new(
            CHECK_NAME,
            CheckStatus::Ok,
            format!(
                "no worktree ledger at {} yet — `tm worktrees` backfills it",
                ledger.path().display()
            ),
        );
    }
    let projects = fold(&read.events).by_project();
    let total: usize = projects.iter().map(|p| p.count).sum();
    let bytes: u64 = projects.iter().map(|p| p.bytes).sum();
    let unmeasured: usize = projects.iter().map(|p| p.unmeasured).sum();
    let per_project: Vec<String> = projects
        .iter()
        .map(|p| format!("{} {} ({:.2} GiB)", p.repo.display(), p.count, p.gib))
        .collect();
    let mut message = format!(
        "{total} worktree(s) across {} project(s), {:.2} GiB measured",
        projects.len(),
        gib(bytes)
    );
    if unmeasured > 0 {
        message.push_str(&format!(", {unmeasured} unmeasured"));
    }
    if !per_project.is_empty() {
        message.push_str(": ");
        message.push_str(&per_project.join("; "));
    }
    if read.malformed > 0 {
        message.push_str(&format!(
            " — {} malformed ledger line(s) skipped",
            read.malformed
        ));
        return DoctorCheck::new(CHECK_NAME, CheckStatus::Warn, message);
    }
    DoctorCheck::new(CHECK_NAME, CheckStatus::Ok, message)
}

#[cfg(test)]
#[path = "doctor_worktree_registry_tests.rs"]
mod tests;
