//! Single-record hard-delete for managed sessions (#2012).
//!
//! Why: `decommission` stops the runtime and (when owned) removes the
//! workspace, but ALWAYS leaves a `Decommissioned` tombstone behind — an
//! operator with a mis-provisioned or duplicate record has no single-record
//! verb to drop it from the store outright. `tm session delete` fills that gap.
//! Extracted into its own file (mirroring `adopt.rs`/`decommission.rs`/`prune.rs`)
//! so [`super::prune`] — which already hosts the tombstone-compaction primitive
//! this delegates to — stays under the 500-SLOC production cap.
//! What: an inherent `impl SessionManager` block adding
//! [`SessionManager::delete_record`], which wraps
//! [`SessionManager::compact_record`](super::prune) with the SAME fail-closed
//! running-state guard `super::prune::is_running` enforces elsewhere.
//! Test: `delete_record_*` in `super::delete_tests`.

use super::manager::{ManagedError, SessionManager};
use super::prune::is_running;
use super::record::{ManagedSessionId, SessionRecord};

impl SessionManager {
    /// Hard-delete a single managed session RECORD (#2012).
    ///
    /// Why: distinct from `decommission` (stops the runtime and may remove the
    /// workspace, but always leaves a `Decommissioned` tombstone) — this
    /// permanently drops the record itself via the existing
    /// [`compact_record`](Self::compact_record) (tombstone-removal) primitive,
    /// guarded by the SAME fail-closed running-state check
    /// [`prune_managed`](Self::prune_managed) already enforces, so a live
    /// session can never be hard-deleted out from under its runtime by accident.
    ///
    /// SAFETY: this is a STORE-ONLY operation — it calls `compact_record`, which
    /// calls [`super::store::SessionStore::remove`], which only ever mutates the
    /// in-memory map and the persisted `sessions.json`. It never touches
    /// `workspace_path` on disk (unlike `decommission`, which may `remove_dir_all`
    /// an owned workspace). Deleting the record is deliberately NOT the same
    /// operation as tearing down the workspace; an operator who also wants the
    /// workspace gone should `decommission` first (or accept an orphaned
    /// directory, exactly as a stale `Decommissioned` tombstone would leave one).
    ///
    /// What: looks up the record (a missing id surfaces as
    /// [`ManagedError::SessionNotFound`]). When `force` is `false` (the default)
    /// and [`is_running`] reports the record's state as `Active`/`Provisioning`,
    /// returns [`ManagedError::InvalidState`] with an actionable message telling
    /// the operator to stop the session first or pass `--force` — no record is
    /// touched. Otherwise removes the record via `compact_record` and returns the
    /// pre-deletion [`SessionRecord`] snapshot (so callers can render its identity
    /// after it is gone from the store).
    /// Test: `delete_record_removes_from_store`,
    /// `delete_record_refuses_running_without_force`,
    /// `delete_record_force_bypasses_running_guard`,
    /// `delete_record_never_touches_workspace_dir` in `super::delete_tests`.
    pub async fn delete_record(
        &self,
        id: &ManagedSessionId,
        force: bool,
    ) -> Result<SessionRecord, ManagedError> {
        let record = self.get(id).await?;
        if !force && is_running(&record.state) {
            return Err(ManagedError::InvalidState(
                id.to_string(),
                format!(
                    "session is {} — stop it first with `tm session stop {id}` \
                     (or `tm session decommission {id}`), or pass --force to \
                     delete the record anyway",
                    record.state
                ),
            ));
        }
        self.compact_record(id).await?;
        Ok(record)
    }
}
