//! Recording `removed` for a tree gone from disk AND from git (#8994).
//!
//! Why: nothing else writes `removed`, so a tree reaped by
//! `agent_worktree_reap`, `merged_pr_reclaim` or `git worktree remove` stayed
//! live in the fold, and the `worktree_registry` doctor row and `tm worktrees`
//! only ever grew. The ruling is observe-and-reconcile: `tm worktrees` appends
//! the event; nothing is deleted and no reap or reclaim route changes.
//! What: [`reconcile_removed`]. A live tree earns `removed` only when BOTH hold:
//! its directory is absent (a `NotFound` stat, never any other stat error) and
//! its repository's `git worktree list` answered without listing it. A listed
//! `prunable` record counts as listed. A listing that cannot be trusted is
//! UNKNOWN, never "not listed": the repository is reported in
//! [`ReconcileReport::unlistable`] and its trees stay live. That covers a
//! repository directory that is gone (an unmounted volume or a moved checkout
//! looks the same), a git failure or timeout, and a listing that does not name
//! the repository itself (git walked up into an enclosing repository).
//! Test: `worktree_registry_row_drops_a_tree_reconciled_as_removed`,
//! `tm_worktrees_records_removed_for_a_tree_gone_from_disk_and_git`,
//! `reconcile_writes_nothing_for_a_gone_tree_git_still_lists`,
//! `reconcile_writes_nothing_when_the_repo_cannot_be_listed`,
//! `reconcile_writes_nothing_while_the_directory_exists`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::Serialize;

use super::fold::LedgerState;
use super::{EventKind, LedgerAppender, LedgerError, LedgerEvent, WorktreeLedger};
use crate::session_manager::worktree_registry::probe_registered_worktrees;

/// A repository whose worktree listing could not be trusted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UnlistableRepo {
    /// The repository the ledger names.
    pub repo: PathBuf,
    /// Why its listing was not used.
    pub reason: String,
}

/// Outcome of one [`reconcile_removed`] pass.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ReconcileReport {
    /// `removed` events appended.
    pub removed: usize,
    /// Trees gone from disk that git still lists (a `prunable` record).
    pub still_listed: usize,
    /// Trees gone from disk whose repository could not be listed.
    pub unknown: usize,
    /// The repositories behind [`Self::unknown`].
    pub unlistable: Vec<UnlistableRepo>,
}

impl ReconcileReport {
    /// Live trees gone from disk that this pass left live.
    pub fn missing(&self) -> usize {
        self.still_listed + self.unknown
    }
}

/// Append `removed` for every live tree gone from disk and from git.
///
/// Why: see the module doc.
/// What: groups the live trees whose directory is absent by repository, lists
/// each such repository once, and appends one `removed` per tree the listing
/// does not name. Paths are compared raw and with their deepest existing
/// ancestor canonicalized, so `/var` and `/private/var` spellings match. Git
/// runs only for a repository with a gone tree, and the ledger is opened only
/// when there is something to append.
/// Test: see the module doc.
pub fn reconcile_removed(
    ledger: &WorktreeLedger,
    state: &LedgerState,
) -> Result<ReconcileReport, LedgerError> {
    let mut gone: BTreeMap<&Path, Vec<&Path>> = BTreeMap::new();
    for wt in state.live.values() {
        if directory_gone(&wt.path) {
            gone.entry(wt.repo.as_path()).or_default().push(&wt.path);
        }
    }
    let mut report = ReconcileReport::default();
    let mut appender: Option<LedgerAppender> = None;
    for (repo, trees) in gone {
        let listed = match listed_keys(repo) {
            Ok(listed) => listed,
            Err(reason) => {
                report.unknown += trees.len();
                report.unlistable.push(UnlistableRepo {
                    repo: repo.to_path_buf(),
                    reason,
                });
                continue;
            }
        };
        for path in trees {
            if listed.contains(path) || listed.contains(&lenient_key(path)) {
                report.still_listed += 1;
                continue;
            }
            if appender.is_none() {
                appender = Some(ledger.open_appender()?);
            }
            if let Some(out) = appender.as_mut() {
                out.append(&LedgerEvent::now(path.to_path_buf(), EventKind::Removed))?;
                report.removed += 1;
            }
        }
    }
    Ok(report)
}

/// `true` only when a stat of `path` says it does not exist.
fn directory_gone(path: &Path) -> bool {
    matches!(std::fs::symlink_metadata(path), Err(e) if e.kind() == std::io::ErrorKind::NotFound)
}

/// Every path `repo`'s `git worktree list` names, or why it cannot be trusted.
fn listed_keys(repo: &Path) -> Result<BTreeSet<PathBuf>, String> {
    if directory_gone(repo) {
        return Err("repository directory is gone".to_string());
    }
    let listed =
        probe_registered_worktrees(repo).map_err(|e| format!("`git worktree list` failed: {e}"))?;
    let mut keys = BTreeSet::new();
    for wt in &listed {
        keys.insert(wt.path.clone());
        keys.insert(lenient_key(&wt.path));
    }
    if !keys.contains(repo) && !keys.contains(&lenient_key(repo)) {
        return Err("`git worktree list` does not list the repository itself".to_string());
    }
    Ok(keys)
}

/// `path` with its deepest existing ancestor canonicalized.
///
/// Why: a gone tree cannot be canonicalized, but git and the ledger may spell
/// its parent differently (macOS `/var` → `/private/var`).
fn lenient_key(path: &Path) -> PathBuf {
    let mut rest = Vec::new();
    let mut cur = path;
    loop {
        if let Ok(canon) = std::fs::canonicalize(cur) {
            return rest.iter().rev().fold(canon, |acc, name| acc.join(name));
        }
        match (cur.parent(), cur.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name.to_os_string());
                cur = parent;
            }
            _ => return path.to_path_buf(),
        }
    }
}
