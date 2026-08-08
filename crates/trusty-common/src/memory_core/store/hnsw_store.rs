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

use hnsw_rs::prelude::{DistCosine, Hnsw};
use parking_lot::RwLock;
use redb::{Database, ReadableDatabase, ReadableTable, ReadableTableMetadata, Table};
use thiserror::Error;

use crate::memory_core::store::kg_store::{
    DELETED_VECTORS, NEXT_VECTOR_ID, VECTOR_ID_SEQ, VECTOR_KEYS, VECTORS,
};

mod exhaustive;
use exhaustive::{EXHAUSTIVE_SCAN_MAX_POINTS, exhaustive_nearest, resolve_shadowed};

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

/// How many candidate ids the allocator may probe before it gives up.
///
/// Why (#5005): the persisted counter is the mechanism that keeps ids unique;
/// the occupancy probe in [`allocate_vector_id`] is the backstop for a counter
/// that is somehow behind the table (a palace written by a pre-#5005 binary
/// between two opens, a hand-edited file). Each probe jumps past the highest
/// occupied id, so one iteration resolves any real collision — a budget this
/// small only runs out if the table is being mutated underneath a write
/// transaction, which redb does not permit. Bounded so a corrupt file makes
/// `upsert` fail loudly instead of spinning.
/// What: 8 attempts, then [`HnswStoreError::IdAllocationFailed`].
/// Test: `upsert_refuses_to_reuse_an_id_already_present_in_vectors`.
const MAX_ALLOC_PROBES: u8 = 8;

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
    /// The allocator could not find a free `vector_id` within
    /// [`MAX_ALLOC_PROBES`]. Fails the upsert rather than overwriting a live
    /// vector (#5005).
    #[error(
        "could not allocate a free vector_id after {probes} attempts \
         (last candidate {last_candidate} is already present in VECTORS); \
         refusing to overwrite a live vector — the palace index needs a rebuild"
    )]
    IdAllocationFailed { probes: u8, last_candidate: u64 },
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

/// Reserve the next free `vector_id`, inside the caller's write transaction.
///
/// Why (#5005): the old allocator was a process-local `AtomicU64` seeded at
/// open. Two live `HnswStore`s over one database file — which the vector-db
/// cache deliberately allows, so that a palace opened twice in one process
/// shares a single redb handle — each seeded from the same high-water mark and
/// then issued the same ids. Because redb permits one write transaction at a
/// time per database, moving the reservation into the transaction that does the
/// insert makes it serialisable with every other writer on that file: the id
/// and the row that claims it commit or roll back together.
/// What: reads `VECTOR_ID_SEQ`, takes the first candidate with no `VECTORS`
/// row, writes `candidate + 1` back, and returns the candidate. When the
/// candidate IS occupied — a counter left behind by a pre-#5005 binary, or a
/// hand-edited file — it jumps past the highest id EITHER vector table knows
/// about (see [`high_water`]) rather than overwriting, logs the correction, and
/// retries; after [`MAX_ALLOC_PROBES`] it fails with
/// [`HnswStoreError::IdAllocationFailed`] instead of aliasing.
/// A missing counter row (only reachable if the store skipped open-time
/// seeding) falls back to the same high-water mark the seed would have used.
/// Test: `upsert_refuses_to_reuse_an_id_already_present_in_vectors`,
/// `two_live_stores_over_one_file_never_alias_ids`.
fn allocate_vector_id(
    seq: &mut Table<'_, &'static str, u64>,
    vectors: &Table<'_, u64, &'static [u8]>,
    keys: &Table<'_, &'static str, u64>,
) -> Result<u64> {
    let mut candidate = match seq.get(NEXT_VECTOR_ID)? {
        Some(g) => g.value(),
        None => {
            tracing::warn!(
                "#5005: VECTOR_ID_SEQ has no counter row at allocation time; \
                 falling back to the VECTORS high-water mark"
            );
            high_water(vectors, keys)?
        }
    };

    for probe in 0..MAX_ALLOC_PROBES {
        if vectors.get(candidate)?.is_none() {
            seq.insert(NEXT_VECTOR_ID, candidate.saturating_add(1))?;
            return Ok(candidate);
        }
        tracing::warn!(
            candidate,
            probe,
            "#5005: persisted vector_id counter is behind the VECTORS table; \
             skipping the occupied id instead of overwriting it"
        );
        // Jump past the highest occupied id so one correction is enough.
        candidate = high_water(vectors, keys)?.max(candidate.saturating_add(1));
    }

    Err(HnswStoreError::IdAllocationFailed {
        probes: MAX_ALLOC_PROBES,
        last_candidate: candidate,
    })
}

