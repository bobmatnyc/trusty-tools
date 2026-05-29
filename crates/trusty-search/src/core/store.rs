use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use usearch::{Index, IndexOptions, MetricKind, ScalarKind};

/// Sidecar JSON written alongside the usearch binary snapshot, capturing the
/// `chunk_id → u64 key` mapping (and the `next_key` counter) so a restored
/// index can translate HNSW matches back into chunk ids.
///
/// Why: usearch persists vectors + graph + keys, but only as `u64`s. We
/// allocate string→u64 mappings ourselves in `UsearchStore::id_to_key`, so
/// without this sidecar the loaded index would have orphaned keys.
/// What: `id_to_key` is the authoritative mapping; `next_key` is the
/// monotonic counter so post-restore inserts never collide with restored
/// keys.
/// Test: `tests::test_save_load_roundtrip` exercises this.
#[derive(Debug, Serialize, Deserialize)]
struct StoreKeyMap {
    id_to_key: HashMap<String, u64>,
    next_key: u64,
    dim: usize,
}

/// Initial reserved capacity for a new HNSW index. Grows geometrically on demand.
///
/// Why (memory fix): we register one HNSW index per project, and the daemon
/// is expected to hold hundreds of them resident (~243 on the reference
/// host). usearch's `reserve()` pre-allocates the HNSW arena tape for the
/// requested slot count even when the index is empty — at ~20 MB of overhead
/// per empty 1 024-slot arena, 238 unused indexes would burn ~4.8 GB of RSS
/// before a single chunk is added. New indexes are almost always empty at
/// creation time and grow on demand via `ensure_capacity` (geometric ×2
/// growth), so a tiny initial reserve is enough to avoid pathological reserve
/// churn on the first batch of inserts without burning RAM on cold indexes.
const INITIAL_CAPACITY: usize = 64;

/// Default hard cap on the HNSW index size. The usearch `IndexOptions` API
/// (v2.25) does not expose a `max_elements` field directly, so we enforce the
/// cap in `ensure_capacity` / `upsert_batch`: once the index would grow past
/// this many vectors, subsequent inserts return an error so the daemon can
/// bound RAM (~6 GB at 1M × 384-dim × 4 bytes plus graph overhead).
const DEFAULT_HNSW_MAX_ELEMENTS: usize = 1_000_000;

/// Read the HNSW max-elements cap from the environment, with a sane default.
/// Shared with `TRUSTY_MAX_CHUNKS` so a single knob bounds both the chunk
/// corpus and the vector store.
fn hnsw_max_elements() -> usize {
    std::env::var("TRUSTY_MAX_CHUNKS")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|&n: &usize| n > 0)
        .unwrap_or(DEFAULT_HNSW_MAX_ELEMENTS)
}

