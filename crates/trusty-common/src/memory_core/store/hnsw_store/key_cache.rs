//! The `vector_id → uuid` reverse map and tombstone set `search` resolves hits
//! against, cached between writes (#9141).
//!
//! Why: `HnswStore::search` rebuilt both from a full scan of `VECTOR_KEYS` and
//! `DELETED_VECTORS` on every call — one `String` allocation per drawer, every
//! recall, on a palace whose key set changes only when a drawer is written.
//! What: [`KeyCache`] holds the last [`KeyMap`] tagged with a generation number.
//! Every `HnswStore` write that commits a change to either table calls
//! [`KeyCache::invalidate`], which bumps a counter shared by every store open
//! on the same `redb::Database` — two handles over one palace file see each
//! other's writes, as they did when each search re-read the tables. `get`
//! rebuilds only when its map is older than that counter.
//! Test: `search_reads_vector_keys_once_until_a_store_write`,
//! `search_cache_follows_writes_from_every_store_on_the_file`.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};

use parking_lot::{Mutex, RwLock};
use redb::{Database, ReadableDatabase, ReadableTable, ReadableTableMetadata};

use super::Result;
use crate::memory_core::store::kg_store::{DELETED_VECTORS, VECTOR_KEYS};

/// One consistent read of `VECTOR_KEYS` (inverted) and `DELETED_VECTORS`.
pub(super) struct KeyMap {
    /// `vector_id → uuid`; its length is the live drawer count.
    pub(super) reverse: HashMap<u64, String>,
    /// Tombstoned `vector_id`s.
    pub(super) tombstones: HashSet<u64>,
}

impl KeyMap {
    /// Read both tables in one read transaction, so the pair is consistent.
    fn load(db: &Database) -> Result<Self> {
        let rtx = db.begin_read()?;
        let keys = rtx.open_table(VECTOR_KEYS)?;
        let mut reverse = HashMap::with_capacity(keys.len()? as usize);
        for entry in keys.iter()? {
            let (k, v) = entry?;
            reverse.insert(v.value(), k.value().to_string());
        }
        let dead = rtx.open_table(DELETED_VECTORS)?;
        let mut tombstones = HashSet::new();
        for entry in dead.iter()? {
            let (k, _) = entry?;
            tombstones.insert(k.value());
        }
        Ok(Self {
            reverse,
            tombstones,
        })
    }
}

/// Generation counters, one per open `Database`, shared by every store on it.
///
/// Why: a `Weak` keeps its allocation alive, so a live entry's address cannot
/// be reused by another `Database` — pointer equality identifies the file.
static GENERATIONS: Mutex<Vec<(Weak<Database>, Arc<AtomicU64>)>> = Mutex::new(Vec::new());

/// The write generation shared by every store opened on `db`.
fn shared_generation(db: &Arc<Database>) -> Arc<AtomicU64> {
    let mut all = GENERATIONS.lock();
    all.retain(|(weak, _)| weak.strong_count() > 0);
    if let Some((_, generation)) = all
        .iter()
        .find(|(weak, _)| std::ptr::eq(weak.as_ptr(), Arc::as_ptr(db)))
    {
        return Arc::clone(generation);
    }
    let generation = Arc::new(AtomicU64::new(0));
    all.push((Arc::downgrade(db), Arc::clone(&generation)));
    generation
}

/// The cached [`KeyMap`] of one store, and the write generation it reflects.
pub(super) struct KeyCache {
    generation: Arc<AtomicU64>,
    cached: RwLock<Option<(u64, Arc<KeyMap>)>>,
}

impl KeyCache {
    /// An empty cache for a store on `db`; the first `get` loads the map.
    pub(super) fn new(db: &Arc<Database>) -> Self {
        Self {
            generation: shared_generation(db),
            cached: RwLock::new(None),
        }
    }

    /// The current map, rebuilt from redb only when a write has landed since
    /// the cached one was read.
    ///
    /// What: the generation is read BEFORE the read transaction begins, so a
    /// map read from a snapshot older than a concurrent commit is tagged with
    /// the generation that commit's `invalidate` then moves past — the next
    /// call rebuilds. A newer map already stored is never replaced by an older.
    pub(super) fn get(&self, db: &Database) -> Result<Arc<KeyMap>> {
        let generation = self.generation.load(Ordering::Acquire);
        if let Some((seen, map)) = &*self.cached.read()
            && *seen == generation
        {
            return Ok(Arc::clone(map));
        }
        let map = Arc::new(KeyMap::load(db)?);
        let mut slot = self.cached.write();
        if slot.as_ref().is_none_or(|(seen, _)| *seen <= generation) {
            *slot = Some((generation, Arc::clone(&map)));
        }
        Ok(map)
    }

    /// Mark every store's cached map on this file stale. Call after the commit
    /// of any write to `VECTOR_KEYS` or `DELETED_VECTORS`.
    pub(super) fn invalidate(&self) {
        self.generation.fetch_add(1, Ordering::AcqRel);
    }
}