/// One past the highest `vector_id` either vector table knows about.
///
/// Why: both the allocator's collision jump and its missing-counter fallback
/// need a bound above which NO id is taken. `VECTORS` alone is not that bound.
/// A `VECTOR_KEYS` row can outlive its `VECTORS` row: `compact_orphans` reads
/// the live-id set, computes orphans, and deletes them in three separate
/// transactions, so an `upsert` that lands between the first and the third has
/// its brand-new id treated as an orphan and its `VECTORS` row removed while
/// the key survives. Under the in-process concurrency this store supports that
/// is a reachable state, and a `VECTORS`-only bound would hand the id straight
/// back out — the same aliasing this module exists to prevent, arrived at from
/// the other table.
/// What: `max(last VECTORS key, largest mapped VECTOR_KEYS value) + 1`, or 1
/// when both are empty (id 0 is never issued, matching the pre-#5005 seed of
/// `max_seen + 1`). `VECTORS.last()` is O(log n); the `VECTOR_KEYS` sweep is
/// O(n) but runs only on the rare correction path — never on a healthy upsert,
/// where the counter's own invariant already bounds the candidate.
/// Test: `upsert_refuses_to_reuse_an_id_already_present_in_vectors`,
/// `upsert_refuses_an_id_that_only_vector_keys_still_claims`.
fn high_water(
    vectors: &Table<'_, u64, &'static [u8]>,
    keys: &Table<'_, &'static str, u64>,
) -> Result<u64> {
    let mut max_seen = match vectors.last()? {
        Some((k, _)) => k.value(),
        None => 0,
    };
    for entry in keys.iter()? {
        let (_, v) = entry?;
        max_seen = max_seen.max(v.value());
    }
    Ok(max_seen.saturating_add(1))
}

/// Public result alias to keep call-site signatures concise.
///
/// Why: Most call sites only need `Result<T, HnswStoreError>` and
/// repeating the long error path noise.
/// What: `type Result<T> = std::result::Result<T, HnswStoreError>`.
/// Test: Used by every public method below.
pub type Result<T> = std::result::Result<T, HnswStoreError>;

/// How many drawer keys point at a `vector_id` that another key also points at.
///
/// Why (#5005 / #5000): every count-based health signal this palace had could
/// be zero while four drawers were unretrievable — `drawer_count` matched,
/// `palace_reembed` reported nothing missing, and recall could still hit
/// lexically. `key_rows` versus `distinct_vector_ids` is the one comparison
/// that cannot be fooled by it, because it never leaves `VECTOR_KEYS`.
/// What: the two counts plus the offending groups, each `(vector_id, uuids)`.
/// Test: `audit_detects_two_uuids_mapped_to_one_id`.
#[derive(Debug, Clone, Default)]
pub struct AliasAudit {
    /// Rows in `VECTOR_KEYS` — one per drawer that has ever been embedded.
    pub key_rows: usize,
    /// Distinct `vector_id`s those rows point at. Equal to `key_rows` iff no
    /// drawer's vector has been overwritten by another drawer's.
    pub distinct_vector_ids: usize,
    /// Every `vector_id` claimed by more than one uuid, with its uuids sorted.
    pub aliased: Vec<(u64, Vec<String>)>,
}

impl AliasAudit {
    /// Drawer keys caught in a collision — the count an operator acts on.
    ///
    /// Why: `aliased.len()` counts groups, not drawers, and the number that
    /// matters is how many drawers are affected. Equals
    /// `key_rows - distinct_vector_ids` on a consistent audit.
    /// What: sums the group sizes.
    /// Test: `audit_detects_two_uuids_mapped_to_one_id`.
    pub fn aliased_key_count(&self) -> usize {
        self.aliased.iter().map(|(_, uuids)| uuids.len()).sum()
    }

    /// Whether no drawer's vector is shared with another drawer.
    pub fn is_clean(&self) -> bool {
        self.aliased.is_empty()
    }
}

