//! Concrete `MemoryStore` backed by redb (metadata) + usearch (HNSW vectors).
//!
//! Why: We want an embeddable, single-process store with durable persistence
//! and fast approximate nearest-neighbor search, without running a separate
//! database server. redb gives us transactional k/v storage for payloads and
//! id<->label mappings; usearch gives us a state-of-the-art HNSW index. This
//! module glues them together behind the `MemoryStore` trait so callers never
//! see either engine directly.
//! What: `RedbUsearchStore::open` opens/creates a store directory containing a
//! `store.redb` file and per-segment `.usearch` files. Inserts allocate an
//! auto-incrementing u64 label per segment (tracked in redb), add the vector
//! to the segment's usearch index under that label, and store the payload
//! keyed by `"{prefix}:{id}"`. Searches translate labels back to ids via redb
//! and fetch payloads. Deletes tombstone the vector in usearch and drop all
//! redb rows for the id.
//!
//! Module layout (see #366 split):
//! - `mod.rs` — store struct, `open`, redb table defs, usearch helpers
//! - `store_impl.rs` — the `impl MemoryStore` block (insert/search/...)
//! - `tests.rs` — unit tests
//!
//! Test: See `tests` module — round-trip insert+search, segment isolation,
//! get-by-id, and persistence after reopen.

mod store_impl;

#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, anyhow};
use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};
use tokio::sync::Mutex;
use usearch::{Index, IndexOptions, MetricKind, ScalarKind};

use super::store::Segment;

// --- redb table definitions ---------------------------------------------

/// Key format: `"{prefix}:{id}"`; value: JSON-serialized payload string.
pub(super) const PAYLOAD_TABLE: TableDefinition<&str, &str> = TableDefinition::new("payloads");

/// Segment-scoped bidirectional label<->id maps. Labels are u64 (usearch
/// requirement); ids are arbitrary caller strings (e.g., UUIDs, file paths).
const MEM_LABEL_TO_ID: TableDefinition<u64, &str> = TableDefinition::new("mem_label_to_id");
const MEM_ID_TO_LABEL: TableDefinition<&str, u64> = TableDefinition::new("mem_id_to_label");
const CODE_LABEL_TO_ID: TableDefinition<u64, &str> = TableDefinition::new("code_label_to_id");
const CODE_ID_TO_LABEL: TableDefinition<&str, u64> = TableDefinition::new("code_id_to_label");
const CTX_LABEL_TO_ID: TableDefinition<u64, &str> = TableDefinition::new("ctx_label_to_id");
const CTX_ID_TO_LABEL: TableDefinition<&str, u64> = TableDefinition::new("ctx_id_to_label");
const BRIEF_LABEL_TO_ID: TableDefinition<u64, &str> = TableDefinition::new("brief_label_to_id");
const BRIEF_ID_TO_LABEL: TableDefinition<&str, u64> = TableDefinition::new("brief_id_to_label");
const HIST_LABEL_TO_ID: TableDefinition<u64, &str> = TableDefinition::new("hist_label_to_id");
const HIST_ID_TO_LABEL: TableDefinition<&str, u64> = TableDefinition::new("hist_id_to_label");

/// Auto-incrementing u64 counters, keyed by segment prefix.
const COUNTER_TABLE: TableDefinition<&str, u64> = TableDefinition::new("counters");

/// Default initial capacity reservation for a freshly-created usearch index.
/// Why: usearch panics/errors on `add` before `reserve` has been called with
/// capacity >= size+1. We bump this in `ensure_capacity` as the index grows.
const INITIAL_RESERVE: usize = 64;

/// Growth factor when the usearch index fills up.
const GROWTH_FACTOR: usize = 2;

// --- store --------------------------------------------------------------

