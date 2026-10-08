//! Registering worktrees that predate the ledger (#8994).
//!
//! Why: the ledger starts empty on a machine that already holds dozens of
//! worktrees; without a backfill the first `tm worktrees` would report none of
//! them (issue closure condition 4).
//! What: [`backfill_checkouts`] asks `git worktree list` (through the crate's
//! one parser, `list_registered_worktrees`) for each checkout and appends one
//! event per tree the ledger does not already hold live — `created` for a tree
//! tm manages, `observed` for a Claude Code agent-isolation tree. It never
//! appends `removed`: a vanished tree is [`super::reconcile`]'s.
//! [`registered_checkouts`] supplies the checkouts from the project registry.
//! Test: `backfill_twice_appends_no_duplicate_events`,
//! `backfill_observes_agent_isolation_trees`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use super::fold::fold;
use super::{
    EventKind, LedgerAppender, LedgerError, LedgerEvent, Origin, WorktreeLedger, ledger_key,
};
use crate::session_manager::worktree_ownership::is_harness_agent_worktree;
use crate::session_manager::worktree_registry::list_registered_worktrees;

/// Outcome of one backfill pass.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BackfillReport {
    /// `created` events appended (tm-managed trees).
    pub created: usize,
    /// `observed` events appended (agent-isolation trees).
    pub observed: usize,
    /// Trees the ledger already held live; nothing appended.
    pub already_recorded: usize,
    /// Checkouts git could not list; their trees are not in this pass.
    pub unanswered: Vec<PathBuf>,
}

/// Append an event for every listed worktree the ledger does not hold live.
///
/// Why: idempotence is the contract — the backfill runs on every
/// `tm worktrees`, so a second pass over the same trees must append nothing.
/// What: folds the ledger once, then for each checkout lists its worktrees and
/// skips the main checkout, bare and prunable records, and any path already
/// live (including one appended earlier in this same pass, so two registered
/// projects sharing a repository record each tree once). The ledger is opened
/// only when there is something to append. An unlistable checkout lands in
/// [`BackfillReport::unanswered`].
/// Test: `backfill_twice_appends_no_duplicate_events`,
/// `backfill_observes_agent_isolation_trees`,
/// `backfill_skips_a_tree_provisioning_already_recorded`.
pub fn backfill_checkouts(
    ledger: &WorktreeLedger,
    checkouts: &[PathBuf],
) -> Result<BackfillReport, LedgerError> {
    let mut known: BTreeSet<PathBuf> = fold(&ledger.read()?.events).live.into_keys().collect();
    let mut report = BackfillReport::default();
    let mut appender: Option<LedgerAppender> = None;
    for checkout in checkouts {
        let Some(listed) = list_registered_worktrees(checkout) else {
            report.unanswered.push(checkout.clone());
            continue;
        };
        let repo = listed
            .iter()
            .find(|w| w.is_main)
            .map_or_else(|| ledger_key(checkout), |w| ledger_key(&w.path));
        for wt in listed {
            if wt.bare || wt.is_main || wt.prunable {
                continue;
            }
            let key = ledger_key(&wt.path);
            if !known.insert(key.clone()) {
                report.already_recorded += 1;
                continue;
            }
            let kind = if is_harness_agent_worktree(&key) {
                report.observed += 1;
                EventKind::Observed {
                    repo: repo.clone(),
                    branch: wt.branch,
                    origin: Origin::AgentIsolation,
                }
            } else {
                report.created += 1;
                EventKind::Created {
                    repo: repo.clone(),
                    branch: wt.branch,
                    origin: Origin::Backfill,
                    session: None,
                }
            };
            if appender.is_none() {
                appender = Some(ledger.open_appender()?);
            }
            if let Some(out) = appender.as_mut() {
                out.append(&LedgerEvent::now(key, kind))?;
            }
        }
    }
    Ok(report)
}

/// Every registered project's local checkout, from the registry at
/// `registry_dir`.
///
/// Why: the issue scopes the backfill to "each registered project"; the
/// registry is the list of them.
/// What: loads the registry read-only and maps each project through
/// `local_checkout_for`, dropping projects with no derivable checkout or whose
/// checkout directory is absent. Duplicates are removed.
/// Test: exercised by `tm_worktrees_json_reports_count_and_gib_per_project`
/// (an empty registry) and the backfill unit tests (explicit checkouts).
pub async fn registered_checkouts(
    registry_dir: &Path,
) -> Result<Vec<PathBuf>, crate::project::ProjectStoreError> {
    let projects = crate::project::ProjectRegistry::load(registry_dir)
        .await?
        .list()
        .await?;
    let set: BTreeSet<PathBuf> = projects
        .iter()
        .filter_map(crate::project::resolver::local_checkout_for)
        .filter(|p| p.is_dir())
        .collect();
    Ok(set.into_iter().collect())
}