/// Pure-Rust HNSW store backed by redb for persistence (issue #50).
///
/// Why: We need durable HNSW search without a C++ FFI dependency.
/// Persisting raw vectors in redb (rather than serializing the HNSW graph
/// itself) lets us rebuild a fresh in-memory graph on every palace open,
/// which is both simpler and resilient to `hnsw_rs` schema changes between
/// minor versions. The cost is a one-time O(N) re-insertion per open;
/// real-world palaces are bounded to ~10⁵ vectors so this is acceptable.
/// What: Holds `Arc<Database>` for persistence, the in-memory `Hnsw<f32,
/// DistCosine>` wrapped in `Arc<RwLock<_>>` for concurrent reads, and the
/// embedding dimension (for validation). The vector_id counter is NOT held
/// here — it lives in redb (`VECTOR_ID_SEQ`), because a per-store counter is
/// exactly the #5005 aliasing bug.
/// Test: See `tests::upsert_and_search_round_trips` and friends.
pub struct HnswStore {
    db: Arc<Database>,
    index: Arc<RwLock<Hnsw<'static, f32, DistCosine>>>,
    dim: usize,
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
    /// `vector_id`s that have more than one point in the in-memory graph.
    ///
    /// Why (#5171): `hnsw_rs` cannot remove a point, so re-upserting a mapped
    /// uuid leaves its previous embedding in the graph under the same id. A
    /// search that scores the drawer by that stale copy reports content the
    /// drawer no longer holds. Knowing exactly which ids are ambiguous lets
    /// `search` re-read only those from `VECTORS` instead of re-reading every
    /// candidate — the steady state is empty, and this costs nothing there.
    /// What: written by `upsert` when the uuid already had a mapping. Starts
    /// empty at every `open`, which is correct: `open` rebuilds the graph with
    /// one point per live vector. Marked before the shadow point enters the
    /// graph, so no search can observe the point without the flag.
    /// Test: `search_scores_a_re_upserted_drawer_by_its_current_vector`,
    /// `upsert_marks_a_shadow_before_the_graph_can_serve_it`.
    shadowed: RwLock<std::collections::HashSet<u64>>,
}

impl HnswStore {
    /// Open an HNSW store against an existing redb database.
    ///
    /// Why: The palace's KG and vector store share the same redb file so
    /// drawer/triple writes and vector upserts can be coordinated. Passing
    /// in `Arc<Database>` (rather than a path) lets the caller own the
    /// connection cache and reuse the same handle as `KgStoreRedb`.
    /// What: Touches `VECTORS` / `VECTOR_KEYS` / `DELETED_VECTORS` /
    /// `VECTOR_ID_SEQ` to create them if missing, then reads every
    /// `(vector_id, vec)` row from `VECTORS` (skipping tombstoned ids) and
    /// replays them into a fresh in-memory `Hnsw<f32, DistCosine>` index.
    /// Raises the persisted `VECTOR_ID_SEQ` counter to at least
    /// `max(VECTORS, VECTOR_KEYS) + 1` (#5005).
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
                let _ = wtx.open_table(VECTOR_ID_SEQ)?;
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

        // #5005: raise the persisted allocator to the file's high-water mark.
        //
        // This is both the migration for a palace written before `VECTOR_ID_SEQ`
        // existed (no row → seeded here) and the repair for one that a
        // pre-#5005 binary wrote to between two opens (row present but stale →
        // raised here). Written as a max, never a plain set, so it is
        // idempotent and can only move forward: a rolling upgrade that
        // interleaves an old binary can leave the counter behind the tables,
        // and the next open of a fixed binary pulls it back up before any id is
        // issued. Skipped in read-only/snapshot mode, where every write path
        // returns `ReadOnly` before it could allocate anything.
        if !read_only {
            let wtx = db.begin_write()?;
            {
                let mut seq = wtx.open_table(VECTOR_ID_SEQ)?;
                let current = seq.get(NEXT_VECTOR_ID)?.map(|g| g.value()).unwrap_or(0);
                let floor = max_seen.saturating_add(1);
                if current < floor {
                    seq.insert(NEXT_VECTOR_ID, floor)?;
                }
            }
            wtx.commit()?;
        }