/// redb + usearch backed memory store. Clone-friendly via internal `Arc`s.
///
/// Why: A single instance is meant to be shared across async tasks; the
/// inner `Arc<Mutex<Index>>` handles concurrent access to the non-reentrant
/// usearch indexes, while redb has its own transactional concurrency.
/// What: Holds one `Database` (multi-table) and two `Index` handles, plus
/// the on-disk paths so we can call `save` after mutations.
/// Test: Construction verified in `tests::roundtrip_insert_and_search`.
pub struct RedbUsearchStore {
    pub(super) db: Arc<Database>,
    pub(super) mem_index: Arc<Mutex<Option<Index>>>,
    pub(super) code_index: Arc<Mutex<Option<Index>>>,
    pub(super) ctx_index: Arc<Mutex<Option<Index>>>,
    pub(super) brief_index: Arc<Mutex<Option<Index>>>,
    pub(super) hist_index: Arc<Mutex<Option<Index>>>,
    pub(super) mem_index_path: PathBuf,
    pub(super) code_index_path: PathBuf,
    pub(super) ctx_index_path: PathBuf,
    pub(super) brief_index_path: PathBuf,
    pub(super) hist_index_path: PathBuf,
    /// Vector dimension (same for every segment in this store). Stored so
    /// `warm_segment` can rebuild an evicted index without callers re-passing
    /// the dimension that was supplied at `open()` time.
    pub(super) vector_dim: usize,
}

impl RedbUsearchStore {
    /// Open or create the store rooted at `store_dir`.
    ///
    /// Why: A single entrypoint that handles first-run (create files) and
    /// reopen (load existing files) so callers don't have to probe state.
    /// What: Creates the directory if missing, opens `store.redb`, and
    /// opens-or-creates two usearch indexes (`mem.usearch`, `code.usearch`)
    /// with the requested `vector_dim` and cosine similarity metric.
    /// Test: `tests::persists_across_reopen` covers the reopen path.
    pub fn open(store_dir: &Path, vector_dim: usize) -> Result<Self> {
        std::fs::create_dir_all(store_dir)
            .with_context(|| format!("creating store dir {}", store_dir.display()))?;

        // Issue #702: open via the recovery-aware helper so a stale redb-2.x
        // `store.redb` is moved aside and a fresh empty store is created rather
        // than crashing on warm boot after the binary upgrade.
        let db_path = store_dir.join("store.redb");
        let db = crate::memory::redb_recovery::open_redb_or_recreate(&db_path)?;

        // Touch all tables so that subsequent read transactions don't error
        // out with TableDoesNotExist on a freshly-created database.
        {
            let write_txn = db.begin_write()?;
            {
                let _ = write_txn.open_table(PAYLOAD_TABLE)?;
                let _ = write_txn.open_table(MEM_LABEL_TO_ID)?;
                let _ = write_txn.open_table(MEM_ID_TO_LABEL)?;
                let _ = write_txn.open_table(CODE_LABEL_TO_ID)?;
                let _ = write_txn.open_table(CODE_ID_TO_LABEL)?;
                let _ = write_txn.open_table(CTX_LABEL_TO_ID)?;
                let _ = write_txn.open_table(CTX_ID_TO_LABEL)?;
                let _ = write_txn.open_table(BRIEF_LABEL_TO_ID)?;
                let _ = write_txn.open_table(BRIEF_ID_TO_LABEL)?;
                let _ = write_txn.open_table(HIST_LABEL_TO_ID)?;
                let _ = write_txn.open_table(HIST_ID_TO_LABEL)?;
                let _ = write_txn.open_table(COUNTER_TABLE)?;
            }
            write_txn.commit()?;
        }

        let mem_index_path = store_dir.join("mem.usearch");
        let code_index_path = store_dir.join("code.usearch");
        let ctx_index_path = store_dir.join("ctx.usearch");
        let brief_index_path = store_dir.join("brief.usearch");
        let hist_index_path = store_dir.join("hist.usearch");

        let mem_index = open_or_create_index(&mem_index_path, vector_dim)?;
        let code_index = open_or_create_index(&code_index_path, vector_dim)?;
        let ctx_index = open_or_create_index(&ctx_index_path, vector_dim)?;
        let brief_index = open_or_create_index(&brief_index_path, vector_dim)?;
        let hist_index = open_or_create_index(&hist_index_path, vector_dim)?;

        Ok(Self {
            db: Arc::new(db),
            mem_index: Arc::new(Mutex::new(Some(mem_index))),
            code_index: Arc::new(Mutex::new(Some(code_index))),
            ctx_index: Arc::new(Mutex::new(Some(ctx_index))),
            brief_index: Arc::new(Mutex::new(Some(brief_index))),
            hist_index: Arc::new(Mutex::new(Some(hist_index))),
            mem_index_path,
            code_index_path,
            ctx_index_path,
            brief_index_path,
            hist_index_path,
            vector_dim,
        })
    }

