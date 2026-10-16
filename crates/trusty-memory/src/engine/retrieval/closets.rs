//! The closet keyword index of a `PalaceHandle`.
//!
//! Why: split out of `handle.rs` to keep it under the 500-SLOC cap (#9278
//! T3c-1).
//! What: [`PalaceHandle::rebuild_closets`].
//! Test: `closet_updated_after_remember`.

use super::PalaceHandle;
use crate::engine::dream::extract_keywords;
use crate::engine::palace::Drawer;
use std::collections::HashMap;
use uuid::Uuid;

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
        let mut new_index: HashMap<String, Vec<Uuid>> = HashMap::new();
        for drawer in snapshot.iter() {
            for kw in extract_keywords(drawer.content()) {
                new_index.entry(kw).or_default().push(drawer.id);
            }
        }
        let mut closets = self.closets.write();
        *closets = new_index;
    }
}
