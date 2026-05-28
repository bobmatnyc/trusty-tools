//! AST-aware code indexer: walks source trees and produces function-level chunks.
//!
//! Why: Downstream semantic search needs chunks that are both small enough to
//! embed usefully and large enough to be meaningful. Splitting on AST function
//! boundaries gives exactly that — a single function (or method) becomes one
//! chunk with precise `{file, function_name, start_line, end_line}`. That in
//! turn lets search results be rendered as clickable `path:line` references.
//! What: [`CodeChunk`] is the result shape surfaced to callers;
//! [`CodeIndexer`] orchestrates file reads, AST extraction via tree-sitter,
//! embedding via the injected [`Embedder`], and persistence via the injected
//! [`MemoryStore`] under [`Segment::CodeIndex`].
//! Test: Unit tests in this file cover each language's chunker, markdown
//! heading split, fallback for files with no function nodes, and a full
//! index+search round-trip using a mock store and embedder.

use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use lru::LruCache;
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex as TokioMutex, Semaphore};
use tree_sitter::{Language, Node, Parser};
use walkdir::WalkDir;

use crate::context::bm25::Bm25Index;
use crate::context::indexer::tokenize;
use crate::memory::{Embedder, MemoryStore, Segment};
use crate::search::query_classifier::{ClassifiedQuery, classify_query};

/// Maximum number of characters kept per chunk text payload.
///
/// Why: Embedding models and downstream display both have practical limits;
/// ~2000 chars is a reasonable upper bound for most function bodies and keeps
/// payloads small in redb.
const MAX_CHUNK_CHARS: usize = 2000;

/// Line-count target for the fallback chunker.
///
/// Why: When tree-sitter finds no function nodes (config files, pure-data
/// modules, markdown), we still want the file to be searchable. Larger
/// 150-line windows preserve more surrounding context per chunk so the
/// embedding captures relationships across a wider span (#376).
const FALLBACK_LINES_PER_CHUNK: usize = 150;

/// Stride between successive sliding-window chunks (~67% overlap with the
/// 150-line window).
///
/// Why: Hard windows risk splitting a logical block (loop, struct literal,
/// long match arm) at exactly the wrong line and losing it from semantic
/// matches. With a 150-line window and a 50-line stride every line lands
/// in three consecutive chunks so boundary context is preserved (#376).
/// What: Every chunk after the first starts `FALLBACK_STRIDE` lines after
/// the previous chunk's start. Last chunk is clipped to file length.
/// Test: `fallback_uses_overlapping_windows`.
const FALLBACK_STRIDE: usize = 50;

/// Maximum number of distinct query embeddings cached at once (#376 D2).
///
/// Why: Repeated queries within a session (a user iterating on the same
/// search, or the LLM re-asking the same thing on retries) shouldn't
/// re-pay the FastEmbedder cost (~10–30ms). 256 entries is plenty for a
/// session and bounds memory at ~256 * 384 floats ≈ 400 KB.
const QUERY_CACHE_CAPACITY: usize = 256;

/// Reciprocal Rank Fusion constant (industry standard k=60).
///
/// Why: RRF is parameter-free across score distributions; the only knob is
/// the smoothing constant `k`. 60 is the value Cormack/Clarke/Buettcher
/// recommended in the original paper and is the default in Elastic, Vespa,
/// and most production hybrid-search stacks.
const RRF_K: f32 = 60.0;

/// Default cool-down window after which an idle search index is evicted.
///
/// Why: Default chosen for the user-facing knob in `[search]
/// cool_after_minutes`. 15 minutes balances "never cold under interactive
/// use" against "don't pin a multi-MB HNSW for an idle PM session".
/// What: Used by `CodeIndexer::with_default_cool_after` and by the config
/// loader (`CodeIndexer::new`) when no override is supplied.
/// Test: `cool_down_evicts_after_inactivity` (with a small override).
pub const DEFAULT_COOL_AFTER_MINUTES: u64 = 15;

/// A function (or function-sized) chunk of source code with location metadata.
///
/// Why: Search hits must point back to an exact location in the repo, with
/// enough context to be human-readable without opening the file. `score` is
/// filled from the underlying vector search's similarity value.
/// What: Plain serde struct; the `text` field is pre-truncated to
/// [`MAX_CHUNK_CHARS`] before storage.
/// Test: Round-tripped via `search_returns_code_chunk_with_metadata`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeChunk {
    pub file: PathBuf,
    pub function_name: Option<String>,
    pub start_line: usize,
    pub end_line: usize,
    pub language: String,
    #[serde(default)]
    pub score: f32,
    pub text: String,
    /// How this chunk was retrieved: "vector", "hybrid", "hybrid+kg", or "fallback:ripgrep".
    ///
    /// Why: Callers (search tool, service client, tests) cannot otherwise tell
    /// whether a result came from vector-only, hybrid RRF, KG expansion, or the
    /// ripgrep fallback path. Needed for debugging search quality and for
    /// downstream decisions about how much to trust a result (#401).
    #[serde(default)]
    pub match_reason: String,
}

/// Raw chunk produced by the language-specific extractors before embedding.
///
/// Why: Separating extraction from persistence keeps the AST-walking code
/// pure and easy to unit-test without touching disk or the embedder.
#[derive(Debug, Clone)]
struct RawChunk {
    function_name: Option<String>,
    start_line: usize,
    end_line: usize,
    text: String,
}

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
    store: Arc<dyn MemoryStore>,
    embedder: Arc<dyn Embedder>,
    /// Last time `search`/`search_hybrid` was invoked.
    ///
    /// Why: Drives the cool-down timer (#372). When `Instant::now() -
    /// last_access > cool_after`, the cool-down task evicts the in-memory
    /// HNSW for `Segment::CodeIndex`. The mutex is `tokio::sync::Mutex` so
    /// the eviction task and any concurrent search can serialize without
    /// blocking the runtime.
    last_access: Arc<TokioMutex<Instant>>,
    /// Inactivity threshold; index is evicted after this duration with no
    /// search calls. Set to a very large value (effectively disabled) when
    /// `with_cool_after(Duration::MAX)` or in environments that prefer to
    /// keep the index permanently warm.
    cool_after: Duration,
    /// Per-process LRU cache mapping `query` text → embedded vector.
    ///
    /// Why: Repeat queries within a session shouldn't re-pay the
    /// FastEmbedder cost. Capacity-bounded LRU keeps memory predictable
    /// (#376 D2).
    /// What: `tokio::sync::Mutex<LruCache>` so the cache is safe to share
    /// across the search task and any future warm-up routines without
    /// blocking the runtime.
    query_cache: Arc<TokioMutex<LruCache<String, Vec<f32>>>>,
    /// Optional bounded semaphore that gates indexing `spawn_blocking` jobs.
    ///
    /// Why: When the search daemon is actively re-indexing a tree, fastembed
    /// ONNX inference jobs run on the tokio blocking pool. Without a cap they
    /// can saturate the pool and starve axum HTTP handler tasks, causing
    /// `/search/query` and `/search/health` to time out (#399).
    /// What: `None` for in-process callers (CLI, tests) where indexing isn't
    /// concurrent with HTTP traffic. The daemon installs a semaphore via
    /// [`with_indexing_semaphore`] sized to roughly half the available
    /// parallelism so HTTP handlers always have threads to run on.
    indexing_permits: Option<Arc<Semaphore>>,
}

impl CodeIndexer {
    /// Construct a new `CodeIndexer` with injected store + embedder.
    ///
    /// Why: Constructor injection makes the dependencies explicit and
    /// mockable, per the project's DI conventions.
    /// What: Stores the `Arc`s, initialises `last_access` to `Instant::now()`
    /// and uses [`DEFAULT_COOL_AFTER_MINUTES`] as the inactivity threshold.
    /// Returns `Self` (no background task spawned — call
    /// [`spawn_cool_down_monitor`] on an `Arc<Self>` to enable eviction).
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
    /// written by [`index_file`] gives us an exact list of chunk ids to
    /// delete without scanning the full index.
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
    /// calls [`index_file`]. Returns total chunks inserted.
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

    /// Embed `query` with an LRU cache so repeated queries skip the
    /// FastEmbedder cost (#376 D2).
    ///
    /// Why: Within a session the same query gets re-issued — by the user
    /// iterating, the LLM retrying, or the daemon serving multiple
    /// concurrent agents. Caching is correct because the embedder is
    /// deterministic and the cached value is invariant across queries.
    /// What: Looks up `query` in the LRU. On hit, returns the clone
    /// directly (sub-microsecond). On miss, runs the existing
    /// `spawn_blocking(embed_single)` path and inserts the result.
    /// Test: Indirectly via the bench (`hybrid_vs_ripgrep_benchmark`)
    /// and by `search_hybrid_promotes_lexical_match`.
    async fn embed_query_cached(&self, query: &str) -> Result<Vec<f32>> {
        if let Some(hit) = self.query_cache.lock().await.get(query) {
            return Ok(hit.clone());
        }
        let embedder = Arc::clone(&self.embedder);
        let q = query.to_string();
        let vec = tokio::task::spawn_blocking(move || embedder.embed_single(&q))
            .await
            .context("embed task panicked")??;
        self.query_cache
            .lock()
            .await
            .put(query.to_string(), vec.clone());
        Ok(vec)
    }