    /// Resolve a segment to its (label_to_id, id_to_label) table pair.
    /// Enumerate every record stored in `segment` along with its vector and
    /// payload.
    ///
    /// Why: Cross-machine memory export needs the full record (id + payload +
    /// embedding) so the receiving machine can re-insert without recomputing
    /// embeddings. The trait's search/get aren't enough — we need to walk
    /// every row in the segment regardless of similarity.
    /// What: Iterates the segment's `id_to_label` table, looks up each
    /// payload from the shared `PAYLOAD_TABLE`, and pulls the embedding from
    /// usearch via `Index::get`. Skips rows where any piece is missing
    /// (orphaned labels, in-flight tombstones).
    /// Test: Exercised indirectly by `memory_export_produces_jsonl_with_machine_id`.
    pub async fn list_segment(
        &self,
        segment: Segment,
    ) -> Result<Vec<(String, Vec<f32>, serde_json::Value)>> {
        let (_, id_to_label_def) = Self::label_tables(segment);
        let read_txn = self.db.begin_read()?;
        let id_to_label = read_txn.open_table(id_to_label_def)?;
        let payloads = read_txn.open_table(PAYLOAD_TABLE)?;

        let guard = self.ensure_loaded(segment).await?;
        let index = guard.as_ref().expect("ensure_loaded guarantees Some");
        let dim = index.dimensions();

        let mut out: Vec<(String, Vec<f32>, serde_json::Value)> = Vec::new();
        for entry in id_to_label.iter()? {
            let (id_v, label_v) = entry?;
            let id = id_v.value().to_string();
            let label = label_v.value();
            let key = format!("{}:{}", segment.prefix(), &id);
            let Some(payload_raw) = payloads.get(key.as_str())? else {
                continue;
            };
            let payload: serde_json::Value = serde_json::from_str(payload_raw.value())
                .context("deserializing stored payload JSON")?;
            // Pull the vector from usearch. `get` returns an Option<Vec<f32>>;
            // a missing vector means the record is mid-tombstone — skip.
            let mut buf = vec![0.0f32; dim];
            let n = index
                .get(label, &mut buf)
                .map_err(|e| anyhow!("usearch get: {e}"))?;
            if n == 0 {
                continue;
            }
            out.push((id, buf, payload));
        }
        Ok(out)
    }

    pub(super) fn label_tables(
        segment: Segment,
    ) -> (
        TableDefinition<'static, u64, &'static str>,
        TableDefinition<'static, &'static str, u64>,
    ) {
        match segment {
            Segment::AgentMemory => (MEM_LABEL_TO_ID, MEM_ID_TO_LABEL),
            Segment::CodeIndex => (CODE_LABEL_TO_ID, CODE_ID_TO_LABEL),
            Segment::Context => (CTX_LABEL_TO_ID, CTX_ID_TO_LABEL),
            Segment::Brief => (BRIEF_LABEL_TO_ID, BRIEF_ID_TO_LABEL),
            Segment::History => (HIST_LABEL_TO_ID, HIST_ID_TO_LABEL),
        }
    }

    /// Return the in-memory index handle + on-disk path for a segment.
    pub(super) fn index_for(&self, segment: Segment) -> (Arc<Mutex<Option<Index>>>, PathBuf) {
        match segment {
            Segment::AgentMemory => (self.mem_index.clone(), self.mem_index_path.clone()),
            Segment::CodeIndex => (self.code_index.clone(), self.code_index_path.clone()),
            Segment::Context => (self.ctx_index.clone(), self.ctx_index_path.clone()),
            Segment::Brief => (self.brief_index.clone(), self.brief_index_path.clone()),
            Segment::History => (self.hist_index.clone(), self.hist_index_path.clone()),
        }
    }

