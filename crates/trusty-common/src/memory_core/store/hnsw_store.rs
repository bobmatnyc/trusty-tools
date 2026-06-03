//! Pure-Rust HNSW vector store backed by redb (issue #50, Phase 3).
//!
//! Why: `UsearchStore` depends on the C++ `usearch` library via FFI, which
//! pulls a native build-time toolchain into every downstream consumer and
//! prevents pure-Rust cross-compiles. `hnsw_rs` 0.3 is a pure-Rust HNSW
//! implementation; pairing it with the existing redb knowledge-graph
//! database lets us persist raw vectors (postcard-encoded `Vec<f32>`) in a
//! table and rebuild the in-memory graph on palace open. Doing so also
//! eliminates the JSON `key_map` sidecar that `UsearchStore` carries: the
//! UUID → vector_id mapping now lives in a redb table.
//! What: `HnswStore` owns an `Arc<redb::Database>` plus an
//! `Hnsw<f32, DistCosine>` rebuilt from the `VECTORS` redb table on `open`.
//! `upsert` writes both to redb (`VECTORS` + `VECTOR_KEYS`) and inserts into
//! the in-memory index. `delete` writes a tombstone to `DELETED_VECTORS`
//! (the in-memory `hnsw_rs` graph does not support removal); searches
//! filter tombstoned ids. `compact_orphans` scans `VECTORS` for entries
//! that no longer have a `VECTOR_KEYS` mapping and removes them.
//! Test: See module-level tests covering upsert+search round-trip,
//! tombstoned deletes, hydration on reopen, and orphan compaction.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use hnsw_rs::prelude::{DistCosine, Hnsw};
use parking_lot::RwLock;
use redb::{Database, ReadableDatabase, ReadableTable, ReadableTableMetadata};
use thiserror::Error;

use crate::memory_core::store::kg_store::{DELETED_VECTORS, VECTOR_KEYS, VECTORS};

/// Default HNSW connectivity. Maps to `max_nb_connection` in `hnsw_rs`.
///
/// Why: 16 is the recommended value from the original HNSW paper for
/// high-dimensional dense embeddings (matches usearch's default too).
const HNSW_MAX_NB_CONNECTION: usize = 16;

/// Default `ef_construction` for graph build quality.
///
/// Why: 200 is the standard "good quality" value from the HNSW paper.
const HNSW_EF_CONSTRUCTION: usize = 200;

/// Default maximum number of layers in the HNSW graph.
///
/// Why: 16 layers comfortably holds tens of millions of vectors; the
/// `hnsw_rs` implementation caps the effective ceiling at `NB_LAYER_MAX`
/// internally so picking a generous value is safe.
const HNSW_MAX_LAYER: usize = 16;

/// Initial expected element count hint passed to `Hnsw::new`. Used only to
/// pre-allocate; the index grows transparently.
const HNSW_INITIAL_CAPACITY: usize = 1024;

/// Default `ef_search` used when none is supplied by the caller.
///
/// Why: A small multiple of `top_k` (here, max(top_k, 64)) is the standard
/// recommendation. We pick a baseline of 64 so the search quality stays
/// stable for small `top_k`.
const HNSW_DEFAULT_EF_SEARCH: usize = 64;