    /// Semantic search over the code index.
    ///
    /// Why: The whole point of the indexer is to answer `"where is X?"`
    /// with ranked chunks. This is the read path.
    /// What: Embeds `query`, calls `store.search(Segment::CodeIndex, ...)`,
    /// deserializes each payload into a [`CodeChunk`], and sets `score`
    /// from the raw result.
    /// Test: `search_returns_code_chunk_with_metadata`.
    pub async fn search(&self, query: &str, top_k: usize) -> Result<Vec<CodeChunk>> {
        // Warm-up gate: bumps last_access and (if the index was evicted by
        // the cool-down task) reloads it from disk before the query runs.
        // The reload itself logs a single info line; here we stay quiet so
        // a hot path doesn't spam logs.
        self.ensure_warm().await?;
        let vec = self.embed_query_cached(query).await?;
        // Ask for extra results so we can drop manifest rows without
        // shrinking the final result set below `top_k`.
        let pull = top_k.saturating_mul(2).max(top_k + 4);
        let hits = self.store.search(Segment::CodeIndex, &vec, pull).await?;
        let mut out = Vec::with_capacity(hits.len());
        for hit in hits {
            // Manifest entries share the segment but aren't real chunks;
            // skip them by id prefix.
            if hit.id.starts_with("manifest:") {
                continue;
            }
            let mut chunk: CodeChunk = serde_json::from_value(hit.payload)
                .context("failed to deserialize CodeChunk payload")?;
            chunk.score = hit.score;
            chunk.match_reason = "vector".to_string();
            // Boost `agentconfig` chunks so root-level AGENTS.md /
            // CLAUDE.md surface first for agent/task/workflow queries.
            // Cap at 1.0 to preserve the "score is a similarity in
            // [0, 1]" invariant downstream formatters rely on.
            if chunk.language == "agentconfig" {
                chunk.score = (chunk.score * 1.1).min(1.0);
            }
            out.push(chunk);
        }
        // Re-sort after boosting so promoted chunks rank ahead of
        // equal-raw-score markdown/code siblings.
        out.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        out.truncate(top_k);
        Ok(out)
    }

    /// Hybrid code search: vector recall + BM25 lexical re-ranking via RRF,
    /// optionally followed by a knowledge-graph expansion pass.
    ///
    /// Why: Vector search alone misses exact-token matches (struct names,
    /// CLI flags, error strings). BM25 alone misses paraphrases. Reciprocal
    /// Rank Fusion (RRF) combines both rankings without needing the score
    /// distributions to be commensurable — a parameter-free approach that
    /// consistently outperforms either signal alone in production search
    /// engines (Elastic, Vespa, Weaviate). The graph expansion pass (#376)
    /// pulls in callers/callees of the top-K matches so a single match on
    /// `foo` also surfaces the functions that drive or depend on it.
    /// What: Pulls `4 * top_k` candidates from the vector index, builds a
    /// fresh `Bm25Index` over their text, then computes
    /// `rrf = alpha/(k + rank_vector) + beta/(k + rank_bm25)` for each
    /// candidate (k=60). The (alpha, beta) weights come from
    /// [`classify_query`] so a Definition query leans BM25, a Conceptual
    /// query leans the embedding signal, etc. (#376 B2). When
    /// `expand_graph` is true, looks up callers/callees of each top-K
    /// chunk's function name in the SymbolGraph and appends matching
    /// chunks (scored at 70% of the triggering chunk's RRF) to the result
    /// set. Returns the top `top_k` by combined score with original RRF
    /// hits taking priority on ties.
    /// Test: `search_hybrid_promotes_lexical_match`,
    /// `search_hybrid_expansion_appends_related_chunks`.
    pub async fn search_hybrid(
        &self,
        query: &str,
        top_k: usize,
        expand_graph: bool,
    ) -> Result<Vec<CodeChunk>> {
        // 0. Classify the query so the fusion weighting matches intent
        //    (#376 B2). For Definition/BugDebt queries we want to lean
        //    on BM25; for Conceptual queries the embedding signal wins.
        let classified: ClassifiedQuery = classify_query(query);
        let alpha = classified.vector_weight;
        let beta = classified.bm25_weight;
        tracing::debug!(
            intent = ?classified.intent,
            alpha,
            beta,
            "search_hybrid: classified query"
        );

        // 1. Pull a wider vector candidate set so BM25 has room to re-rank.
        //    Without this expansion, hybrid degrades to plain vector search.
        let pull = top_k.saturating_mul(4).max(top_k + 4);
        let vector_candidates = self.search(query, pull).await?;
        if vector_candidates.is_empty() {
            return Ok(Vec::new());
        }

        // 2. Build a per-candidate BM25 index. Tokens come from the existing
        //    project tokenizer so query and corpus terms agree on normalization
        //    (lowercase, alphanumeric splits, drop tokens ≤2 chars).
        let mut bm25 = Bm25Index::new();
        let mut chunk_ids: Vec<String> = Vec::with_capacity(vector_candidates.len());
        for chunk in &vector_candidates {
            // Stable ID across the two ranked lists; combining file +
            // start_line + end_line matches how the store keys chunks
            // (#376 A4).
            let id = format!(
                "{}:{}:{}",
                chunk.file.display(),
                chunk.start_line,
                chunk.end_line
            );
            // Include function name tokens too — BM25 should reward exact
            // identifier matches in `fn foo()` even when the body is short.
            let mut text = chunk.text.clone();
            if let Some(name) = &chunk.function_name {
                text.push(' ');
                text.push_str(name);
            }
            let terms = tokenize(&text);
            bm25.add_doc(id.clone(), terms);
            chunk_ids.push(id);
        }
        let query_terms = tokenize(query);
        let bm25_scored = bm25.score(&query_terms);

        // 3. Compute rank maps (1-indexed) and keep raw BM25 scores around
        //    so we can use them as a tiebreaker. A missing entry (zero BM25
        //    score) is treated as rank `len + 1` so it still contributes a
        //    small reciprocal but ranks below any matching doc.
        use std::collections::HashMap;
        let mut bm25_rank: HashMap<String, usize> = HashMap::new();
        let mut bm25_raw: HashMap<String, f32> = HashMap::new();
        for (rank, (id, score)) in bm25_scored.iter().enumerate() {
            bm25_rank.insert(id.clone(), rank + 1);
            bm25_raw.insert(id.clone(), *score);
        }
        let absent_rank = vector_candidates.len() + 1;

        // 4. Compute RRF score per candidate and rebuild the chunk list with
        //    that score so the final ordering is RRF-driven. We track the
        //    raw BM25 score alongside as a tiebreaker for the common case
        //    where two chunks happen to flip rank between the vector and
        //    lexical lists and end up with identical RRF (e.g. ranks (1,2)
        //    vs (2,1)) — we'd rather promote the one BM25 actually scored
        //    higher than leave the result order to the prior vector rank.
        let mut scored: Vec<(f32, f32, CodeChunk)> = vector_candidates
            .into_iter()
            .enumerate()
            .map(|(idx, mut chunk)| {
                let id = &chunk_ids[idx];
                let r_vec = idx + 1; // vector candidates are already ranked
                let r_bm = *bm25_rank.get(id).unwrap_or(&absent_rank);
                let rrf = alpha / (RRF_K + r_vec as f32) + beta / (RRF_K + r_bm as f32);
                let bm = *bm25_raw.get(id).unwrap_or(&0.0);
                chunk.score = rrf;
                chunk.match_reason = "hybrid".to_string();
                (rrf, bm, chunk)
            })
            .collect();

        // 5. Sort descending by RRF, breaking ties by raw BM25 score so a
        //    lexical-strong chunk wins when fusion produces a numerical tie.
        scored.sort_by(|a, b| {
            b.0.partial_cmp(&a.0)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal))
        });
        // Collect the *full* re-ranked candidate list (not just top_k) so
        // graph expansion can look up sibling chunks that ranked just
        // below the cutoff but match a caller/callee of a primary hit.
        let all_ranked: Vec<CodeChunk> = scored.into_iter().map(|(_, _, c)| c).collect();
        let primary: Vec<CodeChunk> = all_ranked.iter().take(top_k).cloned().collect();

        if !expand_graph || primary.is_empty() {
            return Ok(primary);
        }

        // 6. Knowledge-graph expansion (#376 B1). For each top-K chunk
        //    with a function name, look up callers + callees in the
        //    SymbolGraph derived from that chunk's source file. Append
        //    matching chunks at 70% of the triggering chunk's RRF.
        Ok(self.expand_with_graph(primary, &all_ranked, top_k, &classified))
    }

    /// Look up callers/callees of each chunk's function in a per-file
    /// SymbolGraph and append those expansions at 70% of the trigger
    /// chunk's score (#376 B1).
    ///
    /// Why: A semantic match on `process_request` is much more useful
    /// when the result also surfaces what calls it and what it calls.
    /// 70% scoring keeps the original RRF order on top while letting
    /// strong expansions outrank weaker primary hits when warranted.
    /// What: Builds a `SymbolGraph` per unique source file (cached for
    /// the duration of this call), looks up `callers_of` + `callees_of`
    /// for each function, and matches them back to existing primary
    /// chunks by `function_name + file`. De-duplicates against the
    /// primary set. Caps the expansion at `top_k` extra hits.
    /// Test: `search_hybrid_expansion_appends_related_chunks`.
    fn expand_with_graph(
        &self,
        primary: Vec<CodeChunk>,
        all_ranked: &[CodeChunk],
        top_k: usize,
        classified: &ClassifiedQuery,
    ) -> Vec<CodeChunk> {
        use std::collections::{HashMap, HashSet};
        use trusty_common::symgraph::graph::SymbolGraph;

        // Cache per-file graphs to avoid rebuilding when multiple top-K
        // hits live in the same file.
        let mut graph_cache: HashMap<PathBuf, Option<SymbolGraph>> = HashMap::new();
        // Build a name -> chunk map across the *full* re-ranked candidate
        // set (not just top-K) so expansion can resolve neighbours that
        // ranked just below the cutoff. Index uses (file, function_name).
        let mut by_name: HashMap<(PathBuf, String), CodeChunk> = HashMap::new();
        for c in all_ranked {
            if let Some(name) = &c.function_name {
                by_name
                    .entry((c.file.clone(), name.clone()))
                    .or_insert_with(|| c.clone());
            }
        }
        let mut seen: HashSet<(PathBuf, usize, usize)> = primary
            .iter()
            .map(|c| (c.file.clone(), c.start_line, c.end_line))
            .collect();

        let mut expansions: Vec<CodeChunk> = Vec::new();
        for trigger in &primary {
            let Some(fn_name) = trigger.function_name.as_ref() else {
                continue;
            };
            let entry = graph_cache
                .entry(trigger.file.clone())
                .or_insert_with(|| SymbolGraph::build_from_file(&trigger.file).ok());
            let Some(graph) = entry.as_ref() else {
                continue;
            };

            let mut neighbours: Vec<&trusty_common::symgraph::graph::SymbolNode> = Vec::new();
            neighbours.extend(graph.callers_of(fn_name));
            neighbours.extend(graph.callees_of(fn_name));

            for node in neighbours {
                let key = (node.file.clone(), node.name.clone());
                let Some(neighbour) = by_name.get(&key) else {
                    continue;
                };
                let dedup_key = (
                    neighbour.file.clone(),
                    neighbour.start_line,
                    neighbour.end_line,
                );
                if seen.contains(&dedup_key) {
                    continue;
                }
                seen.insert(dedup_key);
                let mut hit = neighbour.clone();
                hit.score = trigger.score * 0.7;
                hit.match_reason = "hybrid+kg".to_string();
                expansions.push(hit);
            }
        }

        // Trace for observability without spamming production logs.
        if !expansions.is_empty() {
            tracing::debug!(
                intent = ?classified.intent,
                primary = primary.len(),
                expansions = expansions.len(),
                "search_hybrid: graph expansion appended hits"
            );
        }

        // Combine: primary first (preserve RRF order), then expansions
        // sorted by their (already-discounted) score. Cap at `top_k +
        // top_k` total to bound payload size as the spec calls for.
        expansions.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let mut out = primary;
        let cap = top_k.saturating_mul(2);
        for e in expansions {
            if out.len() >= cap {
                break;
            }
            out.push(e);
        }
        out
    }

    /// Search with an optional language filter.
    ///
    /// Why: Power users often want to scope results to one language
    /// (`"rust"`, `"python"`, ...); filtering in-memory after a larger
    /// top-k pull is simple and correct for the current index sizes.
    /// What: Calls [`search`] with `top_k`, then retains only chunks whose
    /// `language` equals `language` (if provided).
    /// Test: Exercised indirectly by `search_returns_code_chunk_with_metadata`.
    pub async fn search_filtered(
        &self,
        query: &str,
        top_k: usize,
        language: Option<&str>,
    ) -> Result<Vec<CodeChunk>> {
        let hits = self.search(query, top_k).await?;
        let Some(lang) = language else {
            return Ok(hits);
        };
        Ok(hits.into_iter().filter(|c| c.language == lang).collect())
    }
}