    /// Acquire the segment's index, lazily reloading from disk if it was
    /// evicted by a prior `evict_segment` call.
    ///
    /// Why: Eviction (#372) drops the in-memory HNSW to free RAM after
    /// inactivity; the very next op (insert/search/delete) must transparently
    /// reload it. Centralising the reload here keeps every mutation/read path
    /// identical and makes the warm-up boundary impossible to forget.
    /// What: Locks the segment mutex; if the inner option is `None`, calls
    /// `open_or_create_index` against the on-disk file, logs a single
    /// `info!("search index warmed up", segment=?...)` line, and stores the
    /// rebuilt index back into the option. Returns the locked guard ready
    /// to use. The mutex stays held by the caller (returned guard).
    /// Test: `evict_then_warm_returns_same_results`.
    pub(super) async fn ensure_loaded<'a>(
        &'a self,
        segment: Segment,
    ) -> Result<tokio::sync::MutexGuard<'a, Option<Index>>> {
        // Borrow the field reference directly so the returned guard's
        // lifetime is tied to `&self`.
        let (mutex_ref, path_owned) = match segment {
            Segment::AgentMemory => (&self.mem_index, self.mem_index_path.clone()),
            Segment::CodeIndex => (&self.code_index, self.code_index_path.clone()),
            Segment::Context => (&self.ctx_index, self.ctx_index_path.clone()),
            Segment::Brief => (&self.brief_index, self.brief_index_path.clone()),
            Segment::History => (&self.hist_index, self.hist_index_path.clone()),
        };
        let mut guard = mutex_ref.lock().await;
        if guard.is_none() {
            let rebuilt = open_or_create_index(&path_owned, self.vector_dim)?;
            *guard = Some(rebuilt);
            tracing::info!(segment = ?segment, "search index warmed up");
        }
        Ok(guard)
    }
}

/// Construct or reload a usearch index file at `path`.
///
/// Why: Factored out so `open` stays readable and both segments share logic.
/// What: Builds `IndexOptions` with cosine + F32, creates the in-memory index,
/// reserves an initial capacity, then — if a serialized file exists — loads
/// the persisted state over top.
/// Test: `tests::persists_across_reopen` drives both branches.
fn open_or_create_index(path: &Path, vector_dim: usize) -> Result<Index> {
    let options = IndexOptions {
        dimensions: vector_dim,
        metric: MetricKind::Cos,
        quantization: ScalarKind::F32,
        ..Default::default()
    };
    let index = Index::new(&options)
        .map_err(|e| anyhow!("creating usearch index at {}: {e}", path.display()))?;
    index
        .reserve(INITIAL_RESERVE)
        .map_err(|e| anyhow!("reserving initial usearch capacity: {e}"))?;

    if path.exists() {
        let path_str = path
            .to_str()
            .ok_or_else(|| anyhow!("non-UTF8 usearch path: {}", path.display()))?;
        index
            .load(path_str)
            .map_err(|e| anyhow!("loading usearch index from {}: {e}", path.display()))?;
    }
    Ok(index)
}

/// Grow the usearch index if it has no headroom left for one more vector.
///
/// Why: usearch returns an error when `add` would exceed `capacity`; we
/// expand geometrically to amortize reallocation cost.
/// What: If `size + 1 > capacity`, reserves `max(size * GROWTH_FACTOR, INITIAL_RESERVE)`.
/// Test: Implicit in repeated inserts in `tests::roundtrip_insert_and_search`.
pub(super) fn ensure_capacity(index: &Index) -> Result<()> {
    let size = index.size();
    let capacity = index.capacity();
    if size + 1 > capacity {
        let new_cap = std::cmp::max(size * GROWTH_FACTOR, INITIAL_RESERVE);
        index
            .reserve(new_cap)
            .map_err(|e| anyhow!("reserving usearch capacity {new_cap}: {e}"))?;
    }
    Ok(())
}

/// Persist a usearch index to its on-disk path.
pub(super) fn save_index(index: &Index, path: &Path) -> Result<()> {
    let path_str = path
        .to_str()
        .ok_or_else(|| anyhow!("non-UTF8 usearch path: {}", path.display()))?;
    index
        .save(path_str)
        .map_err(|e| anyhow!("saving usearch index to {}: {e}", path.display()))?;
    Ok(())
}

/// Allocate the next auto-incrementing label for `segment`. Caller owns the
/// write transaction so the allocation and usage commit atomically.
pub(super) fn next_label(write_txn: &redb::WriteTransaction, segment: Segment) -> Result<u64> {
    let mut counters = write_txn.open_table(COUNTER_TABLE)?;
    let prefix = segment.prefix();
    let current = counters.get(prefix)?.map(|v| v.value()).unwrap_or(0);
    let next = current + 1;
    counters.insert(prefix, next)?;
    Ok(next)
}