/// Classify an embedding vector as safe to insert into a cosine-metric HNSW.
///
/// Why (issue #128): the CoreML execution provider intermittently emits NaN
/// or all-zero embedding vectors for a small fraction of chunks. usearch's
/// cosine metric divides by the vector norm, so an all-zero vector yields
/// `NaN` distances that poison every subsequent nearest-neighbour query, and
/// a NaN component does the same. Neither is reliably rejected by usearch's
/// `add`, so the batch-upsert path must screen vectors itself rather than
/// trusting the backend to fail loudly.
/// What: returns `Err(reason)` when the vector contains a non-finite
/// component (NaN / ±Inf) or has an effectively-zero L2 norm; otherwise
/// `Ok(())`. The reason string is suitable for a `warn` log.
/// Test: `tests::test_upsert_batch_isolates_bad_vector` feeds a NaN and a
/// zero vector through `upsert_batch` and asserts the good vectors survive.
fn validate_embedding(v: &[f32]) -> std::result::Result<(), &'static str> {
    let mut sum_sq = 0.0f32;
    for &x in v {
        if !x.is_finite() {
            return Err("contains a non-finite component (NaN or infinity)");
        }
        sum_sq += x * x;
    }
    // A cosine-metric index cannot normalise a zero vector; treat anything
    // below this tiny threshold as degenerate.
    if sum_sq < 1e-12 {
        return Err("is an all-zero (degenerate) vector");
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub struct VectorHit {
    pub chunk_id: String,
    pub score: f32,
}

/// Abstract vector store interface. Concrete impls (in-process HNSW today,
/// possibly remote tomorrow) plug in here so the rest of the indexer never
/// imports `usearch` directly.
///
/// Why: Decouples the indexer from any specific ANN backend so we can swap
/// implementations (mocks for tests, remote services for sharding) without
/// touching call sites.
/// What: Async upsert/search/remove/len over `(String chunk_id, Vec<f32>)`.
/// Test: See `UsearchStore` tests below — exercise upsert, search ordering,
/// remove, and len through this trait.
#[async_trait]
#[allow(clippy::len_without_is_empty)]
pub trait VectorStore: Send + Sync {
    async fn upsert(&self, id: &str, embedding: Vec<f32>) -> Result<()>;
    async fn search(&self, query: &[f32], top_k: usize) -> Result<Vec<VectorHit>>;
    async fn remove(&self, id: &str) -> Result<()>;
    async fn len(&self) -> Result<usize>;

    /// Bulk-upsert many `(chunk_id, embedding)` pairs.
    ///
    /// Why: per-chunk `upsert` acquires three write locks (`id_to_key`,
    /// `key_to_id`, `index`) for each call. On a 115k-chunk index that's
    /// ~345k lock round-trips and serializes the entire embed pipeline behind
    /// the HNSW write lock. Concrete impls should override to do all key
    /// allocation and all HNSW writes under a single lock acquisition each.
    /// What: default implementation loops over `upsert` so non-Usearch backends
    /// keep working; `UsearchStore` overrides for the fast path.
    /// Test: see `test_upsert_batch_inserts_all` in this module.
    async fn upsert_batch(&self, items: &[(String, Vec<f32>)]) -> Result<()> {
        for (id, vec) in items {
            self.upsert(id, vec.clone()).await?;
        }
        Ok(())
    }

    /// Persist this store to disk. Default = no-op (in-memory backends).
    ///
    /// Why: lets `CodeIndexer::save_to_disk` call through a `dyn VectorStore`
    /// without downcasting. `UsearchStore` overrides; mock test stores keep
    /// the no-op so they round-trip without filesystem access.
    /// What: persist whatever state is needed to restore via `load_from`.
    /// Test: covered by `UsearchStore::test_save_load_roundtrip`.
    async fn save_to(&self, _path: &Path) -> Result<()> {
        Ok(())
    }

    /// Rewrite the in-memory chunk-ID → u64 key mapping from absolute to
    /// root-relative paths, returning the number of keys rewritten.
    ///
    /// Why (M003 — issue #402 phase 2): M002 rewrites the redb corpus to
    /// relative paths but leaves `hnsw.keys.json` untouched. At query time
    /// vector search returns absolute HNSW chunk IDs, which are no longer
    /// present in redb (now relative), producing 0 vector results on every
    /// migrated legacy index. This method rewrites the in-memory `id_to_key`
    /// and `key_to_id` maps so subsequent searches emit relative IDs that
    /// match the redb corpus. Callers are responsible for persisting the
    /// updated sidecar via `save_to`. Default = no-op (mock / BM25-only stores).
    /// What: for each absolute ID that shares `root_path` as a prefix, strips
    /// the prefix to produce a relative ID, swaps the maps, and returns the
    /// count of rewritten entries. Already-relative IDs are left unchanged
    /// (idempotency). IDs that are absolute but outside `root_path` are left
    /// unchanged and logged at warn.
    /// Test: `test_rewrite_keys_to_relative` in `store::tests`.
    async fn rewrite_keys_to_relative(&self, _root_path: &Path) -> Result<usize> {
        Ok(0)
    }
}

/// `UsearchStore`: usearch HNSW index wrapped in `Arc<RwLock<>>` for concurrent reads.
///
/// Why: The HNSW graph is shared across many concurrent search requests; reader-priority
/// locking lets searches run in parallel and keeps the daemon's p50 latency low.
/// What: Maps `String` chunk IDs ↔ `u64` usearch keys, manages capacity growth, and
/// translates cosine distances back into similarity scores (`1 - d`) so callers see
/// "higher = better" like the rest of the pipeline.
/// Test: `tests::test_upsert_and_search` adds three vectors and asserts the exact-match
/// vector ranks first; `test_remove` and `test_concurrent_reads` cover lifecycle and
/// reader parallelism.
pub struct UsearchStore {
    index: Arc<RwLock<Index>>,
    /// chunk_id → usearch u64 key
    id_to_key: Arc<RwLock<HashMap<String, u64>>>,
    /// usearch u64 key → chunk_id (needed to translate `Matches.keys` back to strings)
    key_to_id: Arc<RwLock<HashMap<u64, String>>>,
    /// Monotonic key generator. Never reused, even after `remove`, so KG/BM25 layers
    /// that may still hold a stale key can't accidentally collide with a fresh insert.
    next_key: Arc<AtomicU64>,
    dim: usize,
    /// `true` when the underlying HNSW was opened via `Index::view` (mmap, read-only)
    /// rather than `Index::load` (heap copy). Mutating operations must promote the
    /// index to a mutable copy first via [`Self::promote_view_to_mutable`].
    ///
    /// Why (memory fix): warm-boot of N projects used to call `Index::load` for every
    /// snapshot, copying the entire HNSW arena into heap RAM even though most indexes
    /// are never written to in a given session (queries are read-only). Opening with
    /// `view` keeps the file mapped and lets the OS page cache decide which pages
    /// stay resident, dropping warm-boot RSS from ~40 GB to a small fraction of that.
    /// What: read by [`Self::ensure_mutable`] before every write path (`upsert`,
    /// `upsert_batch`, `remove`, `save`) so the first mutation transparently reloads
    /// the index in mutable mode. Stored as `AtomicBool` so the read path needs no
    /// extra lock.
    is_view: Arc<AtomicBool>,
    /// Path the index was loaded from (only set on `load_from`). Required so a
    /// later mutation can promote a view-mode index to mutable by re-reading the
    /// same file via `Index::load`.
    hnsw_path: Arc<RwLock<Option<PathBuf>>>,
}

impl UsearchStore {
    /// Construct an empty HNSW index for `dim`-dimensional cosine-similarity vectors.
    ///
    /// Why: All-MiniLM-L6-v2 produces 384-dim embeddings; cosine is the standard
    /// similarity metric for sentence embeddings.
    /// What: Builds a usearch `Index` with `MetricKind::Cos` + `ScalarKind::F32`,
    /// reserves `INITIAL_CAPACITY` slots, and wires up the bidirectional ID map.
    /// Test: `test_len` constructs a fresh store and asserts `len() == 0`.
    pub fn new(dim: usize) -> Result<Self> {
        Self::with_capacity_hint(dim, INITIAL_CAPACITY)
    }

    /// Construct with an estimated final size. When `expected_chunks > 50_000`
    /// we tune the HNSW graph for higher recall (higher `connectivity` /
    /// `expansion_add`) at the cost of more memory and slower build —
    /// worthwhile on large monorepos where the default `connectivity=16`
    /// produces noisier neighbour lists. Smaller indexes keep usearch's
    /// auto-defaults (0 = library-chosen).
    pub fn with_capacity_hint(dim: usize, expected_chunks: usize) -> Result<Self> {
        let (connectivity, expansion_add, expansion_search) = if expected_chunks > 50_000 {
            (32, 128, 64)
        } else {
            (0, 0, 0)
        };
        let options = IndexOptions {
            dimensions: dim,
            metric: MetricKind::Cos,
            quantization: ScalarKind::F32,
            connectivity,
            expansion_add,
            expansion_search,
            multi: false,
        };
        let index = Index::new(&options).map_err(|e| anyhow!("usearch Index::new failed: {e}"))?;
        // Clamp initial reserve to the env-configured max so a runaway
        // `expected_chunks` doesn't pre-allocate hundreds of GB.
        let initial = expected_chunks
            .max(INITIAL_CAPACITY)
            .min(hnsw_max_elements());
        index
            .reserve(initial)
            .map_err(|e| anyhow!("usearch reserve failed: {e}"))?;

        Ok(Self {
            index: Arc::new(RwLock::new(index)),
            id_to_key: Arc::new(RwLock::new(HashMap::new())),
            key_to_id: Arc::new(RwLock::new(HashMap::new())),
            next_key: Arc::new(AtomicU64::new(1)), // start at 1; reserve 0 as sentinel
            dim,
            // A fresh `new` / `with_capacity_hint` index is always mutable; only
            // `load_from` flips this to `true`.
            is_view: Arc::new(AtomicBool::new(false)),
            hnsw_path: Arc::new(RwLock::new(None)),
        })
    }

    /// Vector dimensionality this store was built for.
    pub fn dim(&self) -> usize {
        self.dim
    }

    /// Persist the HNSW graph and the `chunk_id → u64 key` sidecar to disk.
    ///
    /// Why (issue #85): on graceful shutdown (and incrementally after each
    /// `commit_parsed_batch`) we save the in-memory HNSW so the daemon can
    /// warm-boot without re-embedding the entire corpus. Without this every
    /// restart costs minutes of re-indexing.
    /// What: snapshots `id_to_key` + `next_key` under read locks, releases the
    /// locks, then calls usearch's `Index::save(&str)` and writes the sidecar
    /// JSON. Both writes are atomic (tmp + rename) so a crash mid-save never
    /// leaves a partial file. The caller passes the HNSW path; the sidecar is
    /// written next to it with extension `.keys.json`.
    /// Test: `tests::test_save_load_roundtrip` saves then loads into a fresh
    /// store and asserts a search still returns the original chunk_ids.
    pub async fn save(&self, hnsw_path: &Path) -> Result<()> {
        // Fast path: in view mode the in-memory state is a mmap of the
        // on-disk snapshot — there is nothing dirty to flush. Skipping the
        // save here avoids rewriting the same bytes on every shutdown and
        // sidesteps usearch's save path entirely while the index is mapped.
        // The sidecar is also already on disk and matches the live keymap.
        if self.is_view.load(Ordering::Acquire) {
            let same_path = {
                let guard = self.hnsw_path.read().await;
                guard.as_deref() == Some(hnsw_path)
            };
            if same_path {
                tracing::debug!(
                    "usearch: skipping save for {} — index is in view mode, snapshot is clean",
                    hnsw_path.display()
                );
                return Ok(());
            }
            // Save was requested to a different path than the view source.
            // Promote first so we can actually write the index out.
            self.ensure_mutable().await?;
        }

        // Snapshot the key map under read locks so we can release them before
        // the (possibly slow) usearch save. The HNSW write lock is required
        // because usearch's save is `&self` but mutates internal serializer
        // buffers; treating it as a write-side operation matches the rest of
        // this store.
        let key_map = {
            let id_to_key = self.id_to_key.read().await;
            StoreKeyMap {
                id_to_key: id_to_key.clone(),
                next_key: self.next_key.load(Ordering::Relaxed),
                dim: self.dim,
            }
        };

        if let Some(parent) = hnsw_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| anyhow!("create parent of {}: {e}", hnsw_path.display()))?;
        }

        // usearch's `save` takes a `&str` path. We write to a tmp file and
        // rename so callers never observe a half-written snapshot.
        let tmp_hnsw = hnsw_path.with_extension("usearch.tmp");
        let tmp_hnsw_str = tmp_hnsw
            .to_str()
            .ok_or_else(|| anyhow!("non-utf8 path: {}", tmp_hnsw.display()))?;
        {
            let index = self.index.write().await;
            index
                .save(tmp_hnsw_str)
                .map_err(|e| anyhow!("usearch save failed: {e}"))?;
        }
        std::fs::rename(&tmp_hnsw, hnsw_path).map_err(|e| anyhow!("rename hnsw snapshot: {e}"))?;

        let sidecar = hnsw_path.with_extension("keys.json");
        let sidecar_tmp = sidecar.with_extension("json.tmp");
        let json =
            serde_json::to_vec(&key_map).map_err(|e| anyhow!("serialize hnsw key map: {e}"))?;
        std::fs::write(&sidecar_tmp, &json)
            .map_err(|e| anyhow!("write hnsw key sidecar tmp: {e}"))?;
        std::fs::rename(&sidecar_tmp, &sidecar)
            .map_err(|e| anyhow!("rename hnsw key sidecar: {e}"))?;
        Ok(())
    }

    /// Load a previously-saved HNSW snapshot and its key sidecar.
    ///
    /// Why: counterpart of [`Self::save`]. On daemon startup or `create_index`
    /// the registered index can boot with its vectors restored — no re-embed
    /// pass required.
    ///
    /// Why (memory fix): the daemon holds one HNSW index per project resident
    /// at the same time (~243 on the reference host). The previous
    /// implementation called `Index::load`, which deserializes the entire
    /// graph + vector arena into heap RAM. That pushed warm-boot RSS to ~40 GB
    /// even though most of those indexes never serve a single write in a
    /// given session. This path now uses `Index::view`, which memory-maps the
    /// snapshot file and lets the OS page cache lazily fault in pages as
    /// they're actually touched by search. The first write to any given index
    /// transparently promotes its in-memory state back to a mutable copy via
    /// [`Self::ensure_mutable`] (see `upsert`, `upsert_batch`, `remove`).
    ///
    /// What: builds a fresh `Index` with options matching `with_capacity_hint`,
    /// calls usearch's `view(path)`, then reads the sidecar to restore the
    /// string ↔ key mappings and `next_key`. The store is marked
    /// `is_view = true` and remembers the file path so a later mutation can
    /// reload the index in mutable mode. If either file is missing or
    /// corrupt, returns `Ok(None)` so callers fall back to a fresh empty
    /// store instead of crashing the daemon.
    /// Test: `tests::test_save_load_roundtrip` covers the happy path; a
    /// `tests::test_load_missing_returns_none` covers the absent-file branch;
    /// `tests::test_view_promotes_to_mutable_on_write` exercises the
    /// view → mutable promotion when a load is followed by an upsert.
    pub async fn load_from(hnsw_path: &Path) -> Result<Option<Self>> {
        let sidecar = hnsw_path.with_extension("keys.json");
        if !hnsw_path.exists() || !sidecar.exists() {
            return Ok(None);
        }

        let json = match std::fs::read(&sidecar) {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!(
                    "could not read hnsw key sidecar {}: {e} — discarding snapshot",
                    sidecar.display()
                );
                return Ok(None);
            }
        };
        let key_map: StoreKeyMap = match serde_json::from_slice(&json) {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!(
                    "hnsw key sidecar {} is corrupt ({e}) — discarding snapshot",
                    sidecar.display()
                );
                return Ok(None);
            }
        };

        let expected_chunks = key_map.id_to_key.len();
        let store = Self::with_capacity_hint(key_map.dim, expected_chunks)?;
        let hnsw_str = match hnsw_path.to_str() {
            Some(s) => s,
            None => {
                tracing::warn!(
                    "non-utf8 hnsw path {} — discarding snapshot",
                    hnsw_path.display()
                );
                return Ok(None);
            }
        };
        {
            let index = store.index.write().await;
            // Use `view` instead of `load` so the snapshot is memory-mapped,
            // not copied into heap RAM. The OS page cache then services
            // read-only search traffic without inflating RSS. The first
            // write transparently promotes the index back to a mutable copy
            // via `ensure_mutable` / `promote_view_to_mutable`.
            if let Err(e) = index.view(hnsw_str) {
                tracing::warn!(
                    "usearch failed to view {} ({e}) — discarding snapshot",
                    hnsw_path.display()
                );
                return Ok(None);
            }
            // NOTE: `reserve` is a mutation and would invalidate the view.
            // We intentionally skip it here — the eventual `ensure_mutable`
            // call will reload the file in mutable mode and reserve to the
            // restored size on first write.
        }
        store.is_view.store(true, Ordering::Release);
        *store.hnsw_path.write().await = Some(hnsw_path.to_path_buf());

        // Rehydrate the mappings.
        {
            let mut id_map = store.id_to_key.write().await;
            let mut key_map_rev = store.key_to_id.write().await;
            for (id, key) in &key_map.id_to_key {
                id_map.insert(id.clone(), *key);
                key_map_rev.insert(*key, id.clone());
            }
        }
        store
            .next_key
            .store(key_map.next_key.max(1), Ordering::Relaxed);
        Ok(Some(store))
    }

    /// If the index is currently in view (mmap, read-only) mode, reload it
    /// from its source file in mutable mode so subsequent writes can mutate
    /// the graph.
    ///
    /// Why: `load_from` opens snapshots via `Index::view` so warm-boot RSS
    /// stays small (see [`Self::load_from`] docs). usearch's view is
    /// strictly read-only — calling `add` / `remove` / `reserve` on a
    /// view-mode index will either error or undefined-behave. The first
    /// mutating call must therefore promote the in-memory state by
    /// re-reading the source file via `Index::load`, which deserialises the
    /// graph into a heap-resident, mutable copy. From that point on this
    /// store behaves identically to one built via `new` and never enters
    /// view mode again.
    /// What: relaxed-load the `is_view` flag (fast path). When set, acquire
    /// the HNSW write lock, call `Index::load(hnsw_path)`, reserve enough
    /// capacity for the restored size, and clear `is_view`. The flag is
    /// double-checked under the write lock so concurrent writers race to
    /// promote at most once. Returns `Err` if the file is no longer
    /// readable or the source path was never recorded.
    /// Test: `tests::test_view_promotes_to_mutable_on_write`.
    async fn ensure_mutable(&self) -> Result<()> {
        // Fast path — the overwhelmingly common case is a fresh / already
        // promoted store. Acquire-load pairs with the `Release` store in
        // `load_from` and in `promote_view_to_mutable`.
        if !self.is_view.load(Ordering::Acquire) {
            return Ok(());
        }
        self.promote_view_to_mutable().await
    }

    /// Slow-path of [`Self::ensure_mutable`]. Pulled out so the hot read
    /// path stays branch-light.
    async fn promote_view_to_mutable(&self) -> Result<()> {
        let path = {
            let guard = self.hnsw_path.read().await;
            guard.clone()
        };
        let path = path.ok_or_else(|| {
            anyhow!("usearch index is in view mode but has no source path to promote from")
        })?;
        let path_str = path
            .to_str()
            .ok_or_else(|| anyhow!("non-utf8 hnsw path: {}", path.display()))?
            .to_string();

        let index = self.index.write().await;
        // Double-check under the write lock so racing writers promote once.
        if !self.is_view.load(Ordering::Acquire) {
            return Ok(());
        }
        index
            .load(&path_str)
            .map_err(|e| anyhow!("usearch failed to promote view → mutable load: {e}"))?;
        let size = index.size();
        if index.capacity() < size {
            index
                .reserve(size.max(INITIAL_CAPACITY))
                .map_err(|e| anyhow!("usearch reserve after promote failed: {e}"))?;
        }
        self.is_view.store(false, Ordering::Release);
        tracing::info!(
            "usearch: promoted view → mutable for {} ({} vectors)",
            path.display(),
            size
        );
        Ok(())
    }

    /// Ensure the underlying HNSW has room for at least one more vector.
    /// Grows geometrically (×2) to amortize the cost of reserve calls. Refuses
    /// to grow past `hnsw_max_elements()` so the daemon's RAM is bounded
    /// (issue #75).
    fn ensure_capacity(index: &Index) -> Result<()> {
        let size = index.size();
        let cap = index.capacity();
        let max_elem = hnsw_max_elements();
        if size >= max_elem {
            return Err(anyhow!(
                "usearch index at TRUSTY_MAX_CHUNKS cap ({} elements) — refusing further upserts",
                max_elem
            ));
        }
        if size + 1 > cap {
            let mut new_cap = (cap.max(1)).saturating_mul(2);
            if new_cap > max_elem {
                new_cap = max_elem;
            }
            index
                .reserve(new_cap)
                .map_err(|e| anyhow!("usearch reserve grow failed: {e}"))?;
        }
        Ok(())
    }
}

