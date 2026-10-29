//! The closet keyword index of a `PalaceHandle`.
//!
//! Why: split out of `handle.rs` to keep it under the 500-SLOC cap (#9278
//! T3c-1).
//! What: [`PalaceHandle::rebuild_closets`].
//! Test: `closet_updated_after_remember`, `closet_boost_survives_reopen_without_a_write`.

use super::PalaceHandle;
use crate::engine::dream::build_closet_index;
use crate::engine::palace::Drawer;

impl PalaceHandle {
    /// Rebuild the closet keyword index from the current in-memory drawer table.
    ///
    /// Why: Keep the closet index current after every write so L2 tag-boosting
    /// works without waiting for a dream cycle.
    /// What: Rebuilds keyword -> Vec<drawer_id> map by tokenizing each drawer's
    /// content via `extract_keywords` (whitespace + stop-word filter).
    /// Test: `closet_updated_after_remember`.
    pub(crate) fn rebuild_closets(&self) {
        let snapshot: Vec<Drawer> = self.drawers.read().clone();
        let new_index = build_closet_index(&snapshot);
        let mut closets = self.closets.write();
        *closets = new_index;
    }
}
