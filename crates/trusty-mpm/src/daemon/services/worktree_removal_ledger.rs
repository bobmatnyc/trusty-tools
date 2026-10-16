//! The daemon's worktree-removal recorder (#8994 S2c).
//!
//! Why: the merged-PR reclaim, the agent-worktree reap and session
//! decommission each remove trees; the ledger must hear about every one, and
//! only on evidence. One recorder per daemon keeps all three on the same
//! ledger file and the same write lock the hook ingest (S2a/S2b) holds, so a
//! reap and the `SessionEnd` reconcile never both append `removed`.
//! What: [`recorder_for`] builds the [`RemovalRecorder`] over
//! `<framework root>/worktrees.jsonl`; [`record_reclaimed`] feeds it the
//! merged-PR reclaim's `removed` list only; [`install`] hands it to the
//! session manager for decommission. The re-check itself is
//! [`crate::core::worktree_ledger::removal_evidence`]'s. Nothing here deletes.
//! Test: `daemon::services::worktree_removal_ledger::tests`.

use std::path::PathBuf;
use std::sync::Arc;

use crate::core::worktree_ledger::removal_evidence::{
    RemovalEvidence, RemovalRecorder, ledger_recorder,
};
use crate::core::worktree_ledger::{LEDGER_FILE, WorktreeLedger};
use crate::daemon::state::DaemonState;
use crate::session_manager::worktree_reclaim::ReclaimOutcome;

/// The recorder over this daemon's ledger, under the hook-ingest write lock.
///
/// Why: see the module doc.
/// What: a relative framework root has no ledger to write (the S2a ingest
/// refuses it the same way), so the recorder it yields appends nothing and
/// warns on every call; otherwise
/// [`ledger_recorder`] over `<root>/worktrees.jsonl` and
/// [`IngestTracker::write_lock`](super::worktree_ledger_ingest::IngestTracker).
/// Test: `a_relative_framework_root_records_nothing_and_warns`,
/// `merged_pr_reclaim_records_only_its_removed_list`.
pub(crate) fn recorder_for(state: &DaemonState) -> RemovalRecorder {
    recorder_at(state.framework_root(), state.worktree_ingest().write_lock())
}

/// [`recorder_for`] over an explicit framework root and lock.
fn recorder_at(root: &std::path::Path, lock: Arc<std::sync::Mutex<()>>) -> RemovalRecorder {
    let root = root.to_path_buf();
    if !root.is_absolute() {
        return Arc::new(move |route: &str, removed: &[PathBuf]| {
            tracing::warn!(
                route,
                root = %root.display(),
                count = removed.len(),
                "worktree ledger: no absolute framework root, so {route}'s removals are not \
                 recorded; `tm worktrees` reconciles them (#8994)"
            );
            Vec::new()
        });
    }
    ledger_recorder(WorktreeLedger::at(root.join(LEDGER_FILE)), lock)
}

/// Record the merged-PR reclaim's removals.
///
/// Why: `removed` is the engine's list of trees it removed AND found gone
/// (`worktree_reclaim_sweep`); `removal_failed` and `partially_removed` left
/// a directory behind and must keep the tree live, so they are never passed.
/// What: hands `outcome.removed` (empty in `Report` mode) to `recorder`.
/// Test: `merged_pr_reclaim_records_only_its_removed_list`.
pub(crate) fn record_reclaimed(
    recorder: &RemovalRecorder,
    outcome: &ReclaimOutcome,
) -> Vec<RemovalEvidence> {
    if outcome.removed.is_empty() {
        return Vec::new();
    }
    recorder("merged-PR reclaim", &outcome.removed)
}

/// Record one tree the agent-worktree reap removed.
///
/// Why: the reap runs on the async `SessionEnd` path, and the re-check runs
/// git; so it runs on the blocking pool, and a failed task is a `warn`, never
/// silence.
/// What: [`recorder_for`] on `path`, on a blocking thread.
/// Test: `session_end_reap_records_only_the_tree_it_removed`.
pub(crate) async fn record_reaped(state: &DaemonState, path: &std::path::Path) {
    let record = recorder_for(state);
    let paths = vec![path.to_path_buf()];
    let task = tokio::task::spawn_blocking(move || record("agent-worktree reap", &paths));
    if let Err(e) = task.await {
        tracing::warn!(
            path = %path.display(),
            "worktree ledger: the removal record task failed: {e}; `tm worktrees` reconciles \
             it (#8994)"
        );
    }
}

/// Give the daemon's session manager the recorder decommission uses.
///
/// Test: `decommission_records_removed_for_the_worktree_it_removed`.
pub(crate) async fn install(state: &Arc<DaemonState>) {
    let recorder = recorder_for(state);
    state
        .session_manager()
        .await
        .install_removal_recorder(recorder);
}

#[cfg(test)]
#[path = "worktree_removal_ledger_tests.rs"]
mod tests;