#[async_trait]
impl VectorStore for UsearchStore {
    async fn upsert(&self, id: &str, embedding: Vec<f32>) -> Result<()> {
        if embedding.len() != self.dim {
            return Err(anyhow!(
                "embedding dim mismatch: got {}, expected {}",
                embedding.len(),
                self.dim
            ));
        }

        // Promote view → mutable on first write. No-op when already mutable.
        self.ensure_mutable().await?;

        // Resolve or allocate the u64 key under a write lock.
        let key = {
            let mut id_to_key = self.id_to_key.write().await;
            if let Some(&existing) = id_to_key.get(id) {
                existing
            } else {
                let key = self.next_key.fetch_add(1, Ordering::Relaxed);
                id_to_key.insert(id.to_string(), key);
                self.key_to_id.write().await.insert(key, id.to_string());
                key
            }
        };

        let index = self.index.write().await;

        // If the key already existed, remove the old vector first so `add` doesn't
        // collide. usearch's `multi=false` index treats duplicate keys as errors.
        if index.contains(key) {
            index
                .remove(key)
                .map_err(|e| anyhow!("usearch remove (for upsert) failed: {e}"))?;
        }

        Self::ensure_capacity(&index)?;
        index
            .add(key, &embedding)
            .map_err(|e| anyhow!("usearch add failed: {e}"))?;
        Ok(())
    }

