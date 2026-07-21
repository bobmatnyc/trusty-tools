//! In-memory, daemon-lifetime session slot numbering (issue #3034).
//!
//! Why: `tm ls` used to renumber sessions on every fetch — a plain `i + 1`
//! over whatever array the daemon happened to return. Deleting a session
//! shifted every subsequent row's displayed number, so an operator (or PM
//! agent) holding a number captured from an earlier listing could act on the
//! WRONG session. Bob's fix requirement: a session's displayed number is
//! assigned once and never reused or reshuffled for the life of the daemon
//! process; a deleted session's slot stays reserved and renders as a
//! tombstone instead of being handed to the next session; numbering resets
//! only when the daemon restarts.
//! What: [`SlotRegistry`] assigns a monotonically increasing `u32` to each
//! [`ManagedSessionId`] the first time it is observed via
//! [`SlotRegistry::observe`], and never reassigns or reuses a number for the
//! registry's lifetime. It is deliberately NOT persisted to disk — the
//! registry lives only inside the in-memory [`super::manager::SessionManager`]
//! that owns it, so a fresh daemon process starts with a fresh, empty
//! registry. That alone satisfies "reset numbering on restart"; no explicit
//! reset logic is needed. [`NumberedSlot`] is the per-slot view
//! [`super::manager::SessionManager::numbered_snapshot`] returns: `Some`
//! record for a live session, `None` for a slot whose session has since
//! disappeared from the store (hard-deleted, decommission-reaped, or pruned —
//! any removal path, not just the explicit delete verb).
//! Test: `slot_registry_*` below cover assignment stability, no-reuse, and
//! the source_id snapshot used to filter tombstones by project.

use std::collections::{BTreeMap, HashMap};

use super::record::{ManagedSessionId, SessionRecord};

/// Last-known metadata for one assigned slot, retained even after the
/// session's record is gone so a tombstone row can still be attributed to its
/// project for `--source-id` filtering.
#[derive(Debug, Clone)]
struct SlotMeta {
    id: ManagedSessionId,
    source_id: Option<String>,
}

/// Maps session ids to stable, daemon-lifetime slot numbers.
///
/// Why: two concurrent `tm ls` invocations (or two redraws of the same
/// interactive picker) must agree on every session's number — a purely
/// client-side per-fetch enumeration cannot give that guarantee. Owning the
/// assignment here, in the one daemon-side registry every listing call reads
/// and writes, is what makes the numbering authoritative.
/// What: `next` is the next never-used slot number (starts at 1 — slot 0 is
/// never assigned, so it doubles as an unambiguous "no slot" sentinel for
/// callers that default-construct a summary); `id_to_slot` is the reverse
/// index; `slots` retains metadata for every slot ever assigned, in slot
/// order.
/// Test: `slot_registry_assigns_stable_increasing_numbers`,
/// `slot_registry_never_reuses_a_number`,
/// `slot_registry_refreshes_source_id_while_live`.
#[derive(Debug)]
pub struct SlotRegistry {
    next: u32,
    id_to_slot: HashMap<ManagedSessionId, u32>,
    slots: BTreeMap<u32, SlotMeta>,
}

impl SlotRegistry {
    /// Construct an empty registry — the daemon-restart reset point (#3034).
    pub fn new() -> Self {
        Self {
            next: 1,
            id_to_slot: HashMap::new(),
            slots: BTreeMap::new(),
        }
    }

    /// Assign (or return the existing) stable slot number for `id`.
    ///
    /// Why: the single mutation point — every other method is a pure lookup.
    /// Refreshing `source_id` on every observation (rather than only at first
    /// assignment) keeps a legacy record's initially-`None` source_id current
    /// once it is set, without ever changing the assigned slot number.
    /// What: returns the previously-assigned slot when `id` is already known;
    /// otherwise assigns `next`, increments it, and records `id`/`source_id`.
    /// Test: `slot_registry_assigns_stable_increasing_numbers`,
    /// `slot_registry_refreshes_source_id_while_live`.
    pub fn observe(&mut self, id: ManagedSessionId, source_id: Option<&str>) -> u32 {
        if let Some(&slot) = self.id_to_slot.get(&id) {
            if let Some(meta) = self.slots.get_mut(&slot) {
                meta.source_id = source_id.map(str::to_owned);
            }
            return slot;
        }
        let slot = self.next;
        self.next += 1;
        self.id_to_slot.insert(id, slot);
        self.slots.insert(
            slot,
            SlotMeta {
                id,
                source_id: source_id.map(str::to_owned),
            },
        );
        slot
    }

