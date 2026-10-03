//! Who holds an index's mutual-exclusion permit (#8659).
//!
//! Why: a schema migration waiting on `index_semaphore` logged nothing about
//! what it waited for. An operator saw 25 minutes of silence and could not tell
//! a queued migration from a dead one.
//! What: a process-global map from index id to a static label naming the task
//! that holds the permit. A holder registers with [`mark_index_permit_holder`]
//! right after acquiring, and the returned guard clears the entry on drop, so
//! the label lives exactly as long as the permit.
//! Test: `a_waiting_migration_is_logged_and_reported_with_its_holder`.

use std::sync::OnceLock;

use dashmap::DashMap;

use crate::core::registry::IndexId;

static HOLDERS: OnceLock<DashMap<IndexId, &'static str>> = OnceLock::new();

fn holders() -> &'static DashMap<IndexId, &'static str> {
    HOLDERS.get_or_init(DashMap::new)
}

/// Clears the holder label for one index when dropped (#8659).
#[must_use = "the label is cleared when this guard drops"]
pub(crate) struct PermitHolderMark(IndexId);

impl Drop for PermitHolderMark {
    fn drop(&mut self) {
        holders().remove(&self.0);
    }
}

/// Record that `label`'s task now holds `id`'s permit (#8659).
pub(crate) fn mark_index_permit_holder(id: &IndexId, label: &'static str) -> PermitHolderMark {
    holders().insert(id.clone(), label);
    PermitHolderMark(id.clone())
}

/// The label of the task holding `id`'s permit, if one registered (#8659).
pub(crate) fn index_permit_holder(id: &IndexId) -> Option<&'static str> {
    holders().get(id).map(|r| *r)
}