    async fn search(&self, query: &[f32], top_k: usize) -> Result<Vec<VectorHit>> {
        if query.len() != self.dim {
            return Err(anyhow!(
                "query dim mismatch: got {}, expected {}",
                query.len(),
                self.dim
            ));
        }
        if top_k == 0 {
            return Ok(Vec::new());
        }

        let matches = {
            let index = self.index.read().await;
            index
                .search(query, top_k)
                .map_err(|e| anyhow!("usearch search failed: {e}"))?
        };

        let key_to_id = self.key_to_id.read().await;
        let mut hits = Vec::with_capacity(matches.keys.len());
        for (key, dist) in matches.keys.iter().zip(matches.distances.iter()) {
            if let Some(chunk_id) = key_to_id.get(key) {
                // Cosine distance ∈ [0, 2]; convert to similarity ∈ [-1, 1] so callers
                // can RRF/fuse with BM25 scores where "higher = better".
                let score = 1.0 - *dist;
                hits.push(VectorHit {
                    chunk_id: chunk_id.clone(),
                    score,
                });
            }
            // Silently skip orphaned keys (e.g. removed mid-search) — the alternative
            // of erroring would tear down a valid query for a benign race.
        }
        Ok(hits)
    }

    async fn remove(&self, id: &str) -> Result<()> {
        // Promote view → mutable on first write. No-op when already mutable.
        self.ensure_mutable().await?;
        let key = {
            let mut id_to_key = self.id_to_key.write().await;
            match id_to_key.remove(id) {
                Some(k) => k,
                None => return Ok(()), // idempotent: removing an unknown id is a no-op
            }
        };
        self.key_to_id.write().await.remove(&key);

        let index = self.index.write().await;
        if index.contains(key) {
            index
                .remove(key)
                .map_err(|e| anyhow!("usearch remove failed: {e}"))?;
        }
        Ok(())
    }

