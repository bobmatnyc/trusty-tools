//! The [`CodeIndexer`] struct and its write path.
//!
//! Why: Concentrates construction, the warm-start / cool-down lifecycle, and
//! the index/remove/walk methods in one place so the read path (`search.rs`)
//! stays focused on retrieval.
//! What: Defines [`CodeIndexer`], its constructors and builders, the warm-up
//! gate and cool-down monitor (#372), and `index_file` / `remove_file` /
//! `index_directory`.
//! Test: See the `tests` submodule of the parent `indexer` module —
//! `search_returns_code_chunk_with_metadata`, the agentconfig promotion
//! tests, and the warm-start/cool-down tests.

use std::num::NonZeroUsize;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use lru::LruCache;
use tokio::sync::{Mutex as TokioMutex, Semaphore};
use walkdir::WalkDir;

use crate::memory::{Embedder, MemoryStore, Segment};
use crate::search::indexer::chunker::{
    detect_language, extract_chunks_from_source, is_hidden_or_skipped, manifest_key, truncate_chars,
};
use crate::search::indexer::{
    CodeChunk, DEFAULT_COOL_AFTER_MINUTES, MAX_CHUNK_CHARS, QUERY_CACHE_CAPACITY,
};

/// Orchestrates AST chunking + embedding + storage for source files.
///
/// Why: A single entry point (`index_file`, `index_directory`, `search`)
/// keeps callers decoupled from tree-sitter and the embedder/store details.
/// Trait objects (`Arc<dyn MemoryStore>` / `Arc<dyn Embedder>`) let tests
/// substitute mocks cheaply.
/// What: Holds shared-ownership handles to the store and embedder; the
/// tree-sitter parser is created per-call (they aren't `Sync`).
/// Test: See `search_returns_code_chunk_with_metadata`.
pub struct CodeIndexer {
    pub(crate) store: Arc<dyn MemoryStore>,
    pub(crate) embedder: Arc<dyn Embedder>,
    /// Last time `search`/`search_hybrid` was invoked.
    ///
    /// Why: Drives the cool-down timer (#372). When `Instant::now() -
    /// last_access > cool_after`, the cool-down task evicts the in-memory
    /// HNSW for `Segment::CodeIndex`. The mutex is `tokio::sync::Mutex` so
    /// the eviction task and any concurrent search can serialize without
    /// blocking the runtime.
    pub(crate) last_access: Arc<TokioMutex<Instant>>,
    /// Inactivity threshold; index is evicted after this duration with no
    /// search calls. Set to a very large value (effectively disabled) when
    /// `with_cool_after(Duration::MAX)` or in environments that prefer to
    /// keep the index permanently warm.
    pub(crate) cool_after: Duration,
    /// Per-process LRU cache mapping `query` text → embedded vector.
    ///
    /// Why: Repeat queries within a session shouldn't re-pay the
    /// FastEmbedder cost. Capacity-bounded LRU keeps memory predictable
    /// (#376 D2).
    /// What: `tokio::sync::Mutex<LruCache>` so the cache is safe to share
    /// across the search task and any future warm-up routines without
    /// blocking the runtime.
    pub(crate) query_cache: Arc<TokioMutex<LruCache<String, Vec<f32>>>>,
    /// Optional bounded semaphore that gates indexing `spawn_blocking` jobs.
    ///
    /// Why: When the search daemon is actively re-indexing a tree, fastembed
    /// ONNX inference jobs run on the tokio blocking pool. Without a cap they
    /// can saturate the pool and starve axum HTTP handler tasks, causing
    /// `/search/query` and `/search/health` to time out (#399).
    /// What: `None` for in-process callers (CLI, tests) where indexing isn't
    /// concurrent with HTTP traffic. The daemon installs a semaphore via
    /// [`with_indexing_semaphore`](CodeIndexer::with_indexing_semaphore) sized
    /// to roughly half the available parallelism so HTTP handlers always have
    /// threads to run on.
    pub(crate) indexing_permits: Option<Arc<Semaphore>>,
}