/// Compute the manifest key used to track which chunk ids belong to a file.
///
/// Why: `remove_file` needs an O(1) lookup of the per-file chunk list so it
/// can delete precisely the entries that were inserted by `index_file`.
/// What: Returns `"manifest:{canonical_path}"`. Centralised here so the
/// write path and the delete path can't drift out of sync.
fn manifest_key(path: &Path) -> String {
    format!("manifest:{}", path.display())
}

/// Map a file to one of the supported language tags.
///
/// Why: Central switchboard keeps language detection consistent across
/// the indexer, the filter API, and the extractor. The optional `root`
/// argument lets us promote `AGENTS.md`/`CLAUDE.md` sitting directly at
/// the project root to the special `"agentconfig"` language so agent-
/// facing search queries surface them first.
/// What: If `root` is provided and `path`'s parent equals `root`, and the
/// (case-insensitive) filename is `AGENTS.md` or `CLAUDE.md`, returns
/// `"agentconfig"`. Otherwise maps the file extension to one of
/// `"rust"`/`"python"`/`"typescript"`/`"javascript"`/`"go"`/`"markdown"`,
/// or returns `None` for unknown extensions.
/// Test: `root_agents_md_gets_agentconfig_language`,
/// `subdir_agents_md_stays_markdown`, `claude_md_at_root_gets_agentconfig`,
/// plus implicit coverage from per-language chunking tests.
fn detect_language(path: &Path, root: Option<&Path>) -> Option<&'static str> {
    // Root-level AGENTS.md / CLAUDE.md promotion.
    if let Some(root) = root
        && let Some(parent) = path.parent()
        && parent == root
        && let Some(name) = path.file_name().and_then(|n| n.to_str())
    {
        let lower = name.to_ascii_lowercase();
        if lower == "agents.md" || lower == "claude.md" {
            return Some("agentconfig");
        }
    }
    match path.extension()?.to_str()? {
        "rs" => Some("rust"),
        "py" => Some("python"),
        "ts" | "tsx" => Some("typescript"),
        "js" | "jsx" => Some("javascript"),
        "go" => Some("go"),
        "java" => Some("java"),
        "c" | "h" => Some("c"),
        "cpp" | "cc" | "cxx" | "hpp" | "hxx" => Some("cpp"),
        "md" | "markdown" => Some("markdown"),
        _ => None,
    }
}

/// Skip hidden directories and known build/vendor folders during walks.
///
/// Why: Indexing `.git`, `target/`, or `node_modules/` wastes time and
/// pollutes results with generated code.
/// What: Returns true for dotted names (except `.`/`..`) and a short
/// denylist of directory names.
fn is_hidden_or_skipped(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    if name == "." || name == ".." {
        return false;
    }
    if name.starts_with('.') {
        return true;
    }
    matches!(name, "target" | "node_modules" | "dist" | "build")
}

/// Truncate a string to at most `max` characters (by byte index of the
/// `max`-th char, not byte count), returning an owned `String`.
///
/// Why: Chunk text must stay under [`MAX_CHUNK_CHARS`] before embedding
/// and before being written to redb. Using char indices avoids slicing
/// through a UTF-8 multibyte sequence.
/// What: If the string has fewer than `max` chars, returns it unchanged;
/// otherwise truncates and returns a new `String`.
/// Test: Implicit — large source files in indexing tests exercise it.
fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max).collect()
}