    async fn len(&self) -> Result<usize> {
        Ok(self.index.read().await.size())
    }

    async fn save_to(&self, path: &Path) -> Result<()> {
        self.save(path).await
    }

    /// Rewrite in-memory `id_to_key` / `key_to_id` maps from absolute to
    /// root-relative chunk IDs. Returns the count of entries rewritten.
    ///
    /// Why: M003 needs to fix the HNSW key maps after M002 relativized redb.
    /// See [`VectorStore::rewrite_keys_to_relative`] for the full rationale.
    /// What: under a single write-lock pair on `id_to_key` and `key_to_id`,
    /// iterates every entry whose string key is absolute and shares `root_path`
    /// as a prefix, strips the prefix (mirrors M002's `strip_prefix` logic),
    /// replaces the entry in both maps. Idempotent: already-relative IDs are
    /// skipped. Outside-root absolute IDs are left unchanged and logged at warn.
    /// Test: `tests::test_rewrite_keys_to_relative`.
    async fn rewrite_keys_to_relative(&self, root_path: &Path) -> Result<usize> {
        let mut id_map = self.id_to_key.write().await;
        let mut key_map = self.key_to_id.write().await;

        // Collect rewrites first to avoid mutating the map while iterating.
        // Each entry: (old_absolute_id, new_relative_id, u64_key).
        let mut rewrites: Vec<(String, String, u64)> = Vec::new();
        // Compute root prefix string once, outside the loop.
        let root_prefix = root_path.to_string_lossy();

        for (id, &key) in id_map.iter() {
            if !std::path::Path::new(id).is_absolute() {
                // Already relative — idempotency: leave unchanged.
                continue;
            }
            // Try to strip root_path from the absolute chunk ID. Chunk IDs
            // have the format "{file_path}:{start}:{end}". On POSIX, ':'
            // is a valid path character so `Path::strip_prefix` treats the
            // entire ID string as a single path and strips the prefix
            // correctly. We then do a raw string prefix-swap (instead of
            // trusting the Path result's `to_string_lossy`) to preserve the
            // exact ":{start}:{end}" suffix bytes without re-encoding.
            match std::path::Path::new(id.as_str()).strip_prefix(root_path) {
                Ok(_) => {
                    // ID is under root_path. Compute the relative ID by
                    // stripping the root prefix as a raw string and trimming
                    // the leading separator.
                    let new_id = id
                        .strip_prefix(root_prefix.as_ref())
                        .map(|s| s.trim_start_matches('/').to_string())
                        .unwrap_or_else(|| id.clone());
                    rewrites.push((id.clone(), new_id, key));
                }
                Err(_) => {
                    tracing::warn!(
                        id = %id,
                        root = %root_path.display(),
                        "M003: HNSW key is absolute but not under root_path; skipping"
                    );
                }
            }
        }

        let count = rewrites.len();
        for (old_id, new_id, key) in rewrites {
            id_map.remove(&old_id);
            id_map.insert(new_id.clone(), key);
            key_map.insert(key, new_id);
        }

        // The in-memory maps now differ from the on-disk sidecar: clear the
        // view flag so `save()` does not treat the snapshot as clean and skip
        // the flush. We use `Release` ordering to pair with the `Acquire` load
        // in `save()` — the updated map state must be visible before save reads it.
        if count > 0 {
            self.is_view.store(false, Ordering::Release);
        }

        Ok(count)
    }

