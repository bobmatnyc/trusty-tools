//! What a palace already holds from kuzu-memory (#277).
//!
//! Why: idempotency needs a record of what was imported, and a separate
//! "done" file could say a store was imported when a write in the middle had
//! failed. The drawers themselves are the ledger instead: each imported drawer
//! carries its identity tag and its source hash, so the palace can never claim
//! a memory it does not hold.
//! What: [`Ledger::from_drawers`] indexes identity tag -> (drawer id, hash);
//! [`Ledger::plan`] classifies a mapped memory as new, unchanged or changed.
//! Test: `ledger_plans_new_unchanged_changed`.

use std::collections::HashMap;
use trusty_common::memory_core::palace::Drawer;
use uuid::Uuid;

use super::mapping::{MappedMemory, HASH_TAG_PREFIX, SOURCE_TAG_PREFIX};

/// One drawer an earlier run imported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedDrawer {
    pub drawer_id: Uuid,
    /// The source `content_hash` recorded at import, when the tag is present.
    pub hash: Option<String>,
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
}

/// Identity tag -> imported drawer, for one palace.
#[derive(Debug, Default)]
pub struct Ledger {
    by_key: HashMap<String, ImportedDrawer>,
}

impl Ledger {
    /// Index every drawer carrying a `source:kuzu-memory/` tag.
    ///
    /// When two drawers carry the same identity (which only a manual edit can
    /// produce), the first one wins, so a re-run updates one drawer rather
    /// than guessing between them.
    pub fn from_drawers<'a>(drawers: impl IntoIterator<Item = &'a Drawer>) -> Self {
        let mut by_key = HashMap::new();
        for drawer in drawers {
            let Some(key) = drawer
                .tags
                .iter()
                .find_map(|t| t.strip_prefix(SOURCE_TAG_PREFIX))
            else {
                continue;
            };
            let hash = drawer
                .tags
                .iter()
                .find_map(|t| t.strip_prefix(HASH_TAG_PREFIX))
                .map(str::to_string);
            by_key
                .entry(format!("kuzu-memory/{key}"))
                .or_insert(ImportedDrawer {
                    drawer_id: drawer.id,
                    hash,
                });
        }
        Self { by_key }
    }

    /// The drawer imported for `source_key`, if any.
    pub fn get(&self, source_key: &str) -> Option<&ImportedDrawer> {
        self.by_key.get(source_key)
    }

    /// Classify `memory` against this ledger.
    ///
    /// Test: `ledger_plans_new_unchanged_changed`.
    pub fn plan(&self, memory: &MappedMemory) -> MemoryPlan {
        match self.get(&memory.source_key) {
            None => MemoryPlan::New,
            Some(d) if d.hash.as_deref() == Some(memory.hash.as_str()) => {
                MemoryPlan::Unchanged(d.drawer_id)
            }
            Some(d) => MemoryPlan::Changed(d.drawer_id),
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