/// Why: A structured error type lets callers distinguish redb failures
/// from postcard decode failures and from UUID parse failures without
/// pattern-matching on stringly-typed `anyhow::Error` payloads.
/// What: `thiserror` enum carrying the underlying source error.
/// Test: Indirectly via the unit tests below (each variant is reached when
/// the corresponding subsystem returns an error).
#[derive(Debug, Error)]
pub enum HnswStoreError {
    /// Boxed so the enum stays small enough that `Result<T, HnswStoreError>`
    /// doesn't trip clippy's `result_large_err` lint. The redb error types
    /// each occupy ~160 bytes of stack which would otherwise dominate every
    /// `Result` returned from this module.
    #[error("redb error: {0}")]
    Redb(#[from] Box<redb::Error>),
    #[error("redb storage error: {0}")]
    RedbStorage(#[from] Box<redb::StorageError>),
    #[error("redb transaction error: {0}")]
    RedbTransaction(#[from] Box<redb::TransactionError>),
    #[error("redb table error: {0}")]
    RedbTable(#[from] Box<redb::TableError>),
    #[error("redb commit error: {0}")]
    RedbCommit(#[from] Box<redb::CommitError>),
    #[error("postcard codec error: {0}")]
    Postcard(#[from] postcard::Error),
    #[error("invalid uuid in vector_keys table: {0}")]
    InvalidUuid(#[from] uuid::Error),
    #[error("vector dimension mismatch: expected {expected}, got {got}")]
    DimensionMismatch { expected: usize, got: usize },
    /// Returned by every write method when the store is in snapshot
    /// (read-only) mode. Callers should surface this verbatim — the
    /// message is the canonical guidance for issue #59.
    #[error(
        "palace is read-only: HTTP daemon holds the write lock — \
         route writes through the daemon's HTTP API or stop the daemon \
         before retrying via stdio"
    )]
    ReadOnly,
}

// Why: redb's `?` operator needs a `From<redb::StorageError>` (etc.) impl
// to convert. The `#[from] Box<redb::StorageError>` derive only generates
// `From<Box<redb::StorageError>>`, so we add an explicit hop that boxes
// the inner error on the fly. This keeps call sites using `?` clean
// without forcing every caller to `.map_err(Box::new)`.
impl From<redb::Error> for HnswStoreError {
    fn from(e: redb::Error) -> Self {
        Self::Redb(Box::new(e))
    }
}
impl From<redb::StorageError> for HnswStoreError {
    fn from(e: redb::StorageError) -> Self {
        Self::RedbStorage(Box::new(e))
    }
}
impl From<redb::TransactionError> for HnswStoreError {
    fn from(e: redb::TransactionError) -> Self {
        Self::RedbTransaction(Box::new(e))
    }
}
impl From<redb::TableError> for HnswStoreError {
    fn from(e: redb::TableError) -> Self {
        Self::RedbTable(Box::new(e))
    }
}
impl From<redb::CommitError> for HnswStoreError {
    fn from(e: redb::CommitError) -> Self {
        Self::RedbCommit(Box::new(e))
    }
}

/// Public result alias to keep call-site signatures concise.
///
/// Why: Most call sites only need `Result<T, HnswStoreError>` and
/// repeating the long error path noise.
/// What: `type Result<T> = std::result::Result<T, HnswStoreError>`.
/// Test: Used by every public method below.
pub type Result<T> = std::result::Result<T, HnswStoreError>;

/// Pure-Rust HNSW store backed by redb for persistence (issue #50).
///
/// Why: We need durable HNSW search without a C++ FFI dependency.
/// Persisting raw vectors in redb (rather than serializing the HNSW graph
/// itself) lets us rebuild a fresh in-memory graph on every palace open,
/// which is both simpler and resilient to `hnsw_rs` schema changes between
/// minor versions. The cost is a one-time O(N) re-insertion per open;
/// real-world palaces are bounded to ~10⁵ vectors so this is acceptable.
/// What: Holds `Arc<Database>` for persistence, the in-memory `Hnsw<f32,
/// DistCosine>` wrapped in `Arc<RwLock<_>>` for concurrent reads, the
/// embedding dimension (for validation), and an `AtomicU64` monotonic
/// vector_id counter (seeded from `max(VECTOR_KEYS) + 1` on open).
/// Test: See `tests::upsert_and_search_round_trips` and friends.
pub struct HnswStore {
    db: Arc<Database>,
    index: Arc<RwLock<Hnsw<'static, f32, DistCosine>>>,
    dim: usize,
    next_id: AtomicU64,
    /// When true, every write method short-circuits with a "read-only"
    /// error before touching redb.
    ///
    /// Why: Issue #59 — a stdio MCP client may open the same palace as the
    /// HTTP daemon by snapshotting the redb file. Snapshot writes would
    /// silently diverge from the daemon's live file, so we reject them at
    /// the store boundary instead.
    /// What: Set by `HnswStore::open_read_only`; consulted by `upsert`,
    /// `delete`, and `compact_orphans`.
    /// Test: `vector_writes_rejected_on_snapshot` (in `vector.rs`).
    read_only: bool,
}

impl HnswStore {
    /// Open an HNSW store against an existing redb database.
    ///
    /// Why: The palace's KG and vector store share the same redb file so
    /// drawer/triple writes and vector upserts can be coordinated. Passing
    /// in `Arc<Database>` (rather than a path) lets the caller own the
    /// connection cache and reuse the same handle as `KgStoreRedb`.
    /// What: Touches `VECTORS` / `VECTOR_KEYS` / `DELETED_VECTORS` to
    /// create them if missing, then reads every `(vector_id, vec)` row from
    /// `VECTORS` (skipping tombstoned ids) and replays them into a fresh
    /// in-memory `Hnsw<f32, DistCosine>` index. Seeds `next_id` from
    /// `max(VECTOR_KEYS.value()) + 1` so subsequent upserts never collide
    /// with an existing id.
    /// Test: `hydration_restores_index`.
    pub fn open(db: Arc<Database>, dim: usize) -> Result<Self> {
        Self::open_with_mode(db, dim, false)
    }

    /// Open an HNSW store in either read-write or read-only mode.
    ///
    /// Why: Issue #59 — when redb was opened via the snapshot fallback,
    /// the schema is already initialised in the copy we hold, and any
    /// writes would only land in the snapshot. Skipping the init write
    /// transaction in that case preserves the "no writes touch this
    /// file" invariant, and stamping `read_only = true` makes every
    /// mutating method return a clear error to the caller.
    /// What: Identical to `open` except it skips the schema-touch write
    /// txn when `read_only` is true and sets the `read_only` flag.
    /// Test: `vector_writes_rejected_on_snapshot` (in `vector.rs`).
    pub fn open_with_mode(db: Arc<Database>, dim: usize, read_only: bool) -> Result<Self> {
        // Touch the tables in a write txn so a brand-new redb file carries
        // them. redb only persists a table after it is opened for write at
        // least once. In read-only mode we skip this — the snapshot copy
        // we hold was made from a fully-initialised live file, so the
        // tables already exist.
        if !read_only {
            let wtx = db.begin_write()?;
            {
                let _ = wtx.open_table(VECTORS)?;
                let _ = wtx.open_table(VECTOR_KEYS)?;
                let _ = wtx.open_table(DELETED_VECTORS)?;
            }
            wtx.commit()?;
        }

        let index = Hnsw::<f32, DistCosine>::new(
            HNSW_MAX_NB_CONNECTION,
            HNSW_INITIAL_CAPACITY,
            HNSW_MAX_LAYER,
            HNSW_EF_CONSTRUCTION,
            DistCosine,
        );

        // Load tombstones first so we never insert a deleted point into the
        // fresh in-memory graph during hydration.
        let tombstones: std::collections::HashSet<u64> = {
            let rtx = db.begin_read()?;
            let table = rtx.open_table(DELETED_VECTORS)?;
            let mut set = std::collections::HashSet::new();
            for entry in table.iter()? {
                let (k, _) = entry?;
                set.insert(k.value());
            }
            set
        };

        // Replay every live vector into the in-memory graph and find the
        // largest vector_id ever assigned so `next_id` resumes correctly.
        let mut max_seen: u64 = 0;
        {
            let rtx = db.begin_read()?;
            let table = rtx.open_table(VECTORS)?;
            for entry in table.iter()? {
                let (k, v) = entry?;
                let id = k.value();
                if id > max_seen {
                    max_seen = id;
                }
                if tombstones.contains(&id) {
                    continue;
                }
                let vec: Vec<f32> = postcard::from_bytes(v.value())?;
                if vec.len() != dim {
                    return Err(HnswStoreError::DimensionMismatch {
                        expected: dim,
                        got: vec.len(),
                    });
                }
                index.insert((vec.as_slice(), id as usize));
            }
        }

        // Also consider the highest mapped id from VECTOR_KEYS in case
        // VECTORS was cleared but the mapping survived (defensive).
        {
            let rtx = db.begin_read()?;
            let table = rtx.open_table(VECTOR_KEYS)?;
            for entry in table.iter()? {
                let (_, v) = entry?;
                let id = v.value();
                if id > max_seen {
                    max_seen = id;
                }
            }
        }

        Ok(Self {
            db,
            index: Arc::new(RwLock::new(index)),
            dim,
            next_id: AtomicU64::new(max_seen.saturating_add(1)),
            read_only,
        })
    }

    /// Whether this store rejects writes (i.e. it was opened against a
    /// snapshot of a file locked by another process).
    ///
    /// Why: Issue #59 — `UsearchStore` and ultimately `PalaceHandle`
    /// surface this through to the MCP layer so write tools can produce a
    /// meaningful error instead of a redb traceback.
    /// What: Returns the value of the `read_only` flag.
    /// Test: `vector_writes_rejected_on_snapshot` (in `vector.rs`).
    pub fn is_read_only(&self) -> bool {
        self.read_only
    }

    /// Insert a (uuid, vector) row, returning the assigned vector_id.
    ///
    /// Why: Callers reference vectors by drawer UUID; the HNSW graph keys
    /// them by `usize`. Allocating a monotonic id and persisting both the
    /// mapping (`VECTOR_KEYS`) and the raw vector (`VECTORS`) inside the
    /// same write transaction guarantees consistency across crash points.
    /// If the same UUID already has a vector_id, that id is reused — the
    /// old vector is overwritten in redb. The new vector is also inserted
    /// into the in-memory graph (note: `hnsw_rs` does not support point
    /// updates, so a re-upsert effectively shadows the old graph entry; the
    /// older copy stays in the graph but is not addressable via the UUID
    /// mapping, and a full rebuild reclaims it).
    /// What: Validates `vector.len() == dim`, reads (or allocates) the
    /// vector_id under one write txn, writes the postcard-encoded vector to
    /// `VECTORS`, writes the UUID→id mapping to `VECTOR_KEYS`, and removes
    /// any prior tombstone for this id. Then inserts the vector into the
    /// in-memory graph.
    /// Test: `upsert_and_search_round_trips`.
    pub fn upsert(&self, uuid: &str, vector: &[f32]) -> Result<u64> {
        if self.read_only {
            return Err(HnswStoreError::ReadOnly);
        }
        if vector.len() != self.dim {
            return Err(HnswStoreError::DimensionMismatch {
                expected: self.dim,
                got: vector.len(),
            });
        }

        let encoded: Vec<u8> = postcard::to_allocvec(&vector.to_vec())?;
        let wtx = self.db.begin_write()?;
        let vector_id;
        {
            let mut vectors = wtx.open_table(VECTORS)?;
            let mut keys = wtx.open_table(VECTOR_KEYS)?;
            let mut tombstones = wtx.open_table(DELETED_VECTORS)?;

            // Resolve the existing id in a scoped block so the AccessGuard
            // (immutable borrow of `keys`) is dropped before we re-borrow
            // `keys` mutably for `insert`.
            let existing: Option<u64> = keys.get(uuid)?.map(|g| g.value());
            vector_id = match existing {
                Some(id) => id,
                None => {
                    let id = self.next_id.fetch_add(1, Ordering::SeqCst);
                    keys.insert(uuid, id)?;
                    id
                }
            };
            vectors.insert(vector_id, encoded.as_slice())?;
            // Clear any prior tombstone so a re-upsert revives the row.
            let _ = tombstones.remove(vector_id)?;
        }
        wtx.commit()?;

        self.index.read().insert((vector, vector_id as usize));

        Ok(vector_id)
    }

    /// Cosine-similarity search returning (uuid, distance) pairs.
    ///
    /// Why: Callers identify drawers by UUID, not by HNSW-internal `usize`.
    /// We resolve the mapping with a single redb `iter()` over
    /// `VECTOR_KEYS` (small, in-memory after the first scan) so the search
    /// path is a single in-memory graph traversal plus a hash lookup per
    /// hit. Tombstoned ids are filtered out before the lookup so callers
    /// never see deleted points.
    /// What: Calls `Hnsw::search(query, k, ef_search)`, filters results
    /// against `DELETED_VECTORS`, then maps each surviving `d_id` back to
    /// its UUID via the `VECTOR_KEYS` reverse map. Returns hits sorted by
    /// ascending distance (best first).
    /// Test: `upsert_and_search_round_trips`, `delete_filters_results`.
    pub fn search(&self, query: &[f32], k: usize) -> Result<Vec<(String, f32)>> {
        if query.len() != self.dim {
            return Err(HnswStoreError::DimensionMismatch {
                expected: self.dim,
                got: query.len(),
            });
        }

        // Build (id → uuid) reverse map and a tombstone set up front. Both
        // tables are tiny relative to the vector data, so this is cheap.
        let mut reverse: HashMap<u64, String> = HashMap::new();
        let mut tombstones: std::collections::HashSet<u64> = std::collections::HashSet::new();
        {
            let rtx = self.db.begin_read()?;
            let keys = rtx.open_table(VECTOR_KEYS)?;
            for entry in keys.iter()? {
                let (k, v) = entry?;
                reverse.insert(v.value(), k.value().to_string());
            }
            let dead = rtx.open_table(DELETED_VECTORS)?;
            for entry in dead.iter()? {
                let (k, _) = entry?;
                tombstones.insert(k.value());
            }
        }

        // Over-fetch so tombstoning doesn't starve callers asking for k.
        // 2x is sufficient in the common case; for pathological cases the
        // caller can re-issue.
        let ef = HNSW_DEFAULT_EF_SEARCH.max(k * 2);
        let raw = self
            .index
            .read()
            .search(query, k.saturating_mul(2).max(k), ef);

        let mut out: Vec<(String, f32)> = Vec::with_capacity(k);
        for hit in raw {
            let id = hit.d_id as u64;
            if tombstones.contains(&id) {
                continue;
            }
            if let Some(uuid) = reverse.get(&id) {
                out.push((uuid.clone(), hit.distance));
                if out.len() >= k {
                    break;
                }
            }
        }
        Ok(out)
    }

    /// Mark a UUID's vector as deleted (tombstone).
    ///
    /// Why: `hnsw_rs` does not support removing a point from the graph
    /// after insertion, so a true delete would require rebuilding the
    /// index. We instead write a tombstone to redb and filter at search
    /// time. A subsequent `compact_orphans` (or a full rebuild via
    /// `open`) reclaims the storage and the graph entry.
    /// What: In a single write txn, removes the `VECTOR_KEYS` row for the
    /// UUID, inserts the vector_id into `DELETED_VECTORS`. Returns `true`
    /// if a mapping was found and tombstoned, `false` if the UUID was not
    /// known. The vector row itself stays in `VECTORS` until compaction.
    /// Test: `delete_filters_results`.
    pub fn delete(&self, uuid: &str) -> Result<bool> {
        if self.read_only {
            return Err(HnswStoreError::ReadOnly);
        }
        let wtx = self.db.begin_write()?;
        let removed;
        {
            let mut keys = wtx.open_table(VECTOR_KEYS)?;
            let mut tombstones = wtx.open_table(DELETED_VECTORS)?;
            removed = match keys.remove(uuid)? {
                Some(g) => {
                    tombstones.insert(g.value(), [].as_slice())?;
                    true
                }
                None => false,
            };
        }
        wtx.commit()?;
        Ok(removed)
    }

    /// Number of live vectors (total rows in `VECTORS` minus tombstones).
    ///
    /// Why: Callers (diagnostics, compaction reporting) want the user-
    /// visible count, not the raw `VECTORS` row count which includes
    /// tombstoned entries pending compaction.
    /// What: Reads both `VECTORS` and `DELETED_VECTORS` lengths and returns
    /// the difference, clamped at zero.
    /// Test: Indirectly via the unit tests below.
    pub fn len(&self) -> Result<usize> {
        let rtx = self.db.begin_read()?;
        let vectors = rtx.open_table(VECTORS)?;
        let dead = rtx.open_table(DELETED_VECTORS)?;
        let total = vectors.len()? as usize;
        let tombstoned = dead.len()? as usize;
        Ok(total.saturating_sub(tombstoned))
    }

    /// True if no live vectors remain.
    ///
    /// Why: clippy `len_without_is_empty` requires this alongside `len`,
    /// and callers benefit from a zero-allocation existence check that
    /// short-circuits before iterating.
    /// What: Returns `Ok(true)` when `len()? == 0`.
    /// Test: Indirectly via the unit tests below.
    pub fn is_empty(&self) -> Result<bool> {
        Ok(self.len()? == 0)
    }

    /// Snapshot every drawer key currently mapped to a vector.
    ///
    /// Why: The dream compaction / orphan-detection pass (issue #51) needs
    /// to enumerate every drawer UUID known to the index so it can diff
    /// against the authoritative drawer table and drop the strays. Unlike
    /// the old usearch-era `key_map`, `VECTOR_KEYS` is persisted, so this
    /// works after a cold reopen too.
    /// What: Iterates `VECTOR_KEYS` and collects each key (the string
    /// UUID) into a `Vec<String>`. Returns an empty vec for a fresh
    /// store.
    /// Test: Indirectly via `vector::tests::compact_orphans_removes_only_missing_ids`.
    pub fn all_keys(&self) -> Result<Vec<String>> {
        let rtx = self.db.begin_read()?;
        let keys = rtx.open_table(VECTOR_KEYS)?;
        let mut out = Vec::new();
        for entry in keys.iter()? {
            let (k, _) = entry?;
            out.push(k.value().to_string());
        }
        Ok(out)
    }

    /// Remove `VECTORS` rows whose `vector_id` no longer has a matching
    /// `VECTOR_KEYS` entry (i.e. dangling vectors). Also clears tombstoned
    /// rows from `VECTORS` and `DELETED_VECTORS` in the same pass.
    ///
    /// Why: Over a palace's lifetime, the `VECTORS` table accumulates
    /// rows that were tombstoned via `delete` but never physically
    /// removed, plus any historical orphans from crashed writes. This
    /// method reclaims the storage. The in-memory graph still contains
    /// those points, but they are no longer addressable; a full
    /// rebuild via `open` reclaims the graph as well.
    /// What: Builds the set of live vector_ids by scanning `VECTOR_KEYS`,
    /// then iterates `VECTORS` and `DELETED_VECTORS` and removes any row
    /// whose id is not in that set, or which is tombstoned. Runs in a
    /// single write transaction so partial progress can never be observed.
    /// Returns the number of `VECTORS` rows removed.
    /// Test: `compact_orphans_removes_dangling`.
    pub fn compact_orphans(&self) -> Result<usize> {
        if self.read_only {
            return Err(HnswStoreError::ReadOnly);
        }
        // Snapshot live ids and tombstoned ids in a read txn first to
        // avoid holding the write txn over a large scan.
        let (live_ids, tombstoned_ids): (std::collections::HashSet<u64>, Vec<u64>) = {
            let rtx = self.db.begin_read()?;
            let keys = rtx.open_table(VECTOR_KEYS)?;
            let mut live = std::collections::HashSet::new();
            for entry in keys.iter()? {
                let (_, v) = entry?;
                live.insert(v.value());
            }
            let dead = rtx.open_table(DELETED_VECTORS)?;
            let mut deads = Vec::new();
            for entry in dead.iter()? {
                let (k, _) = entry?;
                deads.push(k.value());
            }
            (live, deads)
        };

        // Now find vector rows that are NOT in live_ids (i.e. orphans).
        let orphan_ids: Vec<u64> = {
            let rtx = self.db.begin_read()?;
            let vectors = rtx.open_table(VECTORS)?;
            let mut orphans = Vec::new();
            for entry in vectors.iter()? {
                let (k, _) = entry?;
                let id = k.value();
                if !live_ids.contains(&id) {
                    orphans.push(id);
                }
            }
            orphans
        };

        let removed = orphan_ids.len();
        if orphan_ids.is_empty() && tombstoned_ids.is_empty() {
            return Ok(0);
        }

        let wtx = self.db.begin_write()?;
        {
            let mut vectors = wtx.open_table(VECTORS)?;
            let mut dead = wtx.open_table(DELETED_VECTORS)?;
            for id in &orphan_ids {
                let _ = vectors.remove(id)?;
                // If this orphan was also tombstoned, clear the tombstone.
                let _ = dead.remove(id)?;
            }
            for id in &tombstoned_ids {
                if live_ids.contains(id) {
                    // Tombstoned but still mapped — leave the vector row
                    // intact (re-upsert flow); only clear the tombstone if
                    // it has no mapping. This branch is defensive: in the
                    // current API a tombstoned id can never be remapped
                    // because `delete` also removes the mapping.
                    continue;
                }
                let _ = dead.remove(id)?;
            }
        }
        wtx.commit()?;
        Ok(removed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use redb::Database;
    use tempfile::tempdir;
    use uuid::Uuid;

    /// Build a deterministic dim-D unit vector. Different seeds produce
    /// numerically distinct vectors, which keeps the HNSW graph from
    /// short-circuiting any "exact match" paths during search.
    fn unit_vec(dim: usize, seed: u32) -> Vec<f32> {
        let raw: Vec<f32> = (0..dim).map(|i| ((i as u32 + seed) as f32) + 1.0).collect();
        let norm: f32 = raw.iter().map(|x| x * x).sum::<f32>().sqrt();
        raw.into_iter().map(|x| x / norm).collect()
    }

    fn open_store(dim: usize) -> (tempfile::TempDir, HnswStore) {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("hnsw.redb");
        let db = Arc::new(Database::create(&path).expect("create db"));
        let store = HnswStore::open(db, dim).expect("open store");
        (dir, store)
    }

    /// Why: The end-to-end contract is "upsert a vector under a UUID,
    /// then search for that vector and get the UUID back at rank 0".
    /// What: Insert three vectors, query with one of them, assert the
    /// matching UUID is the top hit with near-zero distance.
    /// Test: This test itself is the verification.
    #[test]
    fn upsert_and_search_round_trips() {
        let (_dir, store) = open_store(8);
        let u1 = Uuid::new_v4().to_string();
        let u2 = Uuid::new_v4().to_string();
        let u3 = Uuid::new_v4().to_string();
        let v1 = unit_vec(8, 1);
        let v2 = unit_vec(8, 100);
        let v3 = unit_vec(8, 200);

        store.upsert(&u1, &v1).unwrap();
        store.upsert(&u2, &v2).unwrap();
        store.upsert(&u3, &v3).unwrap();

        let hits = store.search(&v2, 1).unwrap();
        assert_eq!(hits.len(), 1, "expected one hit");
        assert_eq!(hits[0].0, u2, "top hit must be the queried vector's uuid");
        assert!(
            hits[0].1 < 1e-3,
            "distance should be ~0 for exact match, got {}",
            hits[0].1
        );
    }

    /// Why: `delete` must hide the vector from subsequent searches even
    /// though `hnsw_rs` cannot physically remove it from the graph.
    /// What: Insert three vectors, delete one, search using the deleted
    /// vector, assert the deleted UUID is NOT in the results.
    /// Test: This test itself is the verification.
    #[test]
    fn delete_filters_results() {
        let (_dir, store) = open_store(8);
        let u1 = Uuid::new_v4().to_string();
        let u2 = Uuid::new_v4().to_string();
        let u3 = Uuid::new_v4().to_string();
        let v1 = unit_vec(8, 11);
        let v2 = unit_vec(8, 22);
        let v3 = unit_vec(8, 33);

        store.upsert(&u1, &v1).unwrap();
        store.upsert(&u2, &v2).unwrap();
        store.upsert(&u3, &v3).unwrap();

        assert!(store.delete(&u2).unwrap(), "delete should report removed");
        // Second delete is a no-op and returns false.
        assert!(!store.delete(&u2).unwrap());

        let hits = store.search(&v2, 3).unwrap();
        assert!(
            !hits.iter().any(|(uuid, _)| uuid == &u2),
            "deleted uuid must not appear in results: {hits:?}"
        );
        assert_eq!(store.len().unwrap(), 2, "len should account for tombstone");
    }

    /// Why: A fresh `HnswStore::open` against the same redb file must
    /// rehydrate the in-memory graph from `VECTORS`, so searches return
    /// the same UUIDs as before reopen.
    /// What: Upsert via one store instance, drop it, reopen at the same
    /// path, run the same search, assert the same UUID comes back.
    /// Test: This test itself is the verification.
    #[test]
    fn hydration_restores_index() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("hnsw.redb");
        let u1 = Uuid::new_v4().to_string();
        let v1 = unit_vec(8, 42);

        {
            let db = Arc::new(Database::create(&path).expect("create"));
            let store = HnswStore::open(db, 8).unwrap();
            store.upsert(&u1, &v1).unwrap();
            assert_eq!(store.len().unwrap(), 1);
        }

        // Reopen — the in-memory graph must rebuild from redb.
        let db = Arc::new(Database::create(&path).expect("reopen"));
        let store = HnswStore::open(db, 8).unwrap();
        assert_eq!(store.len().unwrap(), 1, "len survives reopen");

        let hits = store.search(&v1, 1).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].0, u1, "uuid must round-trip across reopen");
    }

    /// Why: `compact_orphans` is the maintenance hook that reclaims
    /// `VECTORS` rows whose `VECTOR_KEYS` mapping has been removed.
    /// What: Manually insert a `VECTORS` row without a corresponding
    /// `VECTOR_KEYS` entry (simulating an old orphan), upsert a real one,
    /// run compaction, assert the orphan was removed and the real one
    /// survived.
    /// Test: This test itself is the verification.
    #[test]
    fn compact_orphans_removes_dangling() {
        let (_dir, store) = open_store(8);
        let u1 = Uuid::new_v4().to_string();
        let v1 = unit_vec(8, 7);

        // Real upsert — creates one (VECTORS, VECTOR_KEYS) pair.
        store.upsert(&u1, &v1).unwrap();
        assert_eq!(store.len().unwrap(), 1);

        // Manually inject an orphan: write to VECTORS without writing to
        // VECTOR_KEYS. Use a vector_id that the store has not allocated.
        let orphan_id: u64 = 999_999;
        let orphan_vec: Vec<f32> = unit_vec(8, 99);
        let encoded = postcard::to_allocvec(&orphan_vec).unwrap();
        {
            let wtx = store.db.begin_write().unwrap();
            {
                let mut vectors = wtx.open_table(VECTORS).unwrap();
                vectors.insert(orphan_id, encoded.as_slice()).unwrap();
            }
            wtx.commit().unwrap();
        }

        // Now `len()` sees two VECTORS rows minus zero tombstones = 2.
        assert_eq!(store.len().unwrap(), 2);

        let removed = store.compact_orphans().unwrap();
        assert_eq!(removed, 1, "should remove exactly the orphan");
        assert_eq!(store.len().unwrap(), 1, "live vector survives");

        // The real upsert should still resolve via search.
        let hits = store.search(&v1, 1).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].0, u1);
    }

    /// Why: Dimension mismatches are programmer errors that must surface
    /// loudly (not corrupt the index silently).
    /// What: Open a dim=8 store, attempt to upsert a 4-d vector, assert
    /// the call returns `DimensionMismatch`.
    /// Test: This test itself is the verification.
    #[test]
    fn dimension_mismatch_is_rejected() {
        let (_dir, store) = open_store(8);
        let u1 = Uuid::new_v4().to_string();
        let too_small = vec![0.1_f32; 4];
        let err = store.upsert(&u1, &too_small).unwrap_err();
        match err {
            HnswStoreError::DimensionMismatch {
                expected: 8,
                got: 4,
            } => {}
            other => panic!("wrong error variant: {other:?}"),
        }
    }
}
