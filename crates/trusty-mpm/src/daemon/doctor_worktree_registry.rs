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
//! `worktree_registry_row_warns_on_malformed_lines`.

use std::path::Path;

use crate::core::doctor::{CheckStatus, DoctorCheck};
use crate::core::worktree_ledger::WorktreeLedger;
use crate::core::worktree_ledger::fold::{fold, gib};

/// The `tm doctor` row name.
pub(crate) const CHECK_NAME: &str = "worktree_registry";

/// The `worktree_registry` row for the ledger under `home`.
///
/// Why: see the module doc.
/// What: absent ledger → `Ok` naming `tm worktrees` as the backfill; an
/// unreadable one → `Unknown`; malformed lines → `Warn`; otherwise `Ok` with
/// the total and the per-project count and GiB. Unmeasured trees are counted
/// and named, never sized.
/// Test: see the module doc.
pub(crate) fn check_worktree_registry(home: &Path) -> DoctorCheck {
    let ledger = WorktreeLedger::under_home(home);
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