impl CodeIndexer {
    /// Construct a new `CodeIndexer` with injected store + embedder.
    ///
    /// Why: Constructor injection makes the dependencies explicit and
    /// mockable, per the project's DI conventions.
    /// What: Stores the `Arc`s, initialises `last_access` to `Instant::now()`
    /// and uses [`DEFAULT_COOL_AFTER_MINUTES`] as the inactivity threshold.
    /// Returns `Self` (no background task spawned — call
    /// [`spawn_cool_down_monitor`](CodeIndexer::spawn_cool_down_monitor) on an
    /// `Arc<Self>` to enable eviction).
    /// Test: Every indexer test calls `CodeIndexer::new(...)`.
    pub fn new(store: Arc<dyn MemoryStore>, embedder: Arc<dyn Embedder>) -> Self {
        let cap = NonZeroUsize::new(QUERY_CACHE_CAPACITY).expect("non-zero cache capacity");
        Self {
            store,
            embedder,
            last_access: Arc::new(TokioMutex::new(Instant::now())),
            cool_after: Duration::from_secs(DEFAULT_COOL_AFTER_MINUTES * 60),
            query_cache: Arc::new(TokioMutex::new(LruCache::new(cap))),
            indexing_permits: None,
        }
    }

    /// Install a bounded semaphore that gates indexing `spawn_blocking` jobs.
    ///
    /// Why: Prevents the search daemon from saturating the tokio blocking pool
    /// during active re-indexing, which previously starved HTTP handler tasks
    /// and caused `/search/query` to time out (#399).
    /// What: Builder-style; consumes self and stores the semaphore. Each
    /// indexing path (`extract_chunks`, `embed_single` during indexing)
    /// acquires one permit before running.
    /// Test: Indirectly via `start_query_stop_round_trip` (the daemon now
    /// installs this and HTTP handlers stay responsive under reindex load).
    pub fn with_indexing_semaphore(mut self, permits: Arc<Semaphore>) -> Self {
        self.indexing_permits = Some(permits);
        self
    }

    /// Override the cool-down duration (mostly for tests + the config loader).
    ///
    /// Why: The default is tuned for interactive PM sessions; tests want a
    /// sub-second window so they don't sleep for 15 minutes, and projects
    /// that disable cool-down can pass `Duration::MAX`.
    /// What: Builder-style; consumes self and returns it with `cool_after`
    /// replaced.
    /// Test: `cool_down_evicts_after_inactivity`.
    pub fn with_cool_after(mut self, cool_after: Duration) -> Self {
        self.cool_after = cool_after;
        self
    }

    /// Eagerly load the on-disk index into memory.
    ///
    /// Why: #372 warm-start requirement — as soon as `main()` starts, the
    /// PM should never serve a cold first query. This forwards to the
    /// underlying store's `warm_segment` so the HNSW is resident before
    /// any user request arrives. Stores without an evictable index treat
    /// it as a no-op (default trait impl).
    /// What: Calls `store.warm_segment(Segment::CodeIndex)`. Resets the
    /// `last_access` clock so the cool-down timer counts from "load
    /// completed", not from construction.
    /// Test: `warm_up_marks_segment_warm`.
    pub async fn warm_up(&self) -> Result<()> {
        self.store.warm_segment(Segment::CodeIndex).await?;
        *self.last_access.lock().await = Instant::now();
        Ok(())
    }

    /// Touch the access timestamp and ensure the index is loaded.
    ///
    /// Why: Every read path (`search`, `search_hybrid`) calls this so the
    /// cool-down timer slides forward and any prior eviction is reversed
    /// transparently before serving the query.
    /// What: Updates `last_access` to `Instant::now()`, then calls
    /// `store.warm_segment(Segment::CodeIndex)`. The store's default impl
    /// is a no-op; `RedbUsearchStore` rebuilds the HNSW from disk if it
    /// was previously evicted and logs a single info line.
    /// Test: `cool_down_evicts_after_inactivity` (warm path),
    /// `search_warms_index_after_eviction`.
    pub async fn ensure_warm(&self) -> Result<()> {
        *self.last_access.lock().await = Instant::now();
        self.store.warm_segment(Segment::CodeIndex).await
    }

    /// Spawn a background tokio task that evicts the code-index HNSW after
    /// `cool_after` of search inactivity.
    ///
    /// Why: Implements the cool-down half of the warm-start/cool-down
    /// contract. Lives outside `new()` because spawning requires an
    /// `Arc<Self>` so the task can hold a weak handle without preventing
    /// `CodeIndexer` from ever dropping.
    /// What: Loops with a 60-second wake interval. On each wake, checks
    /// `Instant::now() - last_access`; if it exceeds `cool_after` AND the
    /// segment is still warm, calls `store.evict_segment`. Returns the
    /// `JoinHandle` so callers can abort it on shutdown.
    /// Test: `cool_down_evicts_after_inactivity`.
    pub fn spawn_cool_down_monitor(self: &Arc<Self>) -> tokio::task::JoinHandle<()> {
        let me = Arc::clone(self);
        // Choose a tick interval that's small enough to evict promptly after
        // crossing the threshold, but large enough to be invisible on a
        // sleeping laptop. 60s is the production default; tests override
        // through the dedicated `spawn_cool_down_monitor_with_tick` helper.
        let tick = Duration::from_secs(60);
        Self::spawn_cool_down_monitor_inner(me, tick)
    }