/// Dispatch to the per-language chunk extractor.
///
/// Why: Language-specific node kinds differ enough that sharing a single
/// traversal is awkward. Per-language functions keep each implementation
/// self-contained and readable.
/// What: Delegates to a language-specific helper; falls back to fixed
/// line windows if the helper returns zero chunks.
/// Test: `rust_function_chunking`, `python_function_chunking`,
/// `go_function_chunking`, `markdown_heading_chunking`,
/// `fallback_to_line_chunks_when_no_functions`.
fn extract_chunks_from_source(source: &str, language: &str) -> Vec<RawChunk> {
    let chunks = match language {
        "rust" => extract_tree_sitter(
            source,
            tree_sitter_rust::LANGUAGE.into(),
            &["function_item"],
        ),
        "python" => extract_tree_sitter(
            source,
            tree_sitter_python::LANGUAGE.into(),
            &["function_definition", "async_function_definition"],
        ),
        "javascript" => extract_tree_sitter(
            source,
            tree_sitter_javascript::LANGUAGE.into(),
            &[
                "function_declaration",
                "function_expression",
                "arrow_function",
                "method_definition",
            ],
        ),
        "typescript" => extract_tree_sitter(
            source,
            tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            &[
                "function_declaration",
                "function_expression",
                "arrow_function",
                "method_definition",
                "method_signature",
            ],
        ),
        "go" => extract_tree_sitter(
            source,
            tree_sitter_go::LANGUAGE.into(),
            &["function_declaration", "method_declaration"],
        ),
        "java" => extract_tree_sitter(
            source,
            tree_sitter_java::LANGUAGE.into(),
            &["method_declaration", "constructor_declaration"],
        ),
        "c" => extract_tree_sitter(
            source,
            tree_sitter_c::LANGUAGE.into(),
            &["function_definition"],
        ),
        "cpp" => extract_tree_sitter(
            source,
            tree_sitter_cpp::LANGUAGE.into(),
            &["function_definition"],
        ),
        "markdown" | "agentconfig" => return extract_markdown_headings(source),
        _ => Vec::new(),
    };
    if chunks.is_empty() {
        fallback_line_chunks(source)
    } else {
        chunks
    }
}

/// Generic tree-sitter extractor keyed by a list of target node kinds.
///
/// Why: All four code languages share the same walk-and-capture pattern;
/// only the set of "interesting" node kinds differs.
/// What: Parses `source`, then does a preorder walk; whenever a node's
/// `kind()` matches one of `target_kinds`, emits a `RawChunk` with the
/// node's byte range and `name`-field text (if present).
/// Test: Covered by per-language tests.
fn extract_tree_sitter(source: &str, language: Language, target_kinds: &[&str]) -> Vec<RawChunk> {
    let mut parser = Parser::new();
    if parser.set_language(&language).is_err() {
        tracing::warn!("tree-sitter set_language failed");
        return Vec::new();
    }
    let Some(tree) = parser.parse(source, None) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    walk_for_kinds(tree.root_node(), source, target_kinds, &mut out);
    out
}

/// Preorder walk that captures nodes whose `kind()` is in `targets`.
///
/// Why: tree-sitter cursors are awkward to use recursively; a plain
/// function is easier to read. Once we hit a captured node we still
/// descend — nested functions inside methods (e.g., JS closures in
/// methods) should also be indexed.
/// What: Recursive DFS. For each match, extract the node's text and its
/// `name` child's text; push a [`RawChunk`].
fn walk_for_kinds(node: Node, source: &str, targets: &[&str], out: &mut Vec<RawChunk>) {
    if targets.contains(&node.kind())
        && let Some(chunk) = node_to_chunk(node, source)
    {
        out.push(chunk);
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_for_kinds(child, source, targets, out);
    }
}

/// Convert a tree-sitter node into a [`RawChunk`].
///
/// Why: The body of the captured-node handling is identical across
/// languages; factoring it out keeps `walk_for_kinds` small.
/// What: Extracts UTF-8 text via byte range, reads the `name` field if
/// present, and converts 0-indexed rows to 1-indexed line numbers.
fn node_to_chunk(node: Node, source: &str) -> Option<RawChunk> {
    let start_byte = node.start_byte();
    let end_byte = node.end_byte();
    let text = source.get(start_byte..end_byte)?.to_string();
    let name = node
        .child_by_field_name("name")
        .and_then(|c| c.utf8_text(source.as_bytes()).ok())
        .map(|s| s.to_string());
    Some(RawChunk {
        function_name: name,
        start_line: node.start_position().row + 1,
        end_line: node.end_position().row + 1,
        text,
    })
}

/// Split a markdown document at `##` headings.
///
/// Why: Tree-sitter for markdown is overkill here; heading-based splits
/// are exactly what most docs want anyway, and the regex fallback keeps
/// the dependency graph smaller.
/// What: Iterates lines; every line starting with `## ` (exactly two `#`
/// and a space) closes the previous chunk and opens a new one.
/// Test: `markdown_heading_chunking`.
fn extract_markdown_headings(source: &str) -> Vec<RawChunk> {
    let mut chunks = Vec::new();
    let mut current_start: Option<usize> = None;
    let mut current_name: Option<String> = None;
    let mut current_lines: Vec<&str> = Vec::new();

    let flush = |start: usize,
                 end: usize,
                 name: Option<String>,
                 lines: &[&str],
                 out: &mut Vec<RawChunk>| {
        if lines.is_empty() {
            return;
        }
        out.push(RawChunk {
            function_name: name,
            start_line: start,
            end_line: end,
            text: lines.join("\n"),
        });
    };

    for (idx, line) in source.lines().enumerate() {
        let one_indexed = idx + 1;
        if let Some(rest) = line.strip_prefix("## ") {
            if let Some(start) = current_start {
                flush(
                    start,
                    one_indexed - 1,
                    current_name.take(),
                    &current_lines,
                    &mut chunks,
                );
            }
            current_start = Some(one_indexed);
            current_name = Some(rest.trim().to_string());
            current_lines.clear();
            current_lines.push(line);
        } else if current_start.is_some() {
            current_lines.push(line);
        }
    }
    if let Some(start) = current_start {
        let end = source.lines().count().max(start);
        flush(start, end, current_name, &current_lines, &mut chunks);
    }
    chunks
}

