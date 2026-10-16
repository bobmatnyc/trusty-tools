//! A decommission's worktree removal, recorded in the worktree ledger
//! (#8994 S2c).
//!
//! Why: decommission removes a session's in-project worktree or owned clone,
//! and the ledger kept it live until `tm worktrees` reconciled. The removal
//! is decided inside `decommission_with_root_checked_unrecorded`
//! (`decommission.rs`, at its SLOC cap) — `remove_in_project_worktree` and
//! `remove_owned_workspace`, then the registry prune for an owned clone — so
//! the record has to come after that whole body, which is this wrapper.
//! What: [`SessionManager::install_removal_recorder`] (the daemon installs
//! one; no other process does, so no other decommission writes the ledger)
//! and `decommission_with_root_checked`, which reads the record's workspace
//! path first, runs the unrecorded body, and hands the path to the recorder
//! only when the report says the workspace was removed. The recorder re-checks
//! disk and git before it appends anything.
//! Test: `session_manager::decommission_ledger::tests`.

use std::path::Path;

use super::decommission_force::{DecommissionReport, ProvisioningDirt};
use super::manager::{ManagedError, SessionManager};
use super::record::ManagedSessionId;
use crate::core::worktree_ledger::removal_evidence::RemovalRecorder;

impl SessionManager {
    /// Install the recorder a decommission's removal is reported to.
    ///
    /// What: set-once; `false` when one was already installed.
    /// Test: `decommission_records_removed_for_the_worktree_it_removed`.
    pub(crate) fn install_removal_recorder(&self, recorder: RemovalRecorder) -> bool {
        self.removal_recorder.set(recorder).is_ok()
    }

    /// `decommission_with_root_checked_unrecorded`, then the ledger record.
    ///
    /// Why: see the module doc.
    /// What: as the unrecorded body; on `Ok` with `workspace_removed`, the
    /// workspace path read before the teardown goes to the installed recorder
    /// on a blocking thread. A recorder task that fails is logged at `warn`;
    /// it never turns the decommission into an error.
    /// Test: `decommission_records_removed_for_the_worktree_it_removed`,
    /// `decommission_that_kept_the_worktree_records_nothing`.
    pub(super) async fn decommission_with_root_checked(
        &self,
        id: &ManagedSessionId,
        managed_root: &Path,
        caller: Option<ManagedSessionId>,
        check_foreign_claim: bool,
        dirt_policy: ProvisioningDirt,
    ) -> Result<DecommissionReport, ManagedError> {
        let workspace = self.get(id).await.ok().and_then(|r| r.workspace_path);
        let report = self
            .decommission_with_root_checked_unrecorded(
                id,
                managed_root,
                caller,
                check_foreign_claim,
                dirt_policy,
            )
            .await?;
        if let (true, Some(ws), Some(record)) = (
            report.workspace_removed,
            workspace,
            self.removal_recorder.get().cloned(),
        ) {
            let paths = vec![ws.clone()];
            let task = tokio::task::spawn_blocking(move || record("session decommission", &paths));
            if let Err(e) = task.await {
                tracing::warn!(
                    workspace = %ws.display(),
                    "worktree ledger: the removal record task failed: {e}; `tm worktrees` \
                     reconciles it (#8994)"
                );
            }
        }
        Ok(report)
    }
}

#[cfg(test)]
#[path = "decommission_ledger_tests.rs"]
mod tests;