    /// Test-friendly variant that lets callers shrink the wake interval.
    ///
    /// Why: Production wakes once a minute (cheap, plenty fast); unit tests
    /// need millisecond ticks so they don't sit idle for a minute waiting
    /// for the first wake.
    /// What: Same loop as `spawn_cool_down_monitor`, but with caller-supplied
    /// `tick` between wakes.
    /// Test: `cool_down_evicts_after_inactivity`.
    pub fn spawn_cool_down_monitor_with_tick(
        self: &Arc<Self>,
        tick: Duration,
    ) -> tokio::task::JoinHandle<()> {
        let me = Arc::clone(self);
        Self::spawn_cool_down_monitor_inner(me, tick)
    }

    fn spawn_cool_down_monitor_inner(me: Arc<Self>, tick: Duration) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(tick).await;
                let last = *me.last_access.lock().await;
                if last.elapsed() < me.cool_after {
                    continue;
                }
                // Skip the eviction call when already evicted — `is_segment_warm`
                // is cheap (just a mutex check) and avoids a redundant log line.
                // Default to `false` on error so a misbehaving store
                // can't trigger a redundant evict_segment call (#376 A4).
                let warm = me
                    .store
                    .is_segment_warm(Segment::CodeIndex)
                    .await
                    .unwrap_or(false);
                if !warm {
                    continue;
                }
                if let Err(e) = me.store.evict_segment(Segment::CodeIndex).await {
                    tracing::warn!(error = %e, "cool-down: evict_segment failed");
                }
            }
        })
    }

    /// Index a single file: read -> chunk -> embed -> insert.
    ///
    /// Why: File-granular entry point for incremental indexing (file
    /// watchers, PR diff indexing) and for the directory walker below.
    /// What: Detects the language from the extension, extracts chunks via
    /// tree-sitter (sync parse is wrapped in `spawn_blocking`), embeds each
    /// chunk's text, and inserts into [`Segment::CodeIndex`] keyed by
    /// `"{absolute_path}:{start_line}"`. Returns the count inserted.
    /// Test: Covered by `search_returns_code_chunk_with_metadata`.
    pub async fn index_file(&self, path: &Path, root: Option<&Path>) -> Result<usize> {
        let abs_path = path
            .canonicalize()
            .with_context(|| format!("failed to canonicalize {}", path.display()))?;
        let source = tokio::fs::read_to_string(&abs_path)
            .await
            .with_context(|| format!("failed to read file {}", abs_path.display()))?;
        // Canonicalize the root (if given) so comparisons against
        // `abs_path.parent()` below see the same prefix — otherwise a
        // caller-supplied relative root would never match.
        let canonical_root = root.and_then(|r| r.canonicalize().ok());
        let root_ref = canonical_root.as_deref();
        let Some(language) = detect_language(&abs_path, root_ref) else {
            tracing::debug!(file = %abs_path.display(), "skipping: unsupported extension");
            return Ok(0);
        };
        let language = language.to_string();

        // tree-sitter parsing is synchronous and potentially CPU-heavy; move
        // it off the async runtime to keep the reactor responsive. Acquire
        // an indexing permit (when configured) so the search daemon can cap
        // concurrent indexing and leave threads for HTTP handlers (#399).
        let src_clone = source.clone();
        let lang_clone = language.clone();
        let raw_chunks = {
            let _permit = match &self.indexing_permits {
                Some(sem) => Some(
                    Arc::clone(sem)
                        .acquire_owned()
                        .await
                        .context("acquiring indexing permit")?,
                ),
                None => None,
            };
            tokio::task::spawn_blocking(move || extract_chunks_from_source(&src_clone, &lang_clone))
                .await
                .context("chunk-extraction task panicked")?
        };

        // Remove any existing chunks for this file so re-indexing replaces
        // rather than accumulates. Safe to call even if nothing exists.
        let _ = self.remove_file(&abs_path).await;

        let mut inserted = 0usize;
        let mut chunk_ids: Vec<String> = Vec::with_capacity(raw_chunks.len());
        for chunk in raw_chunks {
            let chunk_text = truncate_chars(&chunk.text, MAX_CHUNK_CHARS);
            let vec = {
                let embedder = Arc::clone(&self.embedder);
                let text = chunk_text.clone();
                // Cap concurrent embedding jobs when an indexing semaphore is
                // installed so HTTP handler tasks stay responsive (#399).
                let _permit = match &self.indexing_permits {
                    Some(sem) => Some(
                        Arc::clone(sem)
                            .acquire_owned()
                            .await
                            .context("acquiring indexing permit")?,
                    ),
                    None => None,
                };
                tokio::task::spawn_blocking(move || embedder.embed_single(&text))
                    .await
                    .context("embed task panicked")??
            };
            let code_chunk = CodeChunk {
                file: abs_path.clone(),
                function_name: chunk.function_name.clone(),
                start_line: chunk.start_line,
                end_line: chunk.end_line,
                language: language.clone(),
                score: 0.0,
                text: chunk_text,
                // "stored" is the at-rest label; search paths overwrite this
                // with "vector", "hybrid", or "hybrid+kg" on retrieval (#401).
                match_reason: "stored".to_string(),
            };
            // Include end_line so two chunks that share a start line
            // (rare but possible across overlapping fallback windows) get
            // distinct keys (#376 A4).
            let id = format!(
                "{}:{}:{}",
                abs_path.display(),
                chunk.start_line,
                chunk.end_line
            );
            let payload = serde_json::to_value(&code_chunk)
                .context("failed to serialize CodeChunk to payload")?;
            self.store
                .insert(Segment::CodeIndex, &id, &vec, payload)
                .await?;
            chunk_ids.push(id);
            inserted += 1;
        }

        // Write a manifest entry tracking which chunk ids belong to this
        // file so `remove_file` can surgically delete them on change/delete.
        let manifest_id = manifest_key(&abs_path);
        let manifest_payload = serde_json::json!({ "chunk_ids": chunk_ids });
        let zero_vec = vec![0.0f32; self.embedder.dimension()];
        self.store
            .insert(
                Segment::CodeIndex,
                &manifest_id,
                &zero_vec,
                manifest_payload,
            )
            .await?;

        Ok(inserted)
    }

    /// Remove all chunks previously indexed for `path`.
    ///
    /// Why: On file delete or modify (before re-indexing) we need to evict
    /// stale entries so search results stay accurate. The per-file manifest
    /// written by [`index_file`](CodeIndexer::index_file) gives us an exact
    /// list of chunk ids to delete without scanning the full index.
    /// What: Looks up `"manifest:{abs_path}"`, iterates its `chunk_ids`,
    /// deletes each chunk, then deletes the manifest itself. Returns the
    /// number of chunks removed (0 if the file was never indexed).
    /// Test: `remove_file_deletes_manifest_and_chunks` in `watcher.rs`.
    pub async fn remove_file(&self, path: &Path) -> Result<usize> {
        // canonicalize if the file still exists; fall back to the raw path.
        let abs_path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        let manifest_id = manifest_key(&abs_path);
        let Some(manifest) = self.store.get(Segment::CodeIndex, &manifest_id).await? else {
            return Ok(0);
        };
        let chunk_ids: Vec<String> = manifest
            .get("chunk_ids")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();
        let mut removed = 0usize;
        for id in &chunk_ids {
            if self.store.delete(Segment::CodeIndex, id).await.is_ok() {
                removed += 1;
            }
        }
        // Delete manifest entry last.
        let _ = self.store.delete(Segment::CodeIndex, &manifest_id).await;
        Ok(removed)
    }

    /// Recursively index a directory, filtering by file extension.
    ///
    /// Why: Full-repo indexing is the common bootstrap path; `walkdir` with
    /// an explicit extension filter gives predictable behavior.
    /// What: Walks `root`, skipping hidden directories (`.git`, `target`,
    /// `node_modules`, and any dotted name). For each matching file,
    /// calls [`index_file`](CodeIndexer::index_file). Returns total chunks
    /// inserted.
    /// Test: Not directly covered (would require on-disk fixtures); the
    /// single-file tests exercise the same chunking + insert path.
    pub async fn index_directory(&self, root: &Path, extensions: &[&str]) -> Result<usize> {
        let mut total = 0usize;
        for entry in WalkDir::new(root)
            .follow_links(false)
            .into_iter()
            .filter_entry(|e| !is_hidden_or_skipped(e.path()))
        {
            let entry = match entry {
                Ok(e) => e,
                Err(err) => {
                    tracing::warn!(error = %err, "walkdir error; skipping");
                    continue;
                }
            };
            if !entry.file_type().is_file() {
                continue;
            }
            let Some(ext) = entry.path().extension().and_then(|e| e.to_str()) else {
                continue;
            };
            if !extensions.contains(&ext) {
                continue;
            }
            match self.index_file(entry.path(), Some(root)).await {
                Ok(n) => total += n,
                Err(e) => {
                    tracing::warn!(file = %entry.path().display(), error = %e, "index_file failed");
                }
            }
        }
        Ok(total)
    }
}
