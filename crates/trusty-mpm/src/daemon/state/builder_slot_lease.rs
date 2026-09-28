//! The daemon's side of the builder-slot leases that survive a restart (#8819).
//!
//! Why: a restart empties the delegation map, so the claim's `taken` set no
//! longer names the slots builders from before the restart still build in,
//! and the builder cap no longer counts them.
//! What: [`DaemonState::slot_lease_for`] builds the lease a handover records;
//! [`DaemonState::lost_lease_verdict`] judges a slot's recorded lease against
//! this daemon's records and the owner's process-table entry; and
//! [`DaemonState::restored_lease_holders`] lists every lease it judges held
//! across the whole pool root, for the cap and the census.
//! Test: `builder_slot_lease_tests`.

use std::path::Path;

use crate::core::builder_slot_pool::SlotPool;
use crate::core::builder_slot_pool::lease::{
    LeaseRecord, LeaseVerdict, OwnerProbe, SlotLease, judge_lease, slots_under,
};
use crate::core::session::SessionId;

use super::builder_slots::{BUILDER_LEASE_TTL_SECS, BuilderHolder, sort_holders};
use super::core::DaemonState;
use super::sessions::DELEGATION_RETENTION_SECS;

// #8819: a disk lease speaks only for a holder this daemon has no record of.
// That is sound because a terminal record outlives its lease's TTL, so a record
// evicted by the sweep never lets a finished lease read as live again.
const _: () = assert!(DELEGATION_RETENTION_SECS >= BUILDER_LEASE_TTL_SECS);

/// The agent name a restored lease is counted under (#8819).
///
/// Why: a lease file records the holder's `tool_use_id`, not its agent, so the
/// census names the lease for what it is rather than inventing an agent. Its
/// session is the lease's own when recorded, else the nil id.
pub const RESTORED_LEASE_AGENT: &str = "restored-lease";

impl DaemonState {
    /// The lease to record when `holder`'s dispatch is handed a slot.
    ///
    /// What: the dispatching session, its pid and that process's start time,
    /// each `None` when it cannot be learned, and the time of the grant.
    /// Test: `a_held_slot_survives_a_daemon_restart_8819`.
    pub(super) fn slot_lease_for(&self, holder: &str) -> SlotLease {
        let session = self
            .delegations
            .iter()
            .find(|e| e.value().tool_use_id.as_deref() == Some(holder))
            .map(|e| e.value().session);
        let owner_pid = session.and_then(|s| self.session(s)?.pid);
        let owner_start = owner_pid.and_then(|pid| {
            crate::session_manager::worktree_owner_gate::process_start_secs(pid).ok()
        });
        SlotLease {
            holder: holder.to_string(),
            owner_pid,
            owner_start,
            granted_at: chrono::Utc::now().timestamp(),
            session,
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
        self.judge_record(&pool.read_lease(index))
    }

    /// Every builder holding a slot: this daemon's live records plus every
    /// restored lease under `pool_root` (#8819).
    ///
    /// Test: `the_census_counts_restored_leases_once_and_skips_stale_ones_8819`.
    #[must_use]
    pub fn builder_slot_holders_with_pool_root(
        &self,
        exclude_tool_use_id: Option<&str>,
        pool_root: Option<&Path>,
    ) -> Vec<BuilderHolder> {
        let mut holders = self.builder_slot_holders(exclude_tool_use_id);
        if let Some(root) = pool_root {
            holders.extend(self.restored_lease_holders(root));
            sort_holders(&mut holders);
        }
        holders
    }

    /// One holder per slot under `pool_root` whose lease a previous daemon
    /// granted and [`judge_lease`] still reads as held or unverifiable (#8819).
    ///
    /// Why: the builder cap is machine-wide, and after a restart these builders
    /// still run. A lease whose holder this daemon has a record for reads as
    /// free here, so it is counted once, by its record. A directory that
    /// cannot be listed counts as one unverifiable holder (fail closed).
    /// What: agent [`RESTORED_LEASE_AGENT`], the lease's session (nil when
    /// unrecorded), and the seconds since the grant.
    /// Test: `the_census_counts_restored_leases_once_and_skips_stale_ones_8819`.
    #[must_use]
    pub fn restored_lease_holders(&self, pool_root: &Path) -> Vec<BuilderHolder> {
        let now = chrono::Utc::now().timestamp();
        let holder = |session: Option<SessionId>, since: i64| BuilderHolder {
            agent: RESTORED_LEASE_AGENT.to_string(),
            session: session.unwrap_or(SessionId(uuid::Uuid::nil())),
            elapsed_secs: (now - since).max(0),
        };
        slots_under(pool_root)
            .into_iter()
            .filter_map(|slot| {
                let (pool, index) = match slot {
                    Ok(slot) => slot,
                    Err(err) => {
                        tracing::warn!("builder slot lease scan could not list {err}");
                        return Some(holder(None, now));
                    }
                };
                let record = pool.read_lease(index);
                let verdict = self.judge_record(&record);
                if !verdict.blocks() {
                    return None;
                }
                // #8819 critic: name the file, so an operator can find it.
                tracing::info!(
                    lease = %pool.lease_path(index).display(),
                    "builder slot counted as a restored lease: {verdict:?}"
                );
                Some(match record {
                    LeaseRecord::Leased(lease) => holder(lease.session, lease.granted_at),
                    LeaseRecord::Served { served_at, .. } => holder(None, served_at),
                    LeaseRecord::Unreadable { since, .. } => holder(None, since.unwrap_or(now)),
                    LeaseRecord::Absent => holder(None, now),
                })
            })
            .collect()
    }

    /// [`judge_lease`] against this daemon's records and the process table.
    fn judge_record(&self, record: &LeaseRecord) -> LeaseVerdict {
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
            record,
            chrono::Utc::now().timestamp(),
            BUILDER_LEASE_TTL_SECS,
            &known,
            &owner,
        )
    }
}
