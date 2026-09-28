//! The daemon's side of the builder-slot leases that survive a restart (#8819).
//!
//! Why: a restart empties the delegation map, so the claim's `taken` set no
//! longer names the slots builders from before the restart still build in.
//! What: [`DaemonState::slot_lease_for`] builds the lease a handover records;
//! [`DaemonState::lost_lease_verdict`] judges a slot's recorded lease against
//! this daemon's records and the owner's process-table entry.
//! Test: `builder_slot_lease_tests`.

use crate::core::builder_slot_pool::SlotPool;
use crate::core::builder_slot_pool::lease::{LeaseVerdict, OwnerProbe, SlotLease, judge_lease};

use super::builder_slots::BUILDER_LEASE_TTL_SECS;
use super::core::DaemonState;
use super::sessions::DELEGATION_RETENTION_SECS;

// #8819: a disk lease speaks only for a holder this daemon has no record of.
// That is sound because a terminal record outlives its lease's TTL, so a record
// evicted by the sweep never lets a finished lease read as live again.
const _: () = assert!(DELEGATION_RETENTION_SECS >= BUILDER_LEASE_TTL_SECS);

impl DaemonState {
    /// The lease to record when `holder`'s dispatch is handed a slot.
    ///
    /// What: the dispatching session's pid and that process's start time,
    /// each `None` when it cannot be learned, and the time of the grant.
    /// Test: `a_held_slot_survives_a_daemon_restart_8819`.
    pub(super) fn slot_lease_for(&self, holder: &str) -> SlotLease {
        let owner_pid = self
            .delegations
            .iter()
            .find(|e| e.value().tool_use_id.as_deref() == Some(holder))
            .and_then(|e| self.session(e.value().session)?.pid);
        let owner_start = owner_pid.and_then(|pid| {
            crate::session_manager::worktree_owner_gate::process_start_secs(pid).ok()
        });
        SlotLease {
            holder: holder.to_string(),
            owner_pid,
            owner_start,
            granted_at: chrono::Utc::now().timestamp(),
        }
    }

    /// Is slot `index` held by a lease granted before this daemon started?
    ///
    /// Why: see the module doc. A holder this daemon has a record for is
    /// decided by that record, as before #8819.
    /// What: [`judge_lease`] over the slot's recorded lease, with the owner
    /// probed through `kill(pid, 0)` and the process table.
    /// Test: `a_held_slot_survives_a_daemon_restart_8819`,
    /// `unverifiable_lease_evidence_keeps_its_slot_after_a_restart_8819`,
    /// `a_stale_lease_frees_its_slot_after_a_restart_8819`.
    pub(super) fn lost_lease_verdict(&self, pool: &SlotPool, index: u32) -> LeaseVerdict {
        let known = |holder: &str| {
            self.delegations
                .iter()
                .any(|e| e.value().tool_use_id.as_deref() == Some(holder))
        };
        let owner = |pid: u32| {
            if crate::core::process::is_process_alive(pid) {
                OwnerProbe::Running(
                    crate::session_manager::worktree_owner_gate::process_start_secs(pid),
                )
            } else {
                OwnerProbe::Gone
            }
        };
        judge_lease(
            &pool.read_lease(index),
            chrono::Utc::now().timestamp(),
            BUILDER_LEASE_TTL_SECS,
            &known,
            &owner,
        )
    }
}
