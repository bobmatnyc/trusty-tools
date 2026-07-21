//! `SessionManager::numbered_snapshot` — the stable `tm ls` slot view (#3034).
//!
//! Why: `manager.rs` sits at the 500-SLOC production cap; this single method
//! is extracted here so it has somewhere to land without needing a broader
//! `manager.rs` refactor, mirroring the `reactivate.rs`/`reconcile.rs` split
//! precedent already used for `mark_reactivated`/`reconcile_on_boot`.
//! What: an inherent `impl SessionManager` block adding
//! [`SessionManager::numbered_snapshot`], which observes every record into
//! the manager's [`super::slots::SlotRegistry`] and returns the resulting
//! per-slot view.
//! Test: `numbered_snapshot_*` in `super::slots_tests`.

use std::collections::HashMap;

use super::manager::SessionManager;
use super::record::{ManagedSessionId, SessionRecord};
use super::slots::NumberedSlot;

impl SessionManager {
    /// Build the stable-numbered session view for `tm ls` (#3034).
    ///
    /// Why: two concurrent `tm ls` invocations must see the SAME number for
    /// the same session, and a deleted session's number must stay reserved
    /// (tombstoned, not reused) — neither guarantee is possible from a
    /// client-side per-fetch enumeration, so assignment lives here in the one
    /// daemon-owned [`super::slots::SlotRegistry`] every listing call reads
    /// and writes.
    /// What: observes every record in `records` (pass the FULL, unfiltered
    /// [`SessionManager::list`] result — a `--source-id`-filtered subset
    /// would leave excluded sessions unobserved and renumbered on their next
    /// global listing) into the registry, then returns one [`NumberedSlot`]
    /// per slot 1..=highest-assigned: `record: Some(_)` when still present in
    /// `records`, `record: None` once it disappears from the store via ANY
    /// removal path (hard-delete, decommission-reap, prune).
    /// Test: `numbered_snapshot_keeps_slot_stable_across_delete`,
    /// `numbered_snapshot_tombstones_deleted_slot`,
    /// `numbered_snapshot_never_reuses_a_deleted_slot`,
    /// `numbered_snapshot_concurrent_calls_agree_on_new_session_slot` in
    /// `super::slots_tests`.
    pub async fn numbered_snapshot(&self, records: &[SessionRecord]) -> Vec<NumberedSlot> {
        let mut reg = self.slots.write().await;
        for r in records {
            reg.observe(r.id, r.source_id.as_deref());
        }
        let live: HashMap<ManagedSessionId, SessionRecord> =
            records.iter().map(|r| (r.id, r.clone())).collect();
        let max = reg.max_slot();
        (1..=max)
            .map(|slot| NumberedSlot {
                slot,
                record: reg.id_at(slot).and_then(|id| live.get(&id).cloned()),
                source_id: reg.source_id_at(slot).map(str::to_owned),
            })
            .collect()
    }
}