    /// The highest slot number ever assigned, or 0 when none has been.
    pub fn max_slot(&self) -> u32 {
        self.next.saturating_sub(1)
    }

    /// The session id last observed at `slot`, if any was ever assigned there.
    pub fn id_at(&self, slot: u32) -> Option<ManagedSessionId> {
        self.slots.get(&slot).map(|m| m.id)
    }

    /// The last-known `source_id` recorded for `slot`, if any.
    pub fn source_id_at(&self, slot: u32) -> Option<&str> {
        self.slots.get(&slot).and_then(|m| m.source_id.as_deref())
    }
}

impl Default for SlotRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// One row of the daemon's stable-numbered session view (#3034).
///
/// Why: [`super::manager::SessionManager::numbered_snapshot`] returns one of
/// these per slot 1..=max so the HTTP layer can render either the live
/// summary or a `-- deleted --` tombstone without needing its own copy of the
/// registry's bookkeeping.
/// What: `record` is `Some` while the session is still present in the store,
/// `None` once it disappears (any removal path); `source_id` is the
/// registry's last-known value, used to filter tombstone rows by project even
/// though the record itself is gone.
/// Test: covered by `numbered_snapshot_*` in `session_manager::tests`.
#[derive(Debug, Clone)]
pub struct NumberedSlot {
    /// Stable, daemon-lifetime slot number (1-based).
    pub slot: u32,
    /// The live record, or `None` for a tombstoned slot.
    pub record: Option<SessionRecord>,
    /// Last-known `source_id`, retained for filtering even after deletion.
    pub source_id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn id() -> ManagedSessionId {
        ManagedSessionId::from(Uuid::new_v4())
    }

    #[test]
    fn slot_registry_assigns_stable_increasing_numbers() {
        let mut reg = SlotRegistry::new();
        let a = id();
        let b = id();
        let slot_a = reg.observe(a, Some("owner/repo"));
        let slot_b = reg.observe(b, None);
        assert_eq!(slot_a, 1);
        assert_eq!(slot_b, 2);
        // Re-observing returns the SAME number, never a fresh one.
        assert_eq!(reg.observe(a, Some("owner/repo")), 1);
        assert_eq!(reg.max_slot(), 2);
    }

    #[test]
    fn slot_registry_never_reuses_a_number() {
        let mut reg = SlotRegistry::new();
        let a = id();
        let b = id();
        let c = id();
        assert_eq!(reg.observe(a, None), 1);
        assert_eq!(reg.observe(b, None), 2);
        // `a`'s slot is never handed to a session observed after it, even
        // though nothing here ever "deletes" `a` from the registry (deletion
        // is a store-membership concept the registry itself is agnostic to —
        // see `numbered_snapshot`).
        assert_eq!(reg.observe(c, None), 3);
        assert_eq!(reg.id_at(1), Some(a));
    }

    #[test]
    fn slot_registry_refreshes_source_id_while_live() {
        let mut reg = SlotRegistry::new();
        let a = id();
        reg.observe(a, None);
        assert_eq!(reg.source_id_at(1), None);
        reg.observe(a, Some("owner/repo"));
        assert_eq!(reg.source_id_at(1), Some("owner/repo"));
    }

    #[test]
    fn slot_registry_unassigned_slot_and_max_are_empty() {
        let reg = SlotRegistry::new();
        assert_eq!(reg.max_slot(), 0);
        assert_eq!(reg.id_at(1), None);
        assert_eq!(reg.source_id_at(1), None);
    }
}