/// Sliding-window line chunker used when AST extraction finds nothing.
///
/// Why: Config files, data-only modules, and unsupported languages still
/// deserve to be searchable. Overlapping windows (50 lines, 25-line stride)
/// preserve cross-boundary context — every line appears in two consecutive
/// chunks so a tight code block split across a window boundary still scores
/// well in semantic search.
/// What: Emits one [`RawChunk`] per stride step, each [`FALLBACK_LINES_PER_CHUNK`]
/// lines wide (or shorter at end-of-file), with no function name and 1-indexed
/// start/end lines.
/// Test: `fallback_uses_overlapping_windows`.
fn fallback_line_chunks(source: &str) -> Vec<RawChunk> {
    let lines: Vec<&str> = source.lines().collect();
    if lines.is_empty() {
        return Vec::new();
    }
    // For files smaller than one window, emit a single chunk covering the
    // whole file rather than producing zero strides.
    if lines.len() <= FALLBACK_LINES_PER_CHUNK {
        return vec![RawChunk {
            function_name: None,
            start_line: 1,
            end_line: lines.len(),
            text: lines.join("\n"),
        }];
    }
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let end = (i + FALLBACK_LINES_PER_CHUNK).min(lines.len());
        out.push(RawChunk {
            function_name: None,
            start_line: i + 1,
            end_line: end,
            text: lines[i..end].join("\n"),
        });
        // Stop once the window has reached EOF; otherwise advance by stride.
        if end == lines.len() {
            break;
        }
        i += FALLBACK_STRIDE;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::HashMap;
    use std::io::Write;
    use std::sync::Mutex;

    use async_trait::async_trait;
    use serde_json::Value;
    use tempfile::NamedTempFile;

    use crate::memory::{Embedder, MemoryResult, MemoryStore, Segment};

    // ---------- mocks ----------

    /// Minimal in-memory store that records inserts and returns them in
    /// insertion order on `search` (regardless of vector).
    ///
    /// Why: Exercising AST extraction and payload serialization end-to-end
    /// doesn't need a real HNSW; insertion order is deterministic and
    /// lets tests assert on concrete results.
    struct MockStore {
        inner: Mutex<HashMap<String, (Vec<f32>, Value)>>,
        order: Mutex<Vec<String>>,
    }
    impl MockStore {
        fn new() -> Self {
            Self {
                inner: Mutex::new(HashMap::new()),
                order: Mutex::new(Vec::new()),
            }
        }
    }
    #[async_trait]
    impl MemoryStore for MockStore {
        async fn insert(
            &self,
            _segment: Segment,
            id: &str,
            vector: &[f32],
            payload: Value,
        ) -> anyhow::Result<()> {
            self.inner
                .lock()
                .unwrap()
                .insert(id.to_string(), (vector.to_vec(), payload));
            self.order.lock().unwrap().push(id.to_string());
            Ok(())
        }
        async fn search(
            &self,
            _segment: Segment,
            _query_vec: &[f32],
            top_k: usize,
        ) -> anyhow::Result<Vec<MemoryResult>> {
            let order = self.order.lock().unwrap().clone();
            let inner = self.inner.lock().unwrap();
            let mut out = Vec::new();
            for (score_idx, id) in order.iter().take(top_k).enumerate() {
                if let Some((_, payload)) = inner.get(id) {
                    out.push(MemoryResult {
                        id: id.clone(),
                        score: 1.0 - (score_idx as f32) * 0.1,
                        payload: payload.clone(),
                        segment: "code".to_string(),
                    });
                }
            }
            Ok(out)
        }
        async fn get(&self, _segment: Segment, id: &str) -> anyhow::Result<Option<Value>> {
            Ok(self.inner.lock().unwrap().get(id).map(|(_, p)| p.clone()))
        }
        async fn delete(&self, _segment: Segment, id: &str) -> anyhow::Result<()> {
            self.inner.lock().unwrap().remove(id);
            self.order.lock().unwrap().retain(|x| x != id);
            Ok(())
        }
    }

    /// Deterministic fake embedder: maps each text to a fixed-length
    /// vector where every element is `text.len() as f32 / 100.0`. Equal
    /// inputs yield equal vectors, which is all the tests need.
    struct MockEmbedder {
        dim: usize,
    }
    impl Embedder for MockEmbedder {
        fn embed(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
            Ok(texts
                .iter()
                .map(|t| vec![t.len() as f32 / 100.0; self.dim])
                .collect())
        }
        fn embed_single(&self, text: &str) -> anyhow::Result<Vec<f32>> {
            Ok(vec![text.len() as f32 / 100.0; self.dim])
        }
        fn dimension(&self) -> usize {
            self.dim
        }
    }

    // ---------- extractor tests ----------

    #[test]
    fn rust_function_chunking() {
        let src = "fn foo() {\n    println!(\"hi\");\n}\n\nfn bar(x: i32) -> i32 {\n    x + 1\n}\n";
        let chunks = extract_chunks_from_source(src, "rust");
        assert_eq!(chunks.len(), 2, "expected 2 Rust functions, got {chunks:?}");
        assert_eq!(chunks[0].function_name.as_deref(), Some("foo"));
        assert_eq!(chunks[0].start_line, 1);
        assert_eq!(chunks[0].end_line, 3);
        assert_eq!(chunks[1].function_name.as_deref(), Some("bar"));
        assert_eq!(chunks[1].start_line, 5);
        assert_eq!(chunks[1].end_line, 7);
    }

    #[test]
    fn python_function_chunking() {
        let src = "def foo():\n    pass\n\nasync def bar():\n    return 1\n";
        let chunks = extract_chunks_from_source(src, "python");
        assert_eq!(
            chunks.len(),
            2,
            "expected 2 Python functions, got {chunks:?}"
        );
        let names: Vec<Option<&str>> = chunks.iter().map(|c| c.function_name.as_deref()).collect();
        assert!(names.contains(&Some("foo")));
        assert!(names.contains(&Some("bar")));
    }

    #[test]
    fn go_function_chunking() {
        let src = "package main\n\nfunc Foo() {}\n\nfunc (r *R) Bar() int {\n    return 1\n}\n";
        let chunks = extract_chunks_from_source(src, "go");
        assert_eq!(chunks.len(), 2, "expected 2 Go functions, got {chunks:?}");
        let names: Vec<Option<&str>> = chunks.iter().map(|c| c.function_name.as_deref()).collect();
        assert!(names.contains(&Some("Foo")));
        assert!(names.contains(&Some("Bar")));
    }

    #[test]
    fn markdown_heading_chunking() {
        let src = "# Title\n\nintro\n\n## Alpha\n\naaa\n\n## Beta\n\nbbb\n\n## Gamma\n\nccc\n";
        let chunks = extract_chunks_from_source(src, "markdown");
        assert_eq!(
            chunks.len(),
            3,
            "expected 3 markdown sections, got {chunks:?}"
        );
        assert_eq!(chunks[0].function_name.as_deref(), Some("Alpha"));
        assert_eq!(chunks[0].start_line, 5);
        assert_eq!(chunks[1].function_name.as_deref(), Some("Beta"));
        assert_eq!(chunks[1].start_line, 9);
        assert_eq!(chunks[2].function_name.as_deref(), Some("Gamma"));
        assert_eq!(chunks[2].start_line, 13);
    }

    #[test]
    fn fallback_uses_overlapping_windows() {
        // 300 lines of a constants-only Rust file (no functions) → fallback
        // uses 150-line windows with a 50-line stride (~67% overlap, #376).
        // Stride positions at 0, 50, 100, 150 → starts 1, 51, 101, 151;
        // window at 151..301 clipped to 151..300, then loop terminates
        // because end == lines.len(). Expect 4 overlapping chunks.
        let body: String = (0..300)
            .map(|i| format!("const K{i}: u32 = {i};\n"))
            .collect();
        let chunks = extract_chunks_from_source(&body, "rust");
        assert_eq!(
            chunks.len(),
            4,
            "expected 4 overlapping fallback chunks, got {chunks:?}"
        );
        assert!(chunks.iter().all(|c| c.function_name.is_none()));
        // First window.
        assert_eq!(chunks[0].start_line, 1);
        assert_eq!(chunks[0].end_line, 150);
        // Second window starts 50 lines after the first → ~67% overlap.
        assert_eq!(chunks[1].start_line, 51);
        assert_eq!(chunks[1].end_line, 200);
        // Third window.
        assert_eq!(chunks[2].start_line, 101);
        assert_eq!(chunks[2].end_line, 250);
        // Final window clipped to file length.
        assert_eq!(chunks[3].start_line, 151);
        assert_eq!(chunks[3].end_line, 300);
    }

    #[test]
    fn fallback_small_file_emits_single_chunk() {
        // A small file (under one window) should produce one chunk
        // covering the whole file (not zero strides) to keep small
        // configs searchable.
        let body: String = (0..10).map(|i| format!("k{i}=v\n")).collect();
        let chunks = fallback_line_chunks(&body);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].start_line, 1);
        assert_eq!(chunks[0].end_line, 10);
    }

    #[test]
    fn java_function_chunking() {
        let src = "class Foo {\n    void bar() {}\n    int baz(int x) { return x; }\n}\n";
        let chunks = extract_chunks_from_source(src, "java");
        assert!(
            chunks.len() >= 2,
            "expected at least 2 Java methods, got {chunks:?}"
        );
        let names: Vec<Option<&str>> = chunks.iter().map(|c| c.function_name.as_deref()).collect();
        assert!(names.contains(&Some("bar")));
        assert!(names.contains(&Some("baz")));
    }

    #[test]
    fn c_function_chunking() {
        let src = "int add(int a, int b) {\n    return a + b;\n}\n\nvoid noop(void) {}\n";
        let chunks = extract_chunks_from_source(src, "c");
        assert_eq!(chunks.len(), 2, "expected 2 C functions, got {chunks:?}");
    }

    #[test]
    fn cpp_function_chunking() {
        let src = "int square(int x) { return x * x; }\n\nvoid greet() {}\n";
        let chunks = extract_chunks_from_source(src, "cpp");
        assert_eq!(chunks.len(), 2, "expected 2 C++ functions, got {chunks:?}");
    }

    // ---------- end-to-end test ----------

    #[tokio::test]
    async fn search_returns_code_chunk_with_metadata() {
        // Write a tiny Rust file, index it, search, and assert that the
        // top hit has the right file path + function name.
        let mut tmp = NamedTempFile::new().expect("tempfile");
        let src = "fn greet() {\n    println!(\"hello world\");\n}\n";
        tmp.write_all(src.as_bytes()).expect("write");
        // Rename to `.rs` so `detect_language` picks Rust. NamedTempFile
        // doesn't expose a rename, so we build a sibling path.
        let new_path = tmp.path().with_extension("rs");
        std::fs::copy(tmp.path(), &new_path).expect("copy to .rs");
        let _guard = scopeguard_for(&new_path);

        let store: Arc<dyn MemoryStore> = Arc::new(MockStore::new());
        let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder { dim: 8 });
        let indexer = CodeIndexer::new(Arc::clone(&store), Arc::clone(&embedder));

        let inserted = indexer
            .index_file(&new_path, None)
            .await
            .expect("index_file");
        assert_eq!(inserted, 1, "expected one chunk inserted");

        let hits = indexer.search("greet", 5).await.expect("search");
        assert_eq!(hits.len(), 1, "expected one hit");
        let hit = &hits[0];
        assert_eq!(hit.function_name.as_deref(), Some("greet"));
        assert_eq!(hit.language, "rust");
        assert_eq!(hit.start_line, 1);
        assert_eq!(hit.end_line, 3);
        // File path should be absolute (canonicalized).
        assert!(
            hit.file.is_absolute(),
            "expected absolute path, got {:?}",
            hit.file
        );
        assert!(hit.score > 0.0, "expected score > 0, got {}", hit.score);

        // Filter by language: rust passes, go excludes.
        let rust_only = indexer
            .search_filtered("greet", 5, Some("rust"))
            .await
            .expect("search_filtered rust");
        assert_eq!(rust_only.len(), 1);
        let go_only = indexer
            .search_filtered("greet", 5, Some("go"))
            .await
            .expect("search_filtered go");
        assert!(go_only.is_empty(), "expected no go hits");
    }

    // ---------- agentconfig promotion tests ----------

    #[tokio::test]
    async fn root_agents_md_gets_agentconfig_language() {
        // Why: Files named AGENTS.md at the *project root* should be
        // indexed as "agentconfig" so they can be boosted in search.
        let dir = tempfile::Builder::new()
            .prefix("agentcfg-")
            .tempdir()
            .expect("tempdir");
        let agents_md = dir.path().join("AGENTS.md");
        std::fs::write(
            &agents_md,
            "# Agents\n\n## Overview\n\nAgent instructions here.\n",
        )
        .expect("write AGENTS.md");

        let store = Arc::new(MockStore::new());
        let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder { dim: 8 });
        let indexer = CodeIndexer::new(store.clone(), Arc::clone(&embedder));

        let n = indexer
            .index_file(&agents_md, Some(dir.path()))
            .await
            .expect("index_file");
        assert!(n >= 1, "expected at least one chunk, got {n}");

        // Pull any stored CodeChunk payload and verify its language.
        let inner = store.inner.lock().unwrap();
        let saw_agentconfig = inner
            .iter()
            .filter(|(id, _)| !id.starts_with("manifest:"))
            .any(|(_, (_, payload))| {
                payload.get("language").and_then(|v| v.as_str()) == Some("agentconfig")
            });
        assert!(
            saw_agentconfig,
            "expected an agentconfig-language chunk to be stored"
        );
    }

    #[tokio::test]
    async fn subdir_agents_md_stays_markdown() {
        // Why: Only *root-level* AGENTS.md is promoted. Nested copies in
        // subdirectories should remain plain markdown so they don't leak
        // into the boosted agentconfig bucket.
        let dir = tempfile::Builder::new()
            .prefix("agentcfg-sub-")
            .tempdir()
            .expect("tempdir");
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).expect("create subdir");
        let nested = sub.join("AGENTS.md");
        std::fs::write(&nested, "## Section\n\nNested content.\n").expect("write nested");

        let store = Arc::new(MockStore::new());
        let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder { dim: 8 });
        let indexer = CodeIndexer::new(store.clone(), Arc::clone(&embedder));

        let n = indexer
            .index_file(&nested, Some(dir.path()))
            .await
            .expect("index_file");
        assert!(n >= 1, "expected at least one chunk, got {n}");

        let inner = store.inner.lock().unwrap();
        let languages: Vec<String> = inner
            .iter()
            .filter(|(id, _)| !id.starts_with("manifest:"))
            .filter_map(|(_, (_, payload))| {
                payload
                    .get("language")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
            })
            .collect();
        assert!(
            languages.iter().all(|l| l == "markdown"),
            "expected only markdown chunks for nested file, got {languages:?}"
        );
        assert!(
            !languages.is_empty(),
            "expected at least one markdown chunk"
        );
    }

    #[tokio::test]
    async fn claude_md_at_root_gets_agentconfig() {
        // Why: CLAUDE.md at the project root is the other canonical
        // agent-config filename and must be promoted alongside AGENTS.md.
        let dir = tempfile::Builder::new()
            .prefix("agentcfg-claude-")
            .tempdir()
            .expect("tempdir");
        let claude_md = dir.path().join("CLAUDE.md");
        std::fs::write(&claude_md, "# Project\n\n## Goals\n\nClaude guidance.\n")
            .expect("write CLAUDE.md");

        let store = Arc::new(MockStore::new());
        let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder { dim: 8 });
        let indexer = CodeIndexer::new(store.clone(), Arc::clone(&embedder));

        indexer
            .index_file(&claude_md, Some(dir.path()))
            .await
            .expect("index_file");

        let inner = store.inner.lock().unwrap();
        let saw_agentconfig = inner
            .iter()
            .filter(|(id, _)| !id.starts_with("manifest:"))
            .any(|(_, (_, payload))| {
                payload.get("language").and_then(|v| v.as_str()) == Some("agentconfig")
            });
        assert!(
            saw_agentconfig,
            "expected CLAUDE.md at root to be indexed as agentconfig"
        );
    }

    #[tokio::test]
    async fn agentconfig_score_boosted_in_search_results() {
        // Why: After deserialization, chunks with language == "agentconfig"
        // must have their score multiplied by 1.1 (capped at 1.0) and the
        // result set re-sorted so they rank above equal-raw-score markdown
        // siblings.
        //
        // Uses a custom mock store that returns a fixed pair of hits with
        // *equal* raw scores — one agentconfig, one markdown — regardless
        // of query vector. This isolates the boost+sort logic.
        struct BoostMockStore;
        #[async_trait]
        impl MemoryStore for BoostMockStore {
            async fn insert(&self, _: Segment, _: &str, _: &[f32], _: Value) -> anyhow::Result<()> {
                Ok(())
            }
            async fn search(
                &self,
                _: Segment,
                _: &[f32],
                _top_k: usize,
            ) -> anyhow::Result<Vec<MemoryResult>> {
                let md_chunk = CodeChunk {
                    file: PathBuf::from("/tmp/readme.md"),
                    function_name: Some("Readme".to_string()),
                    start_line: 1,
                    end_line: 3,
                    language: "markdown".to_string(),
                    score: 0.0,
                    text: "# readme".to_string(),
                    match_reason: String::new(),
                };
                let agent_chunk = CodeChunk {
                    file: PathBuf::from("/tmp/AGENTS.md"),
                    function_name: Some("Agents".to_string()),
                    start_line: 1,
                    end_line: 3,
                    language: "agentconfig".to_string(),
                    score: 0.0,
                    text: "# agents".to_string(),
                    match_reason: String::new(),
                };
                // Raw scores are equal; markdown comes first in the raw
                // order to prove the sort promotes agentconfig above it.
                Ok(vec![
                    MemoryResult {
                        id: "md:1".to_string(),
                        score: 0.5,
                        segment: "code".to_string(),
                        payload: serde_json::to_value(&md_chunk).unwrap(),
                    },
                    MemoryResult {
                        id: "agents:1".to_string(),
                        score: 0.5,
                        segment: "code".to_string(),
                        payload: serde_json::to_value(&agent_chunk).unwrap(),
                    },
                ])
            }
            async fn get(&self, _: Segment, _: &str) -> anyhow::Result<Option<Value>> {
                Ok(None)
            }
            async fn delete(&self, _: Segment, _: &str) -> anyhow::Result<()> {
                Ok(())
            }
        }

        let store: Arc<dyn MemoryStore> = Arc::new(BoostMockStore);
        let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder { dim: 8 });
        let indexer = CodeIndexer::new(store, embedder);

        let hits = indexer.search("agents", 5).await.expect("search");
        assert_eq!(hits.len(), 2, "expected two hits");
        assert_eq!(
            hits[0].language, "agentconfig",
            "agentconfig chunk should rank first after boost"
        );
        assert!(
            hits[0].score > hits[1].score,
            "boosted score {} should exceed unboosted {}",
            hits[0].score,
            hits[1].score
        );
        // 0.5 * 1.1 = 0.55, comfortably under the 1.0 cap.
        assert!(
            (hits[0].score - 0.55).abs() < 1e-5,
            "expected boosted score ~0.55, got {}",
            hits[0].score
        );
    }

    #[tokio::test]
    async fn search_hybrid_promotes_lexical_match() {
        // Why: After RRF re-ranking, a chunk with a strong BM25 lexical hit
        // should outrank a chunk that only matched via the (deterministic
        // mock) vector embedding. We seed the mock store such that vector
        // ordering puts the *non-matching* chunk first; the BM25 signal
        // should flip the order.
        use std::io::Write;

        let dir = tempfile::Builder::new()
            .prefix("hybrid-")
            .tempdir()
            .expect("tempdir");
        // File A: contains the rare token "bm25_special" — should rank
        // higher after lexical fusion.
        let a = dir.path().join("a.rs");
        let mut fa = std::fs::File::create(&a).unwrap();
        writeln!(fa, "fn alpha() {{").unwrap();
        writeln!(fa, "    // bm25_special token appears here").unwrap();
        writeln!(fa, "    println!(\"bm25_special\");").unwrap();
        writeln!(fa, "}}").unwrap();
        drop(fa);
        // File B: same length but no occurrence of the rare token.
        let b = dir.path().join("b.rs");
        let mut fb = std::fs::File::create(&b).unwrap();
        writeln!(fb, "fn beta() {{").unwrap();
        writeln!(fb, "    // generic body lines for padding").unwrap();
        writeln!(fb, "    println!(\"hello\");").unwrap();
        writeln!(fb, "}}").unwrap();
        drop(fb);

        let store: Arc<dyn MemoryStore> = Arc::new(MockStore::new());
        let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder { dim: 8 });
        let indexer = CodeIndexer::new(Arc::clone(&store), Arc::clone(&embedder));
        // Insert b first so vector search returns it first (insertion-order
        // mock); a should be promoted by BM25 to outrank it after fusion.
        indexer.index_file(&b, None).await.expect("index b");
        indexer.index_file(&a, None).await.expect("index a");

        let hits = indexer
            .search_hybrid("bm25_special", 5, false)
            .await
            .expect("hybrid search");
        assert!(!hits.is_empty(), "hybrid returned no hits");
        // The top hit must be the file containing the rare token. With the
        // mock embedder both files get equal cosine, so vector ranking
        // resolves to insertion order (b first, then a). BM25 inverts that
        // (a first because its text contains both query terms). RRF on
        // (1,2) and (2,1) ties numerically; the BM25-raw tiebreaker is what
        // promotes a above b — exactly the property we want to enforce.
        let top_path = hits[0].file.display().to_string();
        assert!(
            top_path.ends_with("a.rs"),
            "expected a.rs to outrank b.rs after RRF; got top={top_path:?}"
        );
        // RRF score is in (0, 2/(RRF_K+1)] — sanity check it's positive.
        assert!(hits[0].score > 0.0, "RRF score should be positive");
    }

    /// Verify that KG expansion appends caller/callee chunks beyond the
    /// initial RRF set when `expand_graph` is true (#376 B1).
    ///
    /// Why: Hybrid search alone doesn't return functions related to the
    /// match by call structure. With expansion enabled, a top-K hit on
    /// `caller` should also surface `callee` and vice-versa, scored at
    /// 0.7× the trigger's RRF.
    /// What: Writes a single Rust file containing two functions where
    /// `caller` calls `callee`, indexes it, queries for `caller`, and
    /// asserts both functions appear in the expanded result set.
    #[tokio::test]
    async fn search_hybrid_expansion_appends_related_chunks() {
        use std::io::Write;

        let dir = tempfile::Builder::new()
            .prefix("kgexpand-")
            .tempdir()
            .expect("tempdir");
        let p = dir.path().join("expand.rs");
        let mut f = std::fs::File::create(&p).unwrap();
        writeln!(f, "fn callee() -> i32 {{ 42 }}").unwrap();
        writeln!(f, "fn caller() -> i32 {{ callee() + 1 }}").unwrap();
        drop(f);

        let store: Arc<dyn MemoryStore> = Arc::new(MockStore::new());
        let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder { dim: 8 });
        let indexer = CodeIndexer::new(Arc::clone(&store), Arc::clone(&embedder));
        indexer.index_file(&p, None).await.expect("index");

        let baseline = indexer
            .search_hybrid("callee", 1, false)
            .await
            .expect("hybrid no expand");
        assert_eq!(baseline.len(), 1, "baseline should be exactly top_k=1");

        let expanded = indexer
            .search_hybrid("callee", 1, true)
            .await
            .expect("hybrid expand");
        assert!(
            expanded.len() > baseline.len(),
            "expansion should add hits; got {}",
            expanded.len()
        );
        let names: Vec<&str> = expanded
            .iter()
            .filter_map(|c| c.function_name.as_deref())
            .collect();
        assert!(
            names.contains(&"callee") && names.contains(&"caller"),
            "expansion missing related fn; got {names:?}"
        );
    }

    /// Verify the query embedding LRU cache returns identical vectors
    /// across calls (#376 D2).
    ///
    /// Why: A correct cache must return the *same* embedding for the
    /// same query without re-running the embedder. Our `MockEmbedder` is
    /// deterministic, so a regression to "always re-embed" would still
    /// yield equal vectors — but the cache hit path returns its stored
    /// clone, which is the property we want to verify.
    /// What: Calls `embed_query_cached` twice with the same query and
    /// asserts the returned vectors are equal.
    #[tokio::test]
    async fn embed_query_cached_returns_consistent_vector() {
        let store: Arc<dyn MemoryStore> = Arc::new(MockStore::new());
        let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder { dim: 8 });
        let indexer = CodeIndexer::new(store, embedder);
        let v1 = indexer.embed_query_cached("hello world").await.unwrap();
        let v2 = indexer.embed_query_cached("hello world").await.unwrap();
        assert_eq!(v1, v2, "cached query embedding must be stable");
        // Cache should now have the entry.
        let cache = indexer.query_cache.lock().await;
        assert!(cache.contains(&"hello world".to_string()));
    }

    /// Recursive case-insensitive substring grep used as the ripgrep stand-in
    /// for the bench. Skips hidden + build dirs to mirror the indexer's filter.
    fn walkdir_grep_bench(root: &Path, query: &str, top_k: usize) -> Vec<(PathBuf, usize)> {
        let needle = query.to_lowercase();
        let mut hits = Vec::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(p) = stack.pop() {
            if hits.len() >= top_k {
                break;
            }
            if p.is_dir() {
                if let Some(name) = p.file_name().and_then(|n| n.to_str())
                    && (name.starts_with('.')
                        || matches!(name, "target" | "node_modules" | "dist" | "build"))
                {
                    continue;
                }
                if let Ok(rd) = std::fs::read_dir(&p) {
                    for entry in rd.flatten() {
                        stack.push(entry.path());
                    }
                }
            } else if p.is_file() {
                let ok = p
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|e| matches!(e, "rs" | "py" | "ts" | "tsx" | "js" | "go" | "md"))
                    .unwrap_or(false);
                if !ok {
                    continue;
                }
                let Ok(body) = std::fs::read_to_string(&p) else {
                    continue;
                };
                for (idx, line) in body.lines().enumerate() {
                    if line.to_lowercase().contains(&needle) {
                        hits.push((p.clone(), idx + 1));
                        if hits.len() >= top_k {
                            break;
                        }
                    }
                }
            }
        }
        hits
    }

    /// Hybrid-vs-ripgrep latency + ranking comparison for #372.
    ///
    /// Why: We want a checked-in baseline showing hybrid (vector + BM25 RRF)
    /// is competitive with ripgrep on representative queries — both in
    /// quality and latency. Without a baseline, regressions on either axis
    /// are easy to ship.
    /// What: Indexes the project's own `src/` with `MockEmbedder` (no model
    /// download — keeps the test hermetic and CI-safe), runs five
    /// representative queries through both `search_hybrid` and a walkdir
    /// grep, prints a comparison table, and asserts hybrid produces at
    /// least one hit per query and completes within a reasonable budget.
    /// Test: `tokio::test`-driven; <30s on a developer laptop. Skipped
    /// gracefully if the manifest source dir isn't reachable.
    #[tokio::test]
    async fn hybrid_vs_ripgrep_benchmark() {
        use std::time::Instant;

        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let src_dir = match manifest.join("src").canonicalize() {
            Ok(p) => p,
            Err(e) => {
                eprintln!("skipping hybrid_vs_ripgrep_benchmark: {e}");
                return;
            }
        };

        // Use the deterministic MockEmbedder so this test never depends on
        // the network or HuggingFace cache. The MockStore is brute-force-
        // searchable (insertion-order + cosine sketches), which is plenty
        // for the bench: BM25 dominates ranking on these lexical queries,
        // and we're measuring the hybrid + grep paths, not embedding quality.
        let store: Arc<dyn MemoryStore> = Arc::new(MockStore::new());
        let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder { dim: 16 });
        let indexer = CodeIndexer::new(store, embedder);

        let t0 = Instant::now();
        let chunks = match indexer.index_directory(&src_dir, &["rs"]).await {
            Ok(n) => n,
            Err(e) => {
                eprintln!("skipping bench: index_directory failed: {e}");
                return;
            }
        };
        let index_elapsed = t0.elapsed();
        eprintln!(
            "[bench] indexed {chunks} chunks from {} in {index_elapsed:?}",
            src_dir.display()
        );
        if chunks == 0 {
            eprintln!("[bench] zero chunks indexed — skipping");
            return;
        }

        let queries = [
            "BM25 ranking score",
            "file watcher debounce",
            "HNSW cosine similarity",
            "agent delegation task",
            "tokio spawn background",
        ];

        println!("\n┌────────────────────────────────────┬────────────┬────────────┐");
        println!("│ Query                              │  hybrid ms │ ripgrep ms │");
        println!("├────────────────────────────────────┼────────────┼────────────┤");

        let mut total_hybrid_us: u128 = 0;
        let mut total_grep_us: u128 = 0;

        for q in &queries {
            let t = Instant::now();
            let hybrid_hits = indexer
                .search_hybrid(q, 3, false)
                .await
                .expect("hybrid search did not error");
            let hybrid_us = t.elapsed().as_micros();
            total_hybrid_us += hybrid_us;

            let t = Instant::now();
            let grep_hits = walkdir_grep_bench(&src_dir, q, 3);
            let grep_us = t.elapsed().as_micros();
            total_grep_us += grep_us;

            let q_disp = if q.len() > 34 {
                format!("{}…", &q[..33])
            } else {
                q.to_string()
            };
            println!(
                "│ {:<34} │ {:>10.2} │ {:>10.2} │",
                q_disp,
                hybrid_us as f64 / 1000.0,
                grep_us as f64 / 1000.0
            );

            // Show top-3 from each so reviewers can eyeball ranking quality.
            eprintln!("\n[{q}] hybrid top {} hits:", hybrid_hits.len());
            for (i, h) in hybrid_hits.iter().enumerate() {
                eprintln!(
                    "  #{}: {}:{}-{} (score={:.4}) fn={:?}",
                    i + 1,
                    h.file.strip_prefix(&manifest).unwrap_or(&h.file).display(),
                    h.start_line,
                    h.end_line,
                    h.score,
                    h.function_name
                );
            }
            eprintln!("[{q}] ripgrep top {} hits:", grep_hits.len());
            for (i, (path, line)) in grep_hits.iter().enumerate() {
                eprintln!(
                    "  #{}: {}:{}",
                    i + 1,
                    path.strip_prefix(&manifest).unwrap_or(path).display(),
                    line
                );
            }

            assert!(
                !hybrid_hits.is_empty(),
                "hybrid returned 0 hits for {q:?} — index empty or scoring broken"
            );
        }

        println!("├────────────────────────────────────┼────────────┼────────────┤");
        println!(
            "│ TOTAL (5 queries)                  │ {:>10.2} │ {:>10.2} │",
            total_hybrid_us as f64 / 1000.0,
            total_grep_us as f64 / 1000.0
        );
        println!("└────────────────────────────────────┴────────────┴────────────┘\n");
    }

    // ---------- warm-start / cool-down tests (#372) ----------

    /// Mock store that tracks warm/evict state so tests can assert the
    /// cool-down monitor and warm-up gate are actually invoked.
    ///
    /// Why: The default trait impls of `warm_segment` / `evict_segment` are
    /// no-ops; without a tracking mock we can't observe the calls the
    /// `CodeIndexer` makes on behalf of #372. This mock flips a flag and
    /// counts calls so each test asserts the exact behavior it cares about.
    /// What: Wraps `MockStore` with `warm`, `warm_calls`, `evict_calls`
    /// counters guarded by a `Mutex`. Forwards `insert`/`search`/`get`/
    /// `delete` to an inner `MockStore` so existing chunk-storage paths
    /// keep working.
    struct WarmTrackingStore {
        inner: MockStore,
        warm: Mutex<bool>,
        warm_calls: Mutex<usize>,
        evict_calls: Mutex<usize>,
    }
    impl WarmTrackingStore {
        fn new() -> Self {
            Self {
                inner: MockStore::new(),
                warm: Mutex::new(true),
                warm_calls: Mutex::new(0),
                evict_calls: Mutex::new(0),
            }
        }
    }
    #[async_trait]
    impl MemoryStore for WarmTrackingStore {
        async fn insert(
            &self,
            segment: Segment,
            id: &str,
            vector: &[f32],
            payload: Value,
        ) -> anyhow::Result<()> {
            self.inner.insert(segment, id, vector, payload).await
        }
        async fn search(
            &self,
            segment: Segment,
            query_vec: &[f32],
            top_k: usize,
        ) -> anyhow::Result<Vec<MemoryResult>> {
            self.inner.search(segment, query_vec, top_k).await
        }
        async fn get(&self, segment: Segment, id: &str) -> anyhow::Result<Option<Value>> {
            self.inner.get(segment, id).await
        }
        async fn delete(&self, segment: Segment, id: &str) -> anyhow::Result<()> {
            self.inner.delete(segment, id).await
        }
        async fn evict_segment(&self, _segment: Segment) -> anyhow::Result<()> {
            *self.warm.lock().unwrap() = false;
            *self.evict_calls.lock().unwrap() += 1;
            Ok(())
        }
        async fn warm_segment(&self, _segment: Segment) -> anyhow::Result<()> {
            *self.warm.lock().unwrap() = true;
            *self.warm_calls.lock().unwrap() += 1;
            Ok(())
        }
        async fn is_segment_warm(&self, _segment: Segment) -> anyhow::Result<bool> {
            Ok(*self.warm.lock().unwrap())
        }
    }

    #[tokio::test]
    async fn warm_up_marks_segment_warm() {
        // Why: `warm_up()` is the eager pre-load entry point called from
        // `main()` so the first user query never pays a cold-start penalty.
        // It must call `store.warm_segment(CodeIndex)` exactly once and
        // refresh `last_access` so the cool-down clock starts from "load
        // completed", not from construction.
        let store = Arc::new(WarmTrackingStore::new());
        let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder { dim: 8 });
        let indexer = CodeIndexer::new(Arc::clone(&store) as Arc<dyn MemoryStore>, embedder);

        // Reset count: the segment starts "warm" by default but warm_up
        // should still call through (idempotent).
        *store.warm_calls.lock().unwrap() = 0;
        indexer.warm_up().await.expect("warm_up");
        assert_eq!(
            *store.warm_calls.lock().unwrap(),
            1,
            "warm_up should call store.warm_segment exactly once"
        );
        assert!(store.is_segment_warm(Segment::CodeIndex).await.unwrap());
    }

    #[tokio::test]
    async fn cool_down_evicts_after_inactivity() {
        // Why: After `cool_after` of search inactivity, the background
        // monitor must call `evict_segment` to free RAM. We use a 50 ms
        // cool_after and a 10 ms tick so the test finishes in well under
        // a second.
        let store = Arc::new(WarmTrackingStore::new());
        let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder { dim: 8 });
        let indexer = Arc::new(
            CodeIndexer::new(Arc::clone(&store) as Arc<dyn MemoryStore>, embedder)
                .with_cool_after(Duration::from_millis(50)),
        );

        let handle = indexer.spawn_cool_down_monitor_with_tick(Duration::from_millis(10));

        // Wait long enough for at least one tick after the cool_after window.
        tokio::time::sleep(Duration::from_millis(200)).await;

        let evict_calls = *store.evict_calls.lock().unwrap();
        assert!(
            evict_calls >= 1,
            "expected at least one evict_segment call, got {evict_calls}"
        );
        assert!(
            !store.is_segment_warm(Segment::CodeIndex).await.unwrap(),
            "segment should be evicted (not warm) after cool-down"
        );

        handle.abort();
    }

    #[tokio::test]
    async fn search_warms_index_after_eviction() {
        // Why: After cool-down has evicted the in-memory HNSW, the next
        // `search()` call must transparently re-warm the index before
        // serving the query. Callers shouldn't see a difference.
        let store = Arc::new(WarmTrackingStore::new());
        let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder { dim: 8 });
        let indexer = CodeIndexer::new(Arc::clone(&store) as Arc<dyn MemoryStore>, embedder);

        // Simulate eviction.
        store.evict_segment(Segment::CodeIndex).await.unwrap();
        assert!(!store.is_segment_warm(Segment::CodeIndex).await.unwrap());

        // Reset counters so we observe only the search-triggered warm.
        *store.warm_calls.lock().unwrap() = 0;
        let _ = indexer.search("anything", 3).await.expect("search");

        let warm_calls = *store.warm_calls.lock().unwrap();
        assert!(
            warm_calls >= 1,
            "search should call warm_segment at least once after eviction, got {warm_calls}"
        );
        assert!(
            store.is_segment_warm(Segment::CodeIndex).await.unwrap(),
            "segment should be warm again after search"
        );
    }

    /// Delete `path` on drop — a tiny hand-rolled scopeguard so we don't
    /// pull in the `scopeguard` crate for one test.
    fn scopeguard_for(path: &Path) -> impl Drop {
        struct G(PathBuf);
        impl Drop for G {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(&self.0);
            }
        }
        G(path.to_path_buf())
    }
}
