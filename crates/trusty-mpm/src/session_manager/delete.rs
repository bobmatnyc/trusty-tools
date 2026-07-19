//! Single-record soft-delete for managed sessions (#2012, `--deleted--` marker).
//!
//! Why: `decommission` stops the runtime and (when owned) removes the
//! workspace, but ALWAYS leaves a `Decommissioned` tombstone behind — an
//! operator with a mis-provisioned or duplicate record has no single-record
//! verb to mark it gone. `tm sessions delete` fills that gap. Rather than
//! silently dropping the record from the store (which made a deleted session
//! VANISH from the master list), delete now marks it
//! [`ManagedSessionState::Deleted`] — rendered `--deleted--` — so the record is
//! still tracked and visible, honouring the "fully-tracked lifecycle, no
//! fire-and-forget" project standard. Permanent removal from the store then
//! happens through the existing prune path (`tm sessions prune --state deleted`
//! / `--state all`). Extracted into its own file (mirroring
//! `adopt.rs`/`decommission.rs`/`prune.rs`) so [`super::prune`] stays under the
//! 500-SLOC production cap.
//! What: an inherent `impl SessionManager` block adding
//! [`SessionManager::delete_record`], guarded by the SAME fail-closed running
//! guard `super::prune::is_running` enforces elsewhere — a real tmux liveness
//! probe, not a persisted-state check (#2022).
//! Test: `delete_record_*` in `super::delete_tests`.

use chrono::Utc;

use super::manager::{ManagedError, SessionManager};
use super::prune::is_running;
use super::record::{ManagedSessionId, ManagedSessionState, SessionRecord};

impl SessionManager {
    /// Soft-delete a single managed session RECORD — mark it `--deleted--` (#2012).
    ///
    /// Why: distinct from `decommission` (stops the runtime and may remove the
    /// workspace, always leaving a `Decommissioned` tombstone) — this marks the
    /// record [`ManagedSessionState::Deleted`] (rendered `--deleted--`) so the
    /// operator's master list REFLECTS the deletion rather than the row
    /// silently vanishing. Guarded by the SAME fail-closed running-state check
    /// [`prune_managed`](Self::prune_managed) enforces, so a live session can
    /// never be deleted out from under its runtime by accident.
    ///
    /// SAFETY: this is a STORE-ONLY state transition — it re-`upsert`s the record
    /// with `state = Deleted`, mutating only the in-memory map and the persisted
    /// `sessions.json`. It never touches `workspace_path` on disk (unlike
    /// `decommission`, which may `remove_dir_all` an owned workspace). Marking
    /// the record deleted is deliberately NOT the same operation as tearing down
    /// the workspace; an operator who also wants the workspace gone should
    /// `decommission` first. To drop the tombstone from the store entirely, use
    /// `tm sessions prune --state deleted` (or `--state all`).
    ///
    /// What: looks up the record (a missing id surfaces as
    /// [`ManagedError::SessionNotFound`]). When `force` is `false` (the default)
    /// and [`is_running`] finds a LIVE tmux session backing the record (#2022 — a
    /// real probe, not the persisted `state` field), returns
    /// [`ManagedError::InvalidState`] with an actionable message telling the
    /// operator to stop the session first or pass `--force` — no record is
    /// touched. A record whose `state` still says `Active`/`Provisioning` but
    /// whose tmux session is actually gone is NOT running by this probe, so it
    /// deletes cleanly without `--force`. Otherwise transitions the record to
    /// `Deleted`, persists it, and returns the PRE-deletion [`SessionRecord`]
    /// snapshot (so callers can render the state it was in before deletion).
    /// Test: `delete_record_marks_deleted`,
    /// `delete_record_refuses_running_without_force`,
    /// `delete_record_force_bypasses_running_guard`,
    /// `delete_record_never_touches_workspace_dir`,
    /// `delete_record_stale_active_deletable_when_tmux_dead` (#2022) in
    /// `super::delete_tests`.
    pub async fn delete_record(
        &self,
        id: &ManagedSessionId,
        force: bool,
    ) -> Result<SessionRecord, ManagedError> {
        let record = self.get(id).await?;
        if !force && is_running(&record, self.tmux.as_ref()) {
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
        // Soft-delete: mark the record `Deleted` (rendered `--deleted--`) and
        // persist. The record stays in the store — visible in the master list —
        // rather than being dropped outright.
        let mut updated = record.clone();
        updated.state = ManagedSessionState::Deleted;
        updated.last_activity_at = Some(Utc::now());
        self.store.write().await.upsert(updated).await?;
        Ok(record)
    }
}