        Ok(Self {
            db,
            index: Arc::new(RwLock::new(index)),
            dim,
            read_only,
            shadowed: RwLock::new(std::collections::HashSet::new()),
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
    ///
    /// #5005: allocation for a NEW uuid reads and bumps the persisted
    /// `VECTOR_ID_SEQ` counter inside this same write transaction, so two live
    /// stores over one file serialise against each other instead of each
    /// handing out ids from a private counter. [`allocate_vector_id`] also
    /// refuses to return an id that already has a `VECTORS` row, so an upsert
    /// can never silently overwrite another drawer's vector.
    /// Test: `upsert_and_search_round_trips`,
    /// `two_live_stores_over_one_file_never_alias_ids`.
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
        // #5171: a re-upsert leaves the old embedding in the graph under this
        // same id, so `search` must re-read the authoritative vector for it.
        let shadows_previous;
        {
            let mut vectors = wtx.open_table(VECTORS)?;
            let mut keys = wtx.open_table(VECTOR_KEYS)?;
            let mut tombstones = wtx.open_table(DELETED_VECTORS)?;
            let mut seq = wtx.open_table(VECTOR_ID_SEQ)?;

            // Resolve the existing id in a scoped block so the AccessGuard
            // (immutable borrow of `keys`) is dropped before we re-borrow
            // `keys` mutably for `insert`.
            let existing: Option<u64> = keys.get(uuid)?.map(|g| g.value());
            shadows_previous = existing.is_some();
            vector_id = match existing {
                Some(id) => id,
                None => {
                    // #5005: allocate from redb, inside this txn.
                    let id = allocate_vector_id(&mut seq, &vectors, &keys)?;
                    keys.insert(uuid, id)?;
                    id
                }
            };
            vectors.insert(vector_id, encoded.as_slice())?;
            // Clear any prior tombstone so a re-upsert revives the row.
            let _ = tombstones.remove(vector_id)?;
        }
        wtx.commit()?;

        // #5171: mark BEFORE the graph can serve the shadow point. The two
        // steps cannot be atomic, and the asymmetry runs one way: a search
        // that sees the shadow without the flag scores the drawer by whichever
        // copy is nearer, which is the defect. A search that sees the flag
        // without the shadow re-reads the already-committed `VECTORS` row and
        // gets the right answer, so over-marking costs one point read.
        if shadows_previous {
            self.shadowed.write().insert(vector_id);
        }
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
    /// What: Ranks candidates — exactly, by scanning every point, when the
    /// palace holds at most [`EXHAUSTIVE_SCAN_MAX_POINTS`] live drawers;
    /// otherwise via `Hnsw::search(query, k, ef_search)`. Re-scores any drawer
    /// that has a shadow copy in the graph against its `VECTORS` row, filters
    /// the result against `DELETED_VECTORS`, then maps each surviving
    /// `vector_id` back to its UUID via the `VECTOR_KEYS` reverse map. Returns
    /// hits sorted by ascending distance (best first), each UUID at most once.
    ///
    /// #5171: below the threshold the graph traversal returns only the points
    /// reachable from its descent pivot along pruned layer-0 neighbour lists,
    /// which on real embeddings dropped a true top-5 neighbour on 2–4% of
    /// queries — and because `hnsw_rs` seeds its level RNG from OS entropy,
    /// *which* drawer went missing changed on every palace open. See
    /// [`exhaustive`] for the measurement and the threshold's rationale.
    /// Test: `upsert_and_search_round_trips`, `delete_filters_results`,
    /// `search_returns_the_exact_top_k_below_the_exhaustive_threshold`,
    /// `search_scores_a_re_upserted_drawer_by_its_current_vector`.
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

        // Over-fetch so tombstoning doesn't starve callers asking for k. 2x is
        // sufficient in the common case; for pathological cases the caller can
        // re-issue. Bounds the graph arm only — the scan returns everything.
        let want = k.saturating_mul(2).max(k);
        let ef = HNSW_DEFAULT_EF_SEARCH.max(k * 2);
        // #5171: `reverse.len()` is the LIVE drawer count — `get_nb_point()`
        // also counts tombstones and shadows, so a small palace could churn
        // past the threshold mid-session and silently revert to the approximate
        // path. The scan returns every live candidate rather than a
        // `want`-sized prefix: a shadowed drawer's provisional distance is
        // `min(stale, current)`, so cutting before `resolve_shadowed` lets a
        // superseded vector evict a true neighbour. The `out.len() >= k` break
        // below does the bounding instead, on the corrected ranking.
        let mut raw: Vec<(u64, f32)> = {
            let index = self.index.read();
            if reverse.len() <= EXHAUSTIVE_SCAN_MAX_POINTS {
                exhaustive_nearest(&index, query, &tombstones)
            } else {
                index
                    .search(query, want, ef)
                    .into_iter()
                    .map(|hit| (hit.d_id as u64, hit.distance))
                    .collect()
            }
        };

        // #5171: a drawer with two embeddings in the graph is scored against
        // the one `VECTORS` holds, never against the copy it superseded.
        let shadowed = self.shadowed.read();
        if !shadowed.is_empty() {
            let rtx = self.db.begin_read()?;
            let vectors = rtx.open_table(VECTORS)?;
            resolve_shadowed(&mut raw, &shadowed, query, &vectors)?;
        }
        drop(shadowed);

        let mut out: Vec<(String, f32)> = Vec::with_capacity(k);
        let mut emitted: std::collections::HashSet<u64> = std::collections::HashSet::new();
        for (id, distance) in raw {
            if tombstones.contains(&id) || !emitted.insert(id) {
                continue;
            }
            if let Some(uuid) = reverse.get(&id) {
                out.push((uuid.clone(), distance));
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

    /// Count how many drawer keys share a `vector_id` with another key.
    ///
    /// Why (#5005, and requirement 1 of #5000): key PRESENCE is what
    /// `palace_reembed` tests, and an aliased drawer has a key — so the palace
    /// that lost four drawers to id collisions reported zero missing. The
    /// signal that does catch it is arithmetic on `VECTOR_KEYS` alone: one row
    /// per drawer, one distinct id per row, so any shortfall between the two is
    /// exactly the number of drawers whose vector belongs to someone else.
    /// What: one pass over `VECTOR_KEYS` building id → uuids. Returns the row
    /// count, the distinct-id count, and every group with more than one uuid.
    /// Cheap: `VECTOR_KEYS` is one small row per drawer and holds no vectors.
    /// Test: `audit_detects_two_uuids_mapped_to_one_id`.
    pub fn audit_aliases(&self) -> Result<AliasAudit> {
        let mut by_id: HashMap<u64, Vec<String>> = HashMap::new();
        let mut key_rows = 0usize;
        {
            let rtx = self.db.begin_read()?;
            let keys = rtx.open_table(VECTOR_KEYS)?;
            for entry in keys.iter()? {
                let (k, v) = entry?;
                key_rows += 1;
                by_id
                    .entry(v.value())
                    .or_default()
                    .push(k.value().to_string());
            }
        }
        let distinct_vector_ids = by_id.len();
        let mut aliased: Vec<(u64, Vec<String>)> = by_id
            .into_iter()
            .filter(|(_, uuids)| uuids.len() > 1)
            .collect();
        // Deterministic order so callers, logs, and tests all agree.
        aliased.sort_by_key(|(id, _)| *id);
        for (_, uuids) in &mut aliased {
            uuids.sort();
        }
        Ok(AliasAudit {
            key_rows,
            distinct_vector_ids,
            aliased,
        })
    }

    /// Unmap every drawer caught in an id collision so a re-embed can repair it.
    ///
    /// 🔴 Never run against a live palace in the PR that added it (#5005) — the
    /// coverage below is all synthetic collisions. Reached from the
    /// `palace_unalias` MCP tool via `PalaceHandle::repair_aliases`, which
    /// defaults to a dry run.
    ///
    /// Why: when N uuids alias onto one `vector_id`, the single surviving
    /// `VECTORS` row holds whichever vector was written LAST, and search
    /// resolves that id to whichever uuid `VECTOR_KEYS` yields last (ascending
    /// by uuid). Those two "last"s are unrelated, so the reachable drawer's
    /// content is not reliably its own either — every member of the group is
    /// suspect, not just the unreachable ones. The only sound repair is to drop
    /// the whole group's mapping and re-embed all of them.
    /// What: for each aliased group, removes every `VECTOR_KEYS` row in it and
    /// tombstones the shared id, in one write transaction. Returns the freed
    /// uuids, which then read as ordinary "missing" drawers to
    /// `PalaceHandle::embed_health` and are repaired by the normal backfill.
    /// Idempotent: a second run finds no groups and frees nothing.
    /// Test: `unalias_frees_every_uuid_in_a_collision_group`.
    pub fn unalias(&self) -> Result<Vec<String>> {
        if self.read_only {
            return Err(HnswStoreError::ReadOnly);
        }
        let audit = self.audit_aliases()?;
        if audit.aliased.is_empty() {
            return Ok(Vec::new());
        }
        let mut freed: Vec<String> = Vec::new();
        let wtx = self.db.begin_write()?;
        {
            let mut keys = wtx.open_table(VECTOR_KEYS)?;
            let mut tombstones = wtx.open_table(DELETED_VECTORS)?;
            for (id, uuids) in &audit.aliased {
                for uuid in uuids {
                    let _ = keys.remove(uuid.as_str())?;
                    freed.push(uuid.clone());
                }
                tombstones.insert(*id, [].as_slice())?;
            }
        }
        wtx.commit()?;
        tracing::warn!(
            groups = audit.aliased.len(),
            freed = freed.len(),
            "#5005: unmapped every drawer in an aliased vector_id group; they now \
             read as missing and need a re-embed"
        );
        Ok(freed)
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
mod tests;
