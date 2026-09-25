//! What a palace already holds from kuzu-memory (#277).
//!
//! Why: idempotency needs a record of what was imported, and a separate
//! "done" file could say a store was imported when a write in the middle had
//! failed. The drawers themselves are the ledger instead: each imported drawer
//! carries its identity tag, its source hash and the store it came from, so
//! the palace can never claim a memory it does not hold.
//! What: [`Ledger::from_drawers`] indexes `Memory.id` -> (drawer id, hash,
//! store), plus the drawers still carrying a pending marker;
//! [`Ledger::plan`] classifies a mapped memory.
//! Test: `ledger_plans_new_unchanged_changed`,
//! `moved_store_reimports_nothing_and_shared_ids_are_reported`.

use std::collections::HashMap;
use trusty_common::memory_core::palace::Drawer;
use uuid::Uuid;

use super::mapping::{
    MappedMemory, HASH_TAG_PREFIX, PENDING_TAG_PREFIX, SOURCE_TAG_PREFIX, STORE_TAG_PREFIX,
};

/// One drawer an earlier run imported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedDrawer {
    pub drawer_id: Uuid,
    /// The source `content_hash` recorded at import, when the tag is present.
    pub hash: Option<String>,
    /// The store directory recorded at import, when the tag is present.
    pub store: Option<String>,
}

/// How one memory relates to what the palace already holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryPlan {
    /// No drawer carries this memory's identity tag.
    New,
    /// A drawer carries it with the same hash.
    Unchanged(Uuid),
    /// A drawer carries it, but the memory changed in kuzu since.
    Changed(Uuid),
    /// An earlier run inserted this drawer but never stamped its identity.
    Resume(Uuid),
    /// Another store that still exists holds a different memory under the
    /// same `Memory.id`; skipped and reported, never written.
    SharedId(Uuid),
}

/// `Memory.id` -> imported drawer, for one palace.
#[derive(Debug, Default)]
pub struct Ledger {
    by_key: HashMap<String, ImportedDrawer>,
    pending: HashMap<String, Uuid>,
}

impl Ledger {
    /// Index every drawer carrying a `source:kuzu-memory/` or pending tag.
    ///
    /// When two drawers carry the same identity (which only a manual edit can
    /// produce), the first one wins, so a re-run updates one drawer rather
    /// than guessing between them.
    pub fn from_drawers<'a>(drawers: impl IntoIterator<Item = &'a Drawer>) -> Self {
        let mut ledger = Self::default();
        for drawer in drawers {
            let tag = |prefix: &str| {
                drawer
                    .tags
                    .iter()
                    .find_map(|t| t.strip_prefix(prefix))
                    .map(str::to_string)
            };
            match tag(SOURCE_TAG_PREFIX) {
                Some(key) => {
                    ledger
                        .by_key
                        .entry(format!("kuzu-memory/{key}"))
                        .or_insert(ImportedDrawer {
                            drawer_id: drawer.id,
                            hash: tag(HASH_TAG_PREFIX),
                            store: tag(STORE_TAG_PREFIX),
                        });
                }
                None => {
                    if let Some(id) = tag(PENDING_TAG_PREFIX) {
                        ledger.pending.entry(id).or_insert(drawer.id);
                    }
                }
            }
        }
        ledger
    }

    /// The drawer imported for `source_key`, if any.
    pub fn get(&self, source_key: &str) -> Option<&ImportedDrawer> {
        self.by_key.get(source_key)
    }

    /// Classify `memory` against this ledger.
    ///
    /// What (#277 H4): identity is `Memory.id` alone. Same hash is unchanged
    /// whichever store it came from, so a moved or copied store re-imports
    /// nothing. A different hash is a change, unless the drawer came from a
    /// different store that `store_is_live` says still exists: then two live
    /// stores disagree about one id, and the memory is [`MemoryPlan::SharedId`]
    /// rather than letting either store overwrite the other on alternate runs.
    /// Test: `ledger_plans_new_unchanged_changed`,
    /// `moved_store_reimports_nothing_and_shared_ids_are_reported`.
    pub fn plan(&self, memory: &MappedMemory, store_is_live: &dyn Fn(&str) -> bool) -> MemoryPlan {
        match self.get(&memory.source_key) {
            None => match self.pending.get(&memory.memory_id) {
                Some(id) => MemoryPlan::Resume(*id),
                None => MemoryPlan::New,
            },
            Some(d) if d.hash.as_deref() == Some(memory.hash.as_str()) => {
                MemoryPlan::Unchanged(d.drawer_id)
            }
            Some(d) => match d.store.as_deref() {
                Some(other) if other != memory.store && store_is_live(other) => {
                    MemoryPlan::SharedId(d.drawer_id)
                }
                _ => MemoryPlan::Changed(d.drawer_id),
            },
        }
    }

    /// Number of imported drawers indexed.
    pub fn len(&self) -> usize {
        self.by_key.len()
    }

    /// Whether no imported drawer is indexed.
    pub fn is_empty(&self) -> bool {
        self.by_key.is_empty()
    }
}
