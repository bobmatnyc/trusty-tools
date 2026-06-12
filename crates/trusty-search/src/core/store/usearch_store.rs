//! `UsearchStore`: HNSW-backed vector store with mmap warm-boot.
//!
//! Why: The HNSW graph is shared across many concurrent search requests;
//! reader-priority locking lets searches run in parallel and keeps the
//! daemon's p50 latency low.
//! What: Wraps usearch's `Index` in `Arc<RwLock<>>`, manages `String` ↔ `u64`
//! key mappings, handles capacity growth, and provides save/load with
//! mmap (view) support for low-RSS warm-boot (#709).
//! Test: see `super::tests`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use anyhow::{anyhow, Result};
use tokio::sync::RwLock;
use usearch::{Index, IndexOptions, MetricKind};

use super::super::store_config::{MmapServeMode, VectorQuant};
use super::types::StoreKeyMap;

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
pub(super) const INITIAL_CAPACITY: usize = 64;

/// Default hard cap on the HNSW index size. The usearch `IndexOptions` API
/// (v2.25) does not expose a `max_elements` field directly, so we enforce the
/// cap in `ensure_capacity` / `upsert_batch`: once the index would grow past
/// this many vectors, subsequent inserts return an error so the daemon can
/// bound RAM (~6 GB at 1M × 384-dim × 4 bytes plus graph overhead).
const DEFAULT_HNSW_MAX_ELEMENTS: usize = 1_000_000;

/// Read the HNSW max-elements cap from the environment, with a sane default.
/// Shared with `TRUSTY_MAX_CHUNKS` so a single knob bounds both the chunk
/// corpus and the vector store.
pub(super) fn hnsw_max_elements() -> usize {
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
pub(super) fn validate_embedding(v: &[f32]) -> std::result::Result<(), &'static str> {
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
    pub(super) index: Arc<RwLock<Index>>,
    /// chunk_id → usearch u64 key
    pub(super) id_to_key: Arc<RwLock<HashMap<String, u64>>>,
    /// usearch u64 key → chunk_id (needed to translate `Matches.keys` back to strings)
    pub(super) key_to_id: Arc<RwLock<HashMap<u64, String>>>,
    /// Monotonic key generator. Never reused, even after `remove`, so KG/BM25 layers
    /// that may still hold a stale key can't accidentally collide with a fresh insert.
    pub(super) next_key: Arc<AtomicU64>,
    pub(super) dim: usize,
    /// `true` when the underlying HNSW was opened via `Index::view` (mmap, read-only)
    /// rather than `Index::load` (heap copy). Mutating operations must promote the
    /// index to a mutable copy first via [`Self::promote_view_to_mutable`].
    ///
    /// Why (memory fix): warm-boot used to `Index::load` every snapshot, copying
    /// the whole HNSW arena to heap. `view` keeps it mapped (OS page cache picks
    /// residency), dropping warm-boot RSS from ~40 GB to a fraction; #709 makes
    /// this the default and adds `TRUSTY_HNSW_MMAP_SERVE` to opt out.
    /// What: read by [`Self::ensure_mutable`] before every write path (`upsert`,
    /// `upsert_batch`, `remove`, `save`) so the first mutation transparently reloads
    /// the index in mutable mode. Stored as `AtomicBool` so the read path needs no
    /// extra lock.
    pub(super) is_view: Arc<AtomicBool>,
    /// Path the index was loaded from (only set on `load_from`). Required so a
    /// later mutation can promote a view-mode index to mutable by re-reading the
    /// same file via `Index::load`.
    pub(super) hnsw_path: Arc<RwLock<Option<PathBuf>>>,
}

impl UsearchStore {
    /// Construct an empty HNSW index for `dim`-dimensional cosine-similarity vectors.
    ///
    /// Why: All-MiniLM-L6-v2 produces 384-dim embeddings; cosine is the standard
    /// similarity metric for sentence embeddings.
    /// What: Builds a usearch `Index` with `MetricKind::Cos` + env-selected `VectorQuant` precision,
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
            quantization: VectorQuant::from_env().scalar_kind(),
            connectivity,
            expansion_add,
            expansion_search,
            multi: false,
        };
        let index = Index::new(&options).map_err(|e| anyhow!("usearch Index::new failed: {e}"))?;
        // Clamp initial reserve to the env max so a runaway hint can't preallocate GBs.
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

    /// `true` while the HNSW is still served from the read-only mmap view (not
    /// yet promoted to a heap copy). Test accessor for the #709 QW#1 no-promotion
    /// invariant — lets integration tests assert the read path stays on the view.
    #[doc(hidden)]
    pub fn in_view_mode(&self) -> bool {
        self.is_view.load(Ordering::Acquire)
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
            // Use `view` (mmap) not `load` (heap copy) so RSS stays small; the OS
            // page cache services read-only search and the first write promotes
            // back to a mutable copy via `ensure_mutable` (#709 QW#1).
            if let Err(e) = index.view(hnsw_str) {
                tracing::warn!(
                    "usearch failed to view {} ({e}) — discarding snapshot",
                    hnsw_path.display()
                );
                return Ok(None);
            }
            // NOTE: `reserve` would mutate (invalidate) the view; skip it — the
            // eventual `ensure_mutable` reserves on first write.
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
        // QW#1 opt-out (#709): TRUSTY_HNSW_MMAP_SERVE=off promotes to heap now
        // (higher RSS, no cold-fault latency on EFS/NFS). Default = mmap, no-op.
        if MmapServeMode::from_env().promote_on_load() {
            store.promote_view_to_mutable().await?;
        }
        Ok(Some(store))
    }

    /// If the index is in view (mmap, read-only) mode, reload it from its source
    /// file in mutable mode so subsequent writes can mutate the graph.
    ///
    /// Why: `load_from` opens snapshots via `Index::view` so warm-boot RSS stays
    /// small. usearch's view is strictly read-only — `add`/`remove`/`reserve` on
    /// it errors or UBs. The first mutating call must promote by re-reading the
    /// source via `Index::load` (a heap-resident mutable copy); thereafter the
    /// store behaves like one built via `new` and never re-enters view mode.
    /// What: relaxed-load `is_view` (fast path); when set, take the HNSW write
    /// lock, `Index::load`, reserve to the restored size, clear `is_view`. The
    /// flag is double-checked under the write lock so racing writers promote at
    /// most once. Returns `Err` if the file is unreadable or the path is unknown.
    /// Test: `tests::test_view_promotes_to_mutable_on_write`.
    pub(super) async fn ensure_mutable(&self) -> Result<()> {
        // Fast path — fresh / already-promoted store. Acquire pairs with the
        // `Release` stores in `load_from` / `promote_view_to_mutable`.
        if !self.is_view.load(Ordering::Acquire) {
            return Ok(());
        }
        self.promote_view_to_mutable().await
    }

    /// Slow-path of [`Self::ensure_mutable`]. Pulled out so the hot read
    /// path stays branch-light.
    pub(super) async fn promote_view_to_mutable(&self) -> Result<()> {
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
    pub(super) fn ensure_capacity(index: &Index) -> Result<()> {
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