    /// Bulk-upsert override that minimises the time the HNSW write lock is held.
    ///
    /// Why: per-vector `upsert` acquires three write locks (`id_to_key`,
    /// `key_to_id`, `index`) for each call, and the original batch path held
    /// the HNSW write lock while calling `index.contains()` on every key —
    /// blocking concurrent searches for the entire batch. For a 640-vector
    /// batch on a hot daemon that was ~640 sequential C FFI calls (plus the
    /// pre-existence probe) under exclusive lock, which serialised all
    /// concurrent queries behind the indexer. usearch 2.25's Rust API exposes
    /// no multi-vector batch insert, so the wins come from (a) doing the
    /// `contains` probe under a read lock so the write lock only does work,
    /// and (b) decoupling the id-map locks from the HNSW write lock so the
    /// hot loop never touches `id_to_key`.
    /// What: four phases —
    /// 1. Validate dims (no locks). 2. Allocate keys for any new IDs under a
    /// single id-map write-lock pair, then drop those locks. 3. Snapshot
    /// `(key, &embedding)` pairs and pre-compute the existing-key set under a
    /// **read** lock on the HNSW index. 4. Acquire the HNSW write lock once,
    /// reserve capacity, remove pre-existing keys, then add every vector.
    /// Search results are identical to the previous implementation because
    /// the same `(key, vector)` pairs are inserted in the same order; only
    /// lock-hold duration changes.
    ///
    /// Per-item error isolation (issue #128): a single bad embedding — most
    /// commonly a NaN or all-zero vector emitted by the CoreML execution
    /// provider — used to make `index.add` fail and abort the whole call,
    /// silently dropping every other vector in a 128-file batch. Phase 4 now
    /// isolates failures: each `add` is attempted independently, the bad
    /// chunk id is logged at `warn`, and the offending item's key map entry
    /// is rolled back so it isn't left orphaned. The remaining vectors are
    /// committed normally. The call only returns `Err` when **every** add
    /// failed, which indicates a systemic problem (corrupt index, dim drift)
    /// rather than one stray vector.
    /// Test: `tests::test_upsert_and_search`, `test_upsert_replaces_existing`,
    /// `test_concurrent_reads`, and `test_upsert_batch_isolates_bad_vector`
    /// cover ordering, idempotent overwrite, reader parallelism, and the
    /// per-item isolation path.
    async fn upsert_batch(&self, items: &[(String, Vec<f32>)]) -> Result<()> {
        if items.is_empty() {
            return Ok(());
        }
        // Promote view → mutable on first write. No-op when already mutable.
        // Done before any other work so we never half-commit against a view.
        self.ensure_mutable().await?;
        // Phase 1: validate dims up front so we don't half-commit on a bad batch.
        for (_, v) in items {
            if v.len() != self.dim {
                return Err(anyhow!(
                    "embedding dim mismatch: got {}, expected {}",
                    v.len(),
                    self.dim
                ));
            }
        }

        // Phase 2: assign keys for any new IDs under a single id-map write-lock
        // pair, then drop the locks before touching the HNSW index. Snapshot the
        // resolved keys so phases 3 and 4 don't re-acquire `id_to_key`.
        let resolved_keys: Vec<u64> = {
            let mut id_map = self.id_to_key.write().await;
            let mut key_map = self.key_to_id.write().await;
            let mut out = Vec::with_capacity(items.len());
            for (id, _) in items {
                let key = if let Some(&k) = id_map.get(id.as_str()) {
                    k
                } else {
                    let k = self.next_key.fetch_add(1, Ordering::Relaxed);
                    id_map.insert(id.clone(), k);
                    key_map.insert(k, id.clone());
                    k
                };
                out.push(key);
            }
            out
        };

        // Phase 3: under a READ lock, determine which keys already exist in the
        // HNSW so the write lock only has to do the actual mutation. Concurrent
        // searches can still run during this probe.
        let existing: std::collections::HashSet<u64> = {
            let index = self.index.read().await;
            resolved_keys
                .iter()
                .copied()
                .filter(|k| index.contains(*k))
                .collect()
        };

        // Phase 4: acquire the HNSW write lock once. Reserve capacity, remove
        // pre-existing keys, then add every vector. The write lock now only
        // does the work that actually requires exclusive access — no
        // `contains` probes inside the hot loop.
        let index = self.index.write().await;
        let max_elem = hnsw_max_elements();
        if index.size() >= max_elem {
            return Err(anyhow!(
                "usearch index at TRUSTY_MAX_CHUNKS cap ({} elements) — refusing batch upsert",
                max_elem
            ));
        }
        // Reserve once for the worst case (every item is new). `existing.len()`
        // items will be removed first, but reserving for the full batch size
        // is a safe upper bound and avoids re-entering reserve mid-loop.
        let want = index.size().saturating_add(items.len());
        if want > index.capacity() {
            let mut new_cap = index.capacity().max(1);
            while new_cap < want {
                new_cap = new_cap.saturating_mul(2);
            }
            if new_cap > max_elem {
                new_cap = max_elem;
            }
            index
                .reserve(new_cap)
                .map_err(|e| anyhow!("usearch reserve grow failed: {e}"))?;
        }
        for &key in &existing {
            index
                .remove(key)
                .map_err(|e| anyhow!("usearch remove (for upsert) failed: {e}"))?;
        }

        // Per-item error isolation (issue #128). Each vector is screened and
        // added independently: a single bad embedding (NaN / zero vector from
        // CoreML) must not abort the whole batch. We collect the chunk ids
        // of any failures, then roll their key-map entries back (below)
        // after releasing the HNSW write lock, so a failed chunk leaves no
        // orphaned `id_to_key` / `key_to_id` entry that a later search would
        // try (and fail) to resolve.
        let mut failed: Vec<(String, String)> = Vec::new();
        for (key, (id, embedding)) in resolved_keys.iter().zip(items.iter()) {
            // Screen the vector first: a NaN/zero vector is not reliably
            // rejected by usearch's `add`, but it poisons cosine search if it
            // lands in the graph. Catching it here keeps the index clean.
            if let Err(reason) = validate_embedding(embedding) {
                failed.push((id.clone(), format!("embedding {reason}")));
                continue;
            }
            if let Err(e) = index.add(*key, embedding) {
                failed.push((id.clone(), e.to_string()));
            }
        }
        // Drop the HNSW write lock before touching the id maps so we don't
        // hold two write locks at once.
        drop(index);

        if failed.is_empty() {
            return Ok(());
        }

        // Roll back the key-map entries for the failed items so they don't
        // dangle. We only remove an `id_to_key` entry when it still points at
        // the key we allocated in phase 2 — an entry that already existed
        // (and whose old vector was removed above) is left as-is rather than
        // silently deleted, since its previous state was already lost and
        // re-removing the mapping wouldn't help.
        {
            let failed_ids: std::collections::HashSet<&str> =
                failed.iter().map(|(id, _)| id.as_str()).collect();
            let mut id_map = self.id_to_key.write().await;
            let mut key_map = self.key_to_id.write().await;
            for (id, key) in resolved_keys
                .iter()
                .zip(items.iter())
                .filter(|(_, (id, _))| failed_ids.contains(id.as_str()))
                .map(|(key, (id, _))| (id, key))
            {
                if !existing.contains(key) && id_map.get(id.as_str()) == Some(key) {
                    id_map.remove(id.as_str());
                    key_map.remove(key);
                }
            }
        }

        let succeeded = items.len() - failed.len();
        for (id, err) in &failed {
            tracing::warn!(
                "usearch upsert_batch: skipped chunk '{id}' — add failed ({err}); \
                 likely a NaN or zero embedding vector. The rest of the batch was indexed."
            );
        }

        if succeeded == 0 {
            // Every add failed — this is a systemic problem (corrupt index,
            // dimension drift), not one stray vector. Surface it so the
            // reindex orchestrator can abort rather than silently produce an
            // empty index.
            return Err(anyhow!(
                "usearch upsert_batch: all {} vectors failed to add — \
                 systemic failure, not isolated bad input (first error: {})",
                items.len(),
                failed.first().map(|(_, e)| e.as_str()).unwrap_or("<none>")
            ));
        }

        tracing::warn!(
            "usearch upsert_batch: {succeeded}/{} vectors indexed; {} skipped due to \
             add failures (see warnings above)",
            items.len(),
            failed.len()
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_upsert_and_search() {
        let store = UsearchStore::new(4).expect("store init");
        let v = vec![1.0f32, 0.0, 0.0, 0.0];
        store.upsert("chunk:a", v.clone()).await.expect("upsert a");
        store
            .upsert("chunk:b", vec![0.0, 1.0, 0.0, 0.0])
            .await
            .expect("upsert b");
        store
            .upsert("chunk:c", vec![0.9, 0.1, 0.0, 0.0])
            .await
            .expect("upsert c");

        let hits = store.search(&v, 2).await.expect("search");
        assert_eq!(hits.len(), 2);
        // chunk:a should be the top hit (exact match)
        assert_eq!(hits[0].chunk_id, "chunk:a");
    }

    #[tokio::test]
    async fn test_len() {
        let store = UsearchStore::new(4).expect("store init");
        assert_eq!(store.len().await.unwrap(), 0);
        store.upsert("x", vec![1.0, 0.0, 0.0, 0.0]).await.unwrap();
        assert_eq!(store.len().await.unwrap(), 1);
    }

    #[tokio::test]
    async fn test_remove() {
        let store = UsearchStore::new(4).expect("store init");
        store
            .upsert("del-me", vec![1.0, 0.0, 0.0, 0.0])
            .await
            .unwrap();
        assert_eq!(store.len().await.unwrap(), 1);
        store.remove("del-me").await.unwrap();
        // After remove, search should not return "del-me"
        let hits = store.search(&[1.0, 0.0, 0.0, 0.0], 5).await.unwrap();
        assert!(!hits.iter().any(|h| h.chunk_id == "del-me"));
    }

    #[tokio::test]
    async fn test_concurrent_reads() {
        let store = Arc::new(UsearchStore::new(4).expect("store init"));
        store.upsert("r1", vec![1.0, 0.0, 0.0, 0.0]).await.unwrap();
        store.upsert("r2", vec![0.0, 1.0, 0.0, 0.0]).await.unwrap();

        let s1 = store.clone();
        let s2 = store.clone();
        let q = vec![1.0f32, 0.0, 0.0, 0.0];
        let (r1, r2) = tokio::join!(s1.search(&q, 2), s2.search(&q, 2));
        assert!(!r1.unwrap().is_empty());
        assert!(!r2.unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_upsert_replaces_existing() {
        // Re-upserting the same id should overwrite, not double-count.
        let store = UsearchStore::new(4).expect("store init");
        store
            .upsert("same", vec![1.0, 0.0, 0.0, 0.0])
            .await
            .unwrap();
        store
            .upsert("same", vec![0.0, 1.0, 0.0, 0.0])
            .await
            .unwrap();
        assert_eq!(store.len().await.unwrap(), 1);

        // Now its closest neighbour to (0,1,0,0) should be itself.
        let hits = store.search(&[0.0, 1.0, 0.0, 0.0], 1).await.unwrap();
        assert_eq!(hits[0].chunk_id, "same");
    }

    #[tokio::test]
    async fn test_dim_mismatch_errors() {
        let store = UsearchStore::new(4).expect("store init");
        assert!(store.upsert("bad", vec![1.0, 0.0]).await.is_err());
        assert!(store.search(&[1.0, 0.0], 1).await.is_err());
    }

    #[tokio::test]
    async fn test_upsert_batch_inserts_all() {
        let store = UsearchStore::new(4).expect("store init");
        // Use orthogonal directions so cosine sim distinguishes them (parallel
        // vectors share cosine sim of 1 regardless of magnitude).
        let dirs: [[f32; 4]; 4] = [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        let items: Vec<(String, Vec<f32>)> = (0..4)
            .map(|i| (format!("k{i}"), dirs[i].to_vec()))
            .collect();
        store.upsert_batch(&items).await.expect("batch upsert");
        assert_eq!(store.len().await.unwrap(), 4);
        // Re-batch upserting the same ids should overwrite, not duplicate.
        store.upsert_batch(&items).await.expect("re-batch upsert");
        assert_eq!(store.len().await.unwrap(), 4);
        // Top hit for k2's exact vector must be k2.
        let hits = store.search(&dirs[2], 1).await.unwrap();
        assert_eq!(hits[0].chunk_id, "k2");
    }

    #[tokio::test]
    async fn test_upsert_batch_empty_noop() {
        let store = UsearchStore::new(4).expect("store init");
        store.upsert_batch(&[]).await.unwrap();
        assert_eq!(store.len().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn test_upsert_batch_dim_mismatch_errors() {
        let store = UsearchStore::new(4).expect("store init");
        let items = vec![("bad".to_string(), vec![1.0, 0.0])];
        assert!(store.upsert_batch(&items).await.is_err());
    }

    #[test]
    fn test_validate_embedding() {
        // Healthy vector passes.
        assert!(validate_embedding(&[1.0, 0.0, 0.0, 0.0]).is_ok());
        // NaN component is rejected.
        assert!(validate_embedding(&[1.0, f32::NAN, 0.0, 0.0]).is_err());
        // Infinity is rejected.
        assert!(validate_embedding(&[f32::INFINITY, 0.0, 0.0, 0.0]).is_err());
        // All-zero (degenerate for cosine) is rejected.
        assert!(validate_embedding(&[0.0, 0.0, 0.0, 0.0]).is_err());
    }

    #[tokio::test]
    async fn test_upsert_batch_isolates_bad_vector() {
        // Issue #128: a single NaN / zero embedding in a batch must not drop
        // the whole batch. The good vectors must still be indexed and the
        // bad chunk ids must be skipped (not left as orphaned key entries).
        let store = UsearchStore::new(4).expect("store init");
        let items: Vec<(String, Vec<f32>)> = vec![
            ("good-a".to_string(), vec![1.0, 0.0, 0.0, 0.0]),
            ("nan-vec".to_string(), vec![f32::NAN, 0.0, 0.0, 0.0]),
            ("good-b".to_string(), vec![0.0, 1.0, 0.0, 0.0]),
            ("zero-vec".to_string(), vec![0.0, 0.0, 0.0, 0.0]),
            ("good-c".to_string(), vec![0.0, 0.0, 1.0, 0.0]),
        ];
        // Batch must succeed: the two bad vectors are isolated, not fatal.
        store
            .upsert_batch(&items)
            .await
            .expect("batch with isolated bad vectors must still succeed");
        // Exactly the three good vectors are in the index.
        assert_eq!(store.len().await.unwrap(), 3);
        // Each good vector is searchable and ranks itself first.
        for (id, dir) in [
            ("good-a", [1.0f32, 0.0, 0.0, 0.0]),
            ("good-b", [0.0, 1.0, 0.0, 0.0]),
            ("good-c", [0.0, 0.0, 1.0, 0.0]),
        ] {
            let hits = store.search(&dir, 1).await.unwrap();
            assert_eq!(hits[0].chunk_id, id, "good vector {id} must round-trip");
        }
        // The bad chunk ids must not resolve to anything — their key-map
        // entries were rolled back, so re-upserting them later is clean.
        store
            .upsert("nan-vec", vec![0.0, 0.0, 0.0, 1.0])
            .await
            .expect("a now-healthy 'nan-vec' must upsert without a key collision");
        assert_eq!(store.len().await.unwrap(), 4);
    }

    #[tokio::test]
    async fn test_upsert_batch_all_bad_vectors_errors() {
        // When *every* vector is bad it's a systemic failure, not isolated
        // bad input — the call must return Err so the orchestrator aborts
        // rather than silently producing an empty index.
        let store = UsearchStore::new(4).expect("store init");
        let items: Vec<(String, Vec<f32>)> = vec![
            ("nan-1".to_string(), vec![f32::NAN, 0.0, 0.0, 0.0]),
            ("zero-2".to_string(), vec![0.0, 0.0, 0.0, 0.0]),
        ];
        assert!(
            store.upsert_batch(&items).await.is_err(),
            "an all-bad batch must surface an error"
        );
        assert_eq!(store.len().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn test_save_load_roundtrip() {
        // Why: validate the persistence path end-to-end so issue #85 actually
        // survives a "restart" (simulated here by dropping the store and
        // loading the snapshot into a fresh one).
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hnsw.usearch");

        let store = UsearchStore::new(4).unwrap();
        store
            .upsert("alpha", vec![1.0, 0.0, 0.0, 0.0])
            .await
            .unwrap();
        store
            .upsert("beta", vec![0.0, 1.0, 0.0, 0.0])
            .await
            .unwrap();
        store.save(&path).await.expect("save");
        assert!(path.exists(), "hnsw file must exist after save");
        assert!(
            path.with_extension("keys.json").exists(),
            "key sidecar must exist after save"
        );

        drop(store);

        let loaded = UsearchStore::load_from(&path)
            .await
            .expect("load ok")
            .expect("load returned Some");
        assert_eq!(loaded.len().await.unwrap(), 2);
        let hits = loaded.search(&[1.0, 0.0, 0.0, 0.0], 1).await.unwrap();
        assert_eq!(hits[0].chunk_id, "alpha", "restored ids must round-trip");
    }

    #[tokio::test]
    async fn test_load_missing_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nope.usearch");
        let loaded = UsearchStore::load_from(&path).await.unwrap();
        assert!(loaded.is_none());
    }

    #[tokio::test]
    async fn test_load_corrupt_sidecar_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hnsw.usearch");
        // Create both files but corrupt the sidecar.
        let store = UsearchStore::new(4).unwrap();
        store.upsert("a", vec![1.0, 0.0, 0.0, 0.0]).await.unwrap();
        store.save(&path).await.unwrap();
        std::fs::write(path.with_extension("keys.json"), b"not valid json").unwrap();
        let loaded = UsearchStore::load_from(&path).await.unwrap();
        assert!(loaded.is_none(), "corrupt sidecar must fall back to None");
    }

    #[tokio::test]
    async fn test_view_promotes_to_mutable_on_write() {
        // Why: warm-boot opens the snapshot via `Index::view` (mmap) to keep
        // RSS low. The first write must transparently promote the index to a
        // mutable copy via `ensure_mutable` so callers don't need to know
        // which mode the store is in.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hnsw.usearch");

        // Save a snapshot with two vectors.
        let store = UsearchStore::new(4).unwrap();
        store
            .upsert("alpha", vec![1.0, 0.0, 0.0, 0.0])
            .await
            .unwrap();
        store
            .upsert("beta", vec![0.0, 1.0, 0.0, 0.0])
            .await
            .unwrap();
        store.save(&path).await.expect("save");
        drop(store);

        // Reopen via `load_from` — should land in view mode.
        let loaded = UsearchStore::load_from(&path)
            .await
            .expect("load ok")
            .expect("load returned Some");
        assert!(
            loaded.is_view.load(Ordering::Acquire),
            "load_from must put the store in view mode for the memory fix"
        );

        // A read-only search must work without promotion.
        let hits = loaded.search(&[1.0, 0.0, 0.0, 0.0], 1).await.unwrap();
        assert_eq!(hits[0].chunk_id, "alpha");
        assert!(
            loaded.is_view.load(Ordering::Acquire),
            "search must not promote view → mutable"
        );

        // First write must promote, and the prior content must survive.
        loaded
            .upsert("gamma", vec![0.0, 0.0, 1.0, 0.0])
            .await
            .expect("upsert after view");
        assert!(
            !loaded.is_view.load(Ordering::Acquire),
            "first write must promote view → mutable"
        );
        assert_eq!(loaded.len().await.unwrap(), 3);

        // Subsequent writes must remain on the mutable path.
        loaded
            .upsert("delta", vec![0.0, 0.0, 0.0, 1.0])
            .await
            .expect("upsert after promote");
        assert_eq!(loaded.len().await.unwrap(), 4);
        let hits = loaded.search(&[0.0, 0.0, 1.0, 0.0], 1).await.unwrap();
        assert_eq!(hits[0].chunk_id, "gamma");
    }

    #[tokio::test]
    async fn test_view_batch_upsert_promotes() {
        // Same as above but exercises the bulk-path `upsert_batch` seam.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hnsw.usearch");

        let store = UsearchStore::new(4).unwrap();
        store
            .upsert_batch(&[("seed".to_string(), vec![1.0, 0.0, 0.0, 0.0])])
            .await
            .unwrap();
        store.save(&path).await.unwrap();
        drop(store);

        let loaded = UsearchStore::load_from(&path).await.unwrap().unwrap();
        assert!(loaded.is_view.load(Ordering::Acquire));
        loaded
            .upsert_batch(&[("more".to_string(), vec![0.0, 1.0, 0.0, 0.0])])
            .await
            .expect("batch upsert after view");
        assert!(!loaded.is_view.load(Ordering::Acquire));
        assert_eq!(loaded.len().await.unwrap(), 2);
    }

    #[tokio::test]
    async fn test_capacity_growth() {
        // Force more inserts than INITIAL_CAPACITY would normally hold to exercise
        // the geometric reserve growth path without bloating test runtime.
        let store = UsearchStore::new(4).expect("store init");
        for i in 0..50 {
            let v = vec![i as f32, 0.0, 0.0, 0.0];
            store.upsert(&format!("k{i}"), v).await.unwrap();
        }
        assert_eq!(store.len().await.unwrap(), 50);
    }
}
