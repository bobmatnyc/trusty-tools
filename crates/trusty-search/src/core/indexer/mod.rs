//! `CodeIndexer`: hybrid HNSW + BM25 + RRF search pipeline.
//!
//! Why: this is the central orchestrator that ties embeddings, vector search,
//! lexical search, and intent-based weight routing into a single `search()` call.
//! What: holds an `Embedder`, a `VectorStore`, and an in-memory chunk corpus;
//! `search()` runs both lanes in parallel, fuses with RRF, and returns the
//! top-k chunks with their fused score and per-result `match_reason`.
//! Test: see the `tests` submodule — RRF unit coverage lives in `search::rrf`,
//! and the integration test `test_search_integration` indexes 3 chunks and
//! verifies the most-relevant one ranks first.
//!
//! Module layout (issue #96 — god-object split):
//!   * `mod.rs` (this file): types, free helpers, struct definition, constructors.
//!   * `ingest`: add/index/batch parse+embed/commit pipeline.
//!   * `persist`: snapshot/restore + background incremental persist.
//!   * `files`: remove + lookup + entity-exact-match helpers.
//!   * `search`: hybrid query pipeline (HNSW + BM25 + RRF + KG + MMR).
//!   * `tests`: every test in one place so private fields stay accessible.

use std::collections::{hash_map::DefaultHasher, HashMap};
use std::hash::{Hash, Hasher};
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use lru::LruCache;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use crate::core::bm25::Bm25Index;
use crate::core::chunker::{ChunkType, RawChunk};
use crate::core::embed::Embedder;
use crate::core::entity::RawEntity;
use crate::core::store::VectorStore;
use crate::core::symbol_graph::SymbolGraph;

pub(crate) mod archive;
pub(crate) mod docs_penalty;
mod files;
mod ingest;
pub(crate) mod migrations;
mod persist;
mod search;

#[cfg(test)]
pub(crate) use search::KG_REFINE_THRESHOLD;

#[cfg(test)]
mod tests;

/// LRU capacity (entries) for the per-indexer query embedding cache.
const QUERY_CACHE_CAPACITY: usize = 256;
/// Oversample factor for the HNSW lane before RRF fusion.
pub(crate) const HNSW_OVERSAMPLE: usize = 4;
/// Default LRU capacity for the per-indexer chunk embedding cache.
///
/// Each entry is `dim × 4` bytes (384-dim f32 ≈ 1 536 B). 1 000 entries ≈
/// ~1.5 MB of RAM per index. Evicted entries are simply re-embedded on demand
/// (MMR rerank gracefully falls back when an embedding is missing). Lowered
/// from 10 000 → 1 000 (issue #79) after a daemon was observed at 43.9 GB RSS;
/// the cache was a meaningful contributor on multi-index hosts. Override
/// at runtime via `TRUSTY_EMBEDDING_CACHE`.
const DEFAULT_EMBEDDING_CACHE_CAP: usize = 1_000;

/// Read the embedding-cache LRU cap from the environment, with a sane default.
fn embedding_cache_cap() -> usize {
    std::env::var("TRUSTY_EMBEDDING_CACHE")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|&n: &usize| n > 0)
        .unwrap_or(DEFAULT_EMBEDDING_CACHE_CAP)
}

/// Default idle window (seconds) after which a durably-backed index's
/// in-memory `chunks` HashMap is evicted to reclaim heap.
///
/// Why (idle-memory audit, follow-up to the redb-cache + embedding-LRU quick
/// wins): the earlier wins capped the redb page cache and the chunk-embedding
/// LRU, but the raw `chunks: Arc<RwLock<HashMap<String, RawChunk>>>` still held
/// every chunk's *text* resident for the index's entire lifetime — on a 200k
/// chunk corpus that is hundreds of MB of `String` heap per index that the
/// query hot path no longer even reads (since issue #28 it materialises top-k
/// results straight from the mmap-backed redb corpus via
/// `CorpusStore::get_chunks`). An idle index therefore parks that whole map in
/// RAM for nothing. Evicting it after a quiet period reclaims the heap; the few
/// remaining in-memory readers (`grep_fallback_search`, `all_chunks`,
/// `enumerate_chunks`, the `fetch_chunks_for_ids` fallback) lazily rehydrate
/// from redb on the next access. 300 s (5 min) is long enough that an actively
/// queried index is never evicted mid-session, short enough that a daemon left
/// idle overnight shrinks back to its durable baseline.
/// What: 300 seconds, used by [`crate::core::indexer::idle_evict_secs`] when
/// `TRUSTY_CHUNKS_IDLE_EVICT_SECS` is unset.
/// Test: `idle_evict_secs_default_and_env_override`.
const DEFAULT_CHUNKS_IDLE_EVICT_SECS: u64 = 300;

/// Resolve the in-memory-chunks idle-eviction window (in seconds) from the
/// environment, falling back to [`DEFAULT_CHUNKS_IDLE_EVICT_SECS`].
///
/// Why: operators on memory-constrained hosts may want a tighter window
/// (evict sooner) while large-corpus hosts that re-query frequently may want
/// to disable eviction entirely. Making it env-tunable mirrors the
/// `TRUSTY_REDB_CACHE_MB` / `TRUSTY_EMBEDDING_CACHE` precedent without a
/// recompile.
/// What: reads `TRUSTY_CHUNKS_IDLE_EVICT_SECS` as `u64` seconds. A value of `0`
/// **disables** idle eviction (the in-memory map is never dropped). An
/// unset / empty / unparseable value falls back to the default (a warn is
/// logged on a non-empty unparseable value so typos surface).
/// Test: `idle_evict_secs_default_and_env_override`.
pub(crate) fn idle_evict_secs() -> u64 {
    match std::env::var("TRUSTY_CHUNKS_IDLE_EVICT_SECS") {
        Ok(v) if !v.is_empty() => match v.parse::<u64>() {
            Ok(n) => n,
            Err(_) => {
                tracing::warn!(
                    "indexer: TRUSTY_CHUNKS_IDLE_EVICT_SECS={v:?} is not a valid u64; \
                     using default ({DEFAULT_CHUNKS_IDLE_EVICT_SECS}s)"
                );
                DEFAULT_CHUNKS_IDLE_EVICT_SECS
            }
        },
        _ => DEFAULT_CHUNKS_IDLE_EVICT_SECS,
    }
}

/// Default hard cap on chunks per index. Also used as the HNSW
/// `max_elements`-style sanity guard. 200 000 chunks × ~5 KB metadata ≈ 1.0 GB
/// of RAM-resident chunk corpus on a single index. Lowered from 500 000 →
/// 200 000 (issue #79) — the previous default permitted >2.5 GB / index just
/// for chunk metadata, on top of HNSW and BM25 structures. Operators with
/// large monorepos can still raise this via `TRUSTY_MAX_CHUNKS`.
const DEFAULT_MAX_CHUNKS_PER_INDEX: usize = 200_000;

/// Read the per-index chunk cap from the environment, with a sane default.
pub(crate) fn max_chunks_per_index() -> usize {
    std::env::var("TRUSTY_MAX_CHUNKS")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|&n: &usize| n > 0)
        .unwrap_or(DEFAULT_MAX_CHUNKS_PER_INDEX)
}
/// Batch size for the fastembed ONNX call when bulk-indexing files.
///
/// 128 chunks per batch balances SIMD/tensor-setup amortisation against ONNX
/// session arena growth. ORT retains per-session activation buffers sized to
/// the largest batch it has seen; on large repos a 256-chunk batch combined
/// with a 512-file reindex batch caused the arena to grow into the tens of
/// GBs and trigger macOS Jetsam kills. 128 keeps the per-call tensor footprint
/// bounded while still being large enough to amortise ONNX kernel launch
/// overhead.
///
/// Override at runtime via `TRUSTY_MAX_BATCH_SIZE` (clamped to
/// `[EMBED_BATCH_MIN, EMBED_BATCH_MAX]`).
///
/// Default safety-net batch size when `TRUSTY_MAX_BATCH_SIZE` is unset.
///
/// Raised from 32 → 64 (issue #19): the ORT arena allocator is disabled on
/// the CPU path (`with_arena_allocator(false)` in trusty-common's embedder),
/// so transient allocation is freed per call and 64 is the minimum safe
/// value that amortises ONNX kernel launch overhead. The authoritative
/// per-tier default is still computed by [`crate::core::MemoryPolicy`] and
/// written back into `TRUSTY_MAX_BATCH_SIZE` before this function is called;
/// this constant only kicks in when the env is unset.
const DEFAULT_EMBED_BATCH_SIZE: usize = 64;
/// Floor for env-clamped batch size. Aligned with
/// `core::memory_policy::MIN_COMPUTED_BATCH_SIZE` (32). Raised from 8 → 32
/// (issue #19): with the CPU arena disabled the prior 200 MB/slot estimate
/// no longer applies, so 32 is the new minimum safe value.
const EMBED_BATCH_MIN: usize = 32;
/// Ceiling for env-clamped batch size. Aligned with
/// `core::memory_policy::MAX_COMPUTED_BATCH_SIZE` (512). The GPU opt-out path
/// (`TRUSTY_MAX_BATCH_SIZE_EXPLICIT=1` on CUDA/CoreML) reaches this ceiling
/// in production.
const EMBED_BATCH_MAX: usize = 512;

/// Read the embedding batch size from `TRUSTY_MAX_BATCH_SIZE`, clamped to
/// `[EMBED_BATCH_MIN, EMBED_BATCH_MAX]`. Falls back to `DEFAULT_EMBED_BATCH_SIZE`
/// when unset or unparseable.
///
/// Why: large repos can exhaust process memory if batches grow unbounded. This
/// gives operators a runtime knob to dial batch size up (faster indexing on
/// memory-rich hosts) or down (safer on constrained hosts) without rebuilding.
/// What: parses env, clamps via `.clamp()`. Filter-then-clamp ensures both
/// missing and zero values fall through to the default.
/// Test: see `tests::test_embed_batch_size_env_clamp`.
pub(crate) fn embed_batch_size() -> usize {
    std::env::var("TRUSTY_MAX_BATCH_SIZE")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|&n| n > 0)
        .map(|n| n.clamp(EMBED_BATCH_MIN, EMBED_BATCH_MAX))
        .unwrap_or(DEFAULT_EMBED_BATCH_SIZE)
}
/// Legacy default score multiplier applied to chunks brought in via KG
/// expansion. Retained for backwards-compat documentation: the live pipeline
/// now uses [`EdgeKind::score_multiplier`] (issue #18) so each edge type
/// contributes its own weight. Tests still reference this constant when
/// validating the `CallsFunction` baseline.
#[allow(dead_code)]
pub(crate) const KG_EXPAND_SCORE_FACTOR: f32 = 0.7;
/// Default BFS depth for KG expansion (1 hop = direct callers/callees only).
pub(crate) const KG_EXPAND_HOPS: usize = 1;

/// How many committed batches must elapse between background HNSW snapshots
/// (issue #29).
///
/// Why: `spawn_incremental_persist` used to fire after *every* committed
/// batch. The chunk corpus is already persisted transactionally per batch by
/// `commit_corpus_to_redb`, so the per-batch work that actually mattered for
/// crash-safety was the redb write — the HNSW `Index::save` is a pure backup
/// that takes hundreds of ms on a large graph. On a 14k-file reindex (128
/// files/batch → ~110 batches) that was ~110 full graph saves; throttling to
/// one every 16 batches cuts that to ~7 (plus one forced save at reindex
/// completion), reclaiming ~15+ seconds of redundant I/O without weakening
/// durability — a crash loses at most the last 16 batches' HNSW vectors,
/// which the next reindex re-embeds anyway, and the redb corpus is intact.
/// What: the batch-count modulus used by `spawn_incremental_persist`.
/// Test: `tests::test_incremental_persist_throttles_to_interval`.
pub(crate) const HNSW_SNAPSHOT_BATCH_INTERVAL: u32 = 16;

/// A search result returned to callers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeChunk {
    /// Collision-safe ID: "{path}:{start}:{end}"
    pub id: String,
    /// Absolute path to the source file on the current host (resolved from the
    /// stored root-relative path at query time). Consumers that need a
    /// mount-agnostic portable path should use [`path`] instead.
    pub file: String,
    /// Portable root-relative path stored in the corpus (e.g. `src/lib.rs`).
    ///
    /// Why (issue #674 — portable-paths feature): `file` is always resolved to
    /// an absolute host path at query time so existing consumers do not break,
    /// but operators on EFS / NFS mounts or multi-host pipelines need a path
    /// that survives the directory changing. `path` exposes the root-relative
    /// form directly from the redb corpus so consumers can build their own
    /// absolute path by joining with `root_path` as appropriate for their
    /// environment.
    /// `None` only for pre-#402 legacy chunks whose stored `file` was absolute
    /// (and thus cannot be stripped to a relative form at read time); these are
    /// repaired by the M004 migration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default)]
    pub language: Option<String>,
    pub start_line: usize,
    pub end_line: usize,
    pub content: String,
    pub function_name: Option<String>,
    pub score: f32,
    /// Compact 7-line snippet for token-efficient output
    pub compact_snippet: Option<String>,
    /// How this result was found: "hybrid", "hybrid+kg", "bm25", "vector", "fallback:ripgrep"
    pub match_reason: String,

    // Issue #29 — structural metadata propagated from RawChunk / entity extractor.
    /// Structural kind of this chunk (Function, Struct, Trait, …). Defaults to
    /// `Unknown` so older serialized payloads round-trip cleanly.
    #[serde(default)]
    pub chunk_type: ChunkType,
    /// Function/method names called within this chunk's body.
    #[serde(default)]
    pub calls: Vec<String>,
    /// Parent type names this chunk's type inherits from / implements.
    #[serde(default)]
    pub inherits_from: Vec<String>,
    /// Nesting depth of this chunk in the file's AST (0 = top-level).
    #[serde(default)]
    pub chunk_depth: u8,

    // Note: complexity metrics and git blame metadata are now owned by
    // trusty-analyzer (issue #71). Removing them here keeps `CodeChunk` lean
    // and avoids duplicating canonical computation.

    // Issue #10 — cross-project search fan-out: when a chunk is returned by
    // the global `POST /search` endpoint (or `search_all` MCP tool), this is
    // populated with the IndexId that produced it. `None` for per-index
    // search responses so older clients round-trip cleanly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index_id: Option<String>,

    // Issue #122 — branch-aware search: true if this chunk's file appears in
    // the branch-modified file set for this query. Always false when no
    // branch context was provided.
    #[serde(default)]
    pub on_branch: bool,

    /// Issue #75: short label explaining why this chunk was archive-downranked,
    /// when a score multiplier penalty was applied. Examples:
    /// `"path:deprecated"`, `"annotation:#[deprecated]"`, `"marker:.archived"`,
    /// `"stale:git_mtime"`. `None` when the chunk did not match any archive
    /// signal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archive_reason: Option<String>,
}

/// File-type filter mode for the unified search tool (issue #77, final
/// design).
///
/// Why: the same hybrid index serves four distinct callers — code search
/// (source files only), text/doc search (prose docs only), data search
/// (JSON/YAML/CSV/TOML/etc. only), and an unrestricted mode that returns
/// whatever the index produced. The previous multiplicative-penalty
/// design still let prose/config outrank source in code mode whenever the
/// raw BM25 score was high enough — CHANGELOG.md routinely came back at
/// rank 1. The revised design replaces the penalty matrix with a **hard
/// file-type filter** applied once per result after RRF / MMR /
/// materialization: chunks whose file is not in the allowed set for the
/// requested mode are dropped entirely. No score distortion, no
/// cross-contamination.
/// What: `Code` (default) returns only chunks from source-code extensions
/// (.rs, .ts, .py, .go, …). `Text` returns only prose / documentation
/// extensions (.md, .rst, .txt, …) plus path-based named docs (README*,
/// CHANGELOG*, LICENSE*, NOTICE*, CONTRIBUTING*) regardless of
/// extension. `Data` returns only structured-data / config / schema files
/// (.json, .yaml, .toml, .xml, .csv, .sql, lockfiles, …). `All` disables
/// the filter and returns every chunk the index produced. Archive
/// downranking (issue #75 — path keywords, `#[deprecated]`, marker files,
/// stale git mtime) still applies within each mode. Exposed to MCP /
/// HTTP callers as the lowercase strings `"code"`, `"text"`, `"data"`,
/// `"all"`.
/// Test: covered by `docs_penalty::tests` (per-mode allow/reject) and
/// the per-mode integration tests in `indexer::tests`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum SearchMode {
    /// Return only source-code chunks. Default for backward compatibility
    /// — existing callers (which used to receive prose/config in the
    /// top-k whenever it scored well) now get strictly source files,
    /// which is the historical intent of the `search` MCP tool.
    #[default]
    Code,
    /// Return only documentation / prose chunks (`.md`, `.rst`, `.txt`,
    /// `.adoc`, `.html`, …) plus path-based named docs (README*,
    /// CHANGELOG*, LICENSE*, NOTICE*, CONTRIBUTING*).
    Text,
    /// Return only structured-data / config / schema chunks (`.json`,
    /// `.yaml`, `.toml`, `.xml`, `.csv`, `.sql`, `.proto`, `.graphql`,
    /// lockfiles, Parquet/Avro, …).
    Data,
    /// No file-type filter — return whatever the index produced. Useful
    /// for queries that span code, docs, and data simultaneously.
    All,
}

/// Query parameters for hybrid search.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchQuery {
    pub text: String,
    #[serde(default = "default_top_k")]
    pub top_k: usize,
    #[serde(default = "default_true")]
    pub expand_graph: bool,
    #[serde(default = "default_true")]
    pub compact: bool,

    // Issue #122 — branch-aware search.
    /// Files modified on the current git branch (relative to index `root_path`).
    /// Chunks whose `file` appears here receive a `branch_boost` multiplier on
    /// their RRF score.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch_files: Option<Vec<String>>,

    /// RRF score multiplier for branch-modified chunks. Default `1.5`, range
    /// `[1.0, 3.0]`. `1.0` disables boosting. Values outside the range are
    /// clamped by the search pipeline.
    #[serde(default = "SearchQuery::default_branch_boost")]
    pub branch_boost: f32,

    /// Optional branch name hint (e.g. "feature/foo"). If `branch_files` is
    /// absent, the daemon will shell out to
    /// `git diff --name-only $(git merge-base HEAD <branch>)..HEAD` in the
    /// index `root_path` to compute the file list. Best-effort: failure logs
    /// a warning and falls back to no boost.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,

    /// Direction of the docs/source downranking penalty (issue #77).
    /// Defaults to [`SearchMode::Code`] so existing callers retain their
    /// behaviour. MCP / HTTP callers pass the lowercase strings `"code"`,
    /// `"text"`, or `"data"`.
    #[serde(default)]
    pub mode: SearchMode,

    /// Drop archived / deprecated / legacy chunks from the result set
    /// entirely instead of merely downranking them (issue #74).
    ///
    /// Why: archive downranking (issue #75) sinks legacy code but still
    /// returns it, which is the right default for exploratory queries. For
    /// code-navigation queries where archived code is pure noise, callers
    /// want it gone. This opt-in flag converts the downrank into a hard
    /// filter for chunks that match any archive signal (path keyword such as
    /// `_archive/`, `archive/`, `_deprecated/`, `old/`, `.archive/`; a
    /// `#[deprecated]` annotation; a `.archived` / `DEPRECATED` marker file).
    /// What: when `true`, the post-RRF `apply_archive_downrank` pass removes
    /// any chunk whose archive classifier fired instead of multiplying its
    /// score. Defaults to `false` so existing callers keep the downrank
    /// behaviour.
    #[serde(default)]
    pub exclude_archived: bool,

    /// Staged-pipeline lane selector (issue #109, Phase 1).
    ///
    /// Why: explicit caller opt-in to Stage-1-only search even on a
    /// fully-indexed index. Useful for grep-replacement use cases that
    /// don't want semantic noise. Independent of the index's `lexical_only`
    /// flag (which is a permanent setting at index-create time) — this is
    /// a per-query override.
    /// What: `None` (default) routes through all currently-ready stages.
    /// `Some(SearchStage::Lexical)` skips the HNSW lane regardless of
    /// what's ready.
    /// Test: `service::reindex::tests::stage_1_completes_and_search_works_before_embedding`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stage: Option<SearchStage>,

    /// Optional refining query for `search_kg` (issue #147).
    ///
    /// Why: when the seed chunk picked by stage 1 (`search_lexical`) is wrong,
    /// `search_kg`'s graph expansion compounds the error — every neighbour
    /// traversed is irrelevant to the user's intent. Providing a longer, more
    /// specific natural-language description here lets the search pipeline
    /// rerank the expanded neighbourhood by cosine similarity to the refining
    /// text and filter out low-relevance neighbours before returning.
    /// What: when `Some`, the `search` pipeline (for `stage = Graph`) embeds
    /// this string and uses cosine similarity to score every KG-expanded
    /// neighbour. Neighbours below [`KG_REFINE_THRESHOLD`] are dropped; the
    /// rest are reranked by their cosine score. When `None`, existing behaviour
    /// is preserved (no rerank, no filter). 100% backward compatible.
    /// Test: `test_kg_refine_query_filters_irrelevant_neighbours` and
    /// `test_kg_refine_query_none_preserves_all_neighbours` in
    /// `core::indexer::tests`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refine_query: Option<String>,
}

/// Stage selector for a single search query (issue #109, Phase 1; extended
/// in issue #138).
///
/// Why: lets callers (HTTP `?stage=...` or the per-lane MCP tools added in
/// #138) force a specific lane combination even when the index is fully
/// ready. Pushes intent classification to the LLM — the LLM picks
/// `search_lexical` / `search_semantic` / `search_kg` and the server simply
/// routes the lane mix.
///
/// What: a lowercase-serialised enum mapping each variant to a fixed lane
/// combination in [`CodeIndexer::search`]. `Lexical` runs BM25 plus the
/// grep-fallback only (no HNSW, no KG). `Semantic` runs BM25 plus HNSW
/// via RRF (no KG expansion, no community-cohesion bonus). `Graph` runs
/// the full BM25 plus HNSW plus KG expansion pipeline (hybrid AND).
/// `None` keeps the legacy adaptive behaviour where the daemon's
/// `search_capabilities` decides which lanes participate.
///
/// Test: `stage_1_completes_and_search_works_before_embedding` (lexical),
/// `search_semantic_stage_skips_kg_expansion` (semantic), and
/// `search_graph_stage_forces_kg_expansion_on_definition_query` (graph)
/// in `core::indexer::tests`. Tool-level routing is covered by
/// `mcp::tools::tests::search_*_tool_routes_to_*_stage`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SearchStage {
    /// BM25 + grep-fallback only — ripgrep-equivalent latency. Skips
    /// HNSW + KG regardless of index readiness.
    Lexical,
    /// BM25 + HNSW vector lane fused via RRF. Skips KG expansion. The
    /// embedder must be wired and Stage 2 ready; otherwise the HNSW
    /// lane silently contributes nothing.
    Semantic,
    /// BM25 + HNSW + KG expansion. Forces KG traversal regardless of
    /// query intent (which `expand_with_kg` would otherwise gate on
    /// `use_kg_first`). Equivalent to the full hybrid pipeline.
    Graph,
}

impl SearchQuery {
    /// Default RRF score multiplier for branch-modified chunks (issue #122).
    /// `1.5` is a gentle nudge that surfaces branch work without smothering
    /// stronger off-branch matches.
    pub fn default_branch_boost() -> f32 {
        1.5_f32
    }
}

impl Default for SearchQuery {
    /// Why: with the addition of branch-aware fields (issue #122) and the
    /// mode field (issue #77), call sites that previously built a 4-field
    /// `SearchQuery` would otherwise all need to spell out the new
    /// None/default fields. `Default` keeps those sites readable via
    /// `..Default::default()`.
    fn default() -> Self {
        Self {
            text: String::new(),
            top_k: default_top_k(),
            expand_graph: true,
            compact: true,
            branch_files: None,
            branch_boost: SearchQuery::default_branch_boost(),
            branch: None,
            mode: SearchMode::default(),
            exclude_archived: false,
            stage: None,
            refine_query: None,
        }
    }
}

fn default_top_k() -> usize {
    10
}
fn default_true() -> bool {
    true
}

/// Stable u64 hash of a query string. Used as the LRU cache key so we don't
/// retain the full string twice (LRU stores the embedding payload only).
pub(crate) fn hash_query(query: &str) -> u64 {
    let mut h = DefaultHasher::new();
    query.hash(&mut h);
    h.finish()
}

/// Build a 7-line snippet centered on the chunk content for token-efficient output.
pub(crate) fn build_compact_snippet(content: &str) -> String {
    let lines: Vec<&str> = content.lines().collect();
    if lines.len() <= 7 {
        return content.to_string();
    }
    // Take the first 7 lines — chunkers tend to put the most important header
    // (function signature, struct decl) at the top of the chunk.
    lines[..7].join("\n")
}

/// Resolve a stored chunk `file` string to an absolute path string.
///
/// Why (issue #402 — relocation resilience): as of the relative-path storage
/// change, newly indexed chunks store `file` as a path relative to
/// `root_path` (e.g. `"src/lib.rs"` instead of `"/Users/me/proj/src/lib.rs"`).
/// Older indexes still carry absolute paths. This helper normalises both
/// representations to an absolute path so all read-side callers — search
/// results, `list_chunks`, `get_call_chain`, MCP outputs — always return
/// absolute paths to callers, regardless of when the index was created.
///
/// What: if `raw_file` starts with the OS path separator (i.e. is already
/// absolute) it is returned as-is. Otherwise `root_path` is joined with
/// `raw_file` to produce an absolute path string.
///
/// Test: `tests::resolve_chunk_file_relative_becomes_absolute` and
///       `tests::resolve_chunk_file_absolute_passthrough` in `indexer::tests`.
pub(crate) fn resolve_chunk_file(raw_file: &str, root_path: &std::path::Path) -> String {
    // Detect an already-absolute path by checking the first byte. This avoids
    // Path::is_absolute() which allocates on some platforms; a leading '/' is
    // the only case we produce from the old write path on Unix/macOS.
    if std::path::Path::new(raw_file).is_absolute() {
        raw_file.to_string()
    } else {
        root_path.join(raw_file).to_string_lossy().into_owned()
    }
}

/// Materialize a `RawChunk` into a `CodeChunk` with the given score, match
/// reason, and optional compact snippet.
///
/// Why: four call sites (`similar_by_embedding`, `all_chunks`,
/// `enumerate_chunks`, the `search` materialization tail) used to inline the
/// same 18-field struct literal. Consolidating them removes ~60 lines of
/// duplication and the inevitable per-site drift when new fields are added.
/// What: clones every metadata field and derives `chunk_depth` (clamped to u8).
/// The `root_path` argument is used to resolve a relative `raw.file` to an
/// absolute path via [`resolve_chunk_file`] (issue #402) for the `file` field.
/// The `path` field (issue #674) is populated with the raw stored form when it
/// is already relative (the normal post-#402 case); it is `None` for legacy
/// absolute-path chunks not yet repaired by M004.
/// Test: covered indirectly by every search/materialization test in this file.
pub(crate) fn raw_to_code_chunk(
    raw: &RawChunk,
    score: f32,
    match_reason: &str,
    compact_snippet: Option<String>,
    root_path: &std::path::Path,
) -> CodeChunk {
    let chunk_depth: u8 = raw.chunk_depth.min(u8::MAX as usize) as u8;
    // `path` carries the raw stored (root-relative) form so consumers on
    // multi-host / EFS mounts can use a mount-agnostic key.  When the stored
    // value is absolute (pre-M002 legacy or missing M004 repair) we leave
    // `path` as `None` rather than propagating a wrong mount-specific prefix.
    let path = if !std::path::Path::new(&raw.file).is_absolute() {
        Some(raw.file.clone())
    } else {
        None
    };
    let file = resolve_chunk_file(&raw.file, root_path);
    CodeChunk {
        id: raw.id.clone(),
        file,
        path,
        language: raw.language.clone(),
        start_line: raw.start_line,
        end_line: raw.end_line,
        content: raw.content.clone(),
        function_name: raw.function_name.clone(),
        score,
        compact_snippet,
        match_reason: match_reason.to_string(),
        chunk_type: raw.chunk_type.clone(),
        calls: raw.calls.clone(),
        inherits_from: raw.inherits_from.clone(),
        chunk_depth,
        index_id: None,
        on_branch: false,
        archive_reason: None,
    }
}

/// Populate `virtual_terms` on each chunk from entities whose source line
/// falls within the chunk's `[start_line, end_line]` range.
///
/// Why: two call sites (`index_file` and `parse_and_embed_files`) used the
/// same dedupe-by-entity-text loop. Extracting prevents drift.
/// What: for each chunk, walks `entities` once, inserting each entity's text
/// at most once into a fresh `virtual_terms` vector.
/// Test: covered by `test_virtual_terms_populated_from_entities`.
pub(crate) fn populate_virtual_terms(chunks: &mut [RawChunk], entities: &[RawEntity]) {
    for chunk in chunks.iter_mut() {
        let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
        let mut terms: Vec<String> = Vec::new();
        for ent in entities {
            if ent.line >= chunk.start_line
                && ent.line <= chunk.end_line
                && seen.insert(ent.text.as_str())
            {
                terms.push(ent.text.clone());
            }
        }
        chunk.virtual_terms = terms;
    }
}

/// Score multiplier applied to a chunk for Definition-intent queries (issue #92).
///
/// Why: Definition queries (e.g. "struct CodeChunk fields") should surface the
/// canonical source-file declaration, not the Markdown / TOML / YAML file that
/// happens to mention the symbol many times. We demote doc/config files by 50%
/// only for Definition intent; Conceptual queries still surface `.md` docs.
/// What: returns `0.5` when the path ends with a known doc/config extension,
/// `1.0` otherwise.
/// Test: covered by `test_file_type_multiplier_demotes_docs` and the
/// integration test `test_definition_demotes_markdown_below_source`.
pub(crate) fn file_type_score_multiplier(path: &str) -> f32 {
    const DOC_EXTENSIONS: &[&str] = &[".md", ".txt", ".toml", ".yaml", ".yml", ".json"];
    let lower = path.to_ascii_lowercase();
    if DOC_EXTENSIONS.iter().any(|ext| lower.ends_with(ext)) {
        0.5
    } else {
        1.0
    }
}

/// Structural-definition score boost applied to a chunk when Definition-intent
/// queries hit a struct/enum/class/trait declaration whose `function_name`
/// contains a query token as a substring (case-insensitive) (issue #117).
///
/// Why: queries containing struct-name acronyms (`HNSW`, `BM25`, `RRF`, `ORT`)
/// or PascalCase identifiers were under-fired by the v0.8.2 ranker. The
/// classifier upgrade (#119) routes them to `Definition`, which already
/// demotes docs and runs the grep lane; the additional structural boost here
/// closes the gap on the cross-file case where the canonical declaration
/// (e.g. `hnsw_store.rs::HnswStore`) was being out-ranked by usage chunks
/// elsewhere in the codebase (e.g. `retrieval.rs` calling into HNSW). A 2.0×
/// multiplier is large enough to lift the declaration from rank ~8 to top-3
/// on the v0.8.1 benchmark scenarios but small enough not to drown out the
/// branch-modified boost (`1.0..=3.0`) when both fire on the same chunk.
/// What: a flat `2.0` multiplier. Symbolic so the constant is easy to find
/// when re-tuning.
/// Test: `test_struct_definition_boost_surfaces_struct_over_usage` in
/// `indexer::tests`.
pub(crate) const STRUCT_DEFINITION_BOOST: f32 = 2.0;

/// Decide whether `chunk_type` participates in the Definition-intent
/// structural boost (issue #117).
///
/// Why: the boost is intentionally narrow — only chunks that ARE the
/// declaration of a type are eligible. Free code, methods, and docstrings
/// stay on the default multiplier so usage and method-of-struct chunks
/// don't accidentally outrank the struct definition itself.
/// What: returns `true` for `Struct`, `Enum`, `Class`, `Trait`, and
/// `TypeAlias`; `false` for everything else. Note: `Function` and `Method`
/// are handled by [`is_function_definition_chunk_type`] (issue #122).
/// Test: covered indirectly by
/// `test_struct_definition_boost_surfaces_struct_over_usage`.
pub(crate) fn is_struct_definition_chunk_type(
    chunk_type: &crate::core::chunker::ChunkType,
) -> bool {
    use crate::core::chunker::ChunkType;
    matches!(
        chunk_type,
        ChunkType::Struct
            | ChunkType::Enum
            | ChunkType::Class
            | ChunkType::Trait
            | ChunkType::TypeAlias
    )
}

/// Decide whether `chunk_type` participates in the Definition-intent
/// function-definition boost (issue #122).
///
/// Why: the synthetic-corpus baseline (#123) reproduced a regression where
/// function-name queries (e.g. `BRUSILOV_EPOCH`, `get_call_chain`) returned
/// usage sites or string-literal occurrences at rank 1 instead of the
/// canonical function declaration. The existing struct-definition boost
/// (#117) deliberately excluded `Function`/`Method` because we assumed the
/// `inject_entity_exact_match` lane would carry them — but in practice
/// usage chunks with high BM25 TF can still out-rank the synthetic-injected
/// entity hit once RRF fuses lanes. Extending the boost to function-like
/// chunks closes that gap.
/// What: returns `true` for `Function` and `Method` (constructor variants
/// would also belong here but the current `ChunkType` enum has no
/// `Constructor` variant — tree-sitter constructors get classified as
/// `Function` or `Method` depending on the language); `false` for
/// everything else. Critically: `Constant` is excluded so chunks whose
/// only mention of the query token is a string literal (e.g.
/// `mcp_descriptor.rs` with `"get_call_chain"` in a JSON tool descriptor)
/// are NOT boosted.
/// Test: covered by
/// `test_function_definition_boost_surfaces_function_over_string_literal_usage`
/// and `test_method_definition_boost_fires`.
pub(crate) fn is_function_definition_chunk_type(
    chunk_type: &crate::core::chunker::ChunkType,
) -> bool {
    use crate::core::chunker::ChunkType;
    matches!(chunk_type, ChunkType::Function | ChunkType::Method)
}

/// Lowercase the meaningful query tokens for the Definition-intent structural
/// boost (issue #117).
///
/// Why: the boost only fires when a chunk's `function_name` literally matches
/// one of the query tokens. Tokenising the same way at boost-decision time
/// keeps the rule predictable and unit-testable.
/// What: splits on whitespace, drops tokens shorter than 2 characters, and
/// lowercases each remaining token. Whitespace-only or empty inputs return
/// an empty Vec.
/// Test: covered indirectly by
/// `test_struct_definition_boost_surfaces_struct_over_usage`.
pub(crate) fn definition_boost_query_tokens(query: &str) -> Vec<String> {
    query
        .split_whitespace()
        .filter(|t| t.len() >= 2)
        .map(|t| t.to_ascii_lowercase())
        .collect()
}

/// Map (`in_hnsw`, `in_bm25`, `in_kg`) booleans to a stable `match_reason`
/// label.
///
/// Why: lifted out of `search` to keep the materialization loop short and
/// to make the precedence rules unit-testable in isolation.
/// What: direct hits (HNSW and/or BM25) take precedence over KG-only paths.
/// Issue #75: the `(false,false,false)` arm is reserved for the grep
/// fallback lane and now returns the canonical `"fallback:ripgrep"` label.
/// Test: covered indirectly by `test_kg_expansion_marks_neighbours_with_hybrid_kg`
/// and `test_compute_match_reason_fallback_label`.
pub(crate) fn compute_match_reason(in_v: bool, in_b: bool, in_kg: bool) -> &'static str {
    match (in_v, in_b, in_kg) {
        (true, true, _) => "hybrid",
        (true, false, _) => "vector",
        (false, true, _) => "bm25",
        (false, false, true) => "hybrid+kg",
        (false, false, false) => "fallback:ripgrep",
    }
}

/// On-disk shape of a chunk corpus snapshot (issue #85). Stored as JSON next
/// to the HNSW snapshot so the daemon can restore an index without re-parsing
/// the source tree.
///
/// Why: BM25 + the symbol graph are both derivable from the chunk corpus, so
/// persisting just the chunks (and the per-file entity lists) is enough to
/// warm-boot the whole search pipeline. We deliberately do NOT persist BM25
/// posting lists — rebuilding them from chunks at load time is O(N tokens)
/// and avoids a second on-disk schema to migrate.
/// What: versioned wrapper around `Vec<RawChunk>` plus the entities map.
/// Test: covered by `tests::test_save_chunks_roundtrip`.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct ChunkSnapshot {
    /// File-format version. Bump when changing the shape so older daemons
    /// fall through to the empty-corpus branch instead of producing garbage.
    pub(crate) version: u32,
    pub(crate) chunks: Vec<RawChunk>,
    pub(crate) entities: Vec<(String, Vec<RawEntity>)>,
}

/// Output of the parse+embed phase: chunks paired with their (optional)
/// embeddings plus the per-file entity lists, ready to be committed into the
/// indexer's shared state. Held without any write lock so it can be shipped
/// between async tasks freely.
#[derive(Default)]
pub struct ParsedBatch {
    pub chunks: Vec<RawChunk>,
    /// `embeddings[i]` is `Some(vec)` iff an embedder was wired during parse.
    /// Always the same length as `chunks`.
    pub embeddings: Vec<Option<Vec<f32>>>,
    pub entities_by_file: Vec<(String, Vec<RawEntity>)>,
    /// Wall-clock time spent in `parse_files_parallel` (tree-sitter chunking).
    pub parse_ms: u64,
    /// Wall-clock time spent in `embed_chunks_in_batches` (ONNX embedding).
    /// `0` when no embedder was wired (BM25-only mode).
    pub embed_ms: u64,
    /// Number of chunks for which `Some(embedding)` was produced. `0` means
    /// the embedder was unavailable and the index degraded to BM25-only mode.
    pub vector_count: usize,
}

/// Per-batch timings emitted by [`CodeIndexer::commit_parsed_batch`]. Captures
/// the cost of the commit-phase work (BM25 ingest, vector upsert, KG rebuild).
///
/// Why: surfaced to the reindex orchestrator so it can accumulate per-subsystem
/// totals across all batches and emit them in the SSE `complete` event. This
/// gives operators visibility into where indexing time was actually spent and
/// is the smoking-gun signal for the "embedder silently fell back to BM25"
/// failure mode (`vector_count == 0` while `chunks > 0`).
#[derive(Debug, Default, Clone, Copy)]
pub struct CommitTimings {
    /// Chunks added by this commit. May be 0 if the batch was empty.
    pub chunks: usize,
    /// Time spent under the BM25 write lock ingesting tokens for this batch.
    pub bm25_ms: u64,
    /// Time spent in the HNSW `upsert_batch` call (vectors only).
    pub vector_upsert_ms: u64,
    /// Time spent rebuilding the symbol graph at the end of this commit. `0`
    /// when `defer_graph_rebuild=true` (the reindex orchestrator path).
    pub kg_ms: u64,
    /// Issue #100: chunks dropped by the per-index `TRUSTY_MAX_CHUNKS` cap
    /// inside this batch. Aggregated across the reindex by the orchestrator
    /// so the `complete` SSE event and `GET /indexes/:id/status` can flag
    /// indexes that were truncated by the budget — the silent partial-index
    /// failure mode where a gitignored subtree consumes the cap before the
    /// walker reaches the real source. Non-zero ⇒ the index is incomplete.
    pub chunks_dropped_by_cap: usize,
}

/// `CodeIndexer`: hybrid search engine for one named index.
///
/// Fields are crate-visible so the submodule `impl` blocks (`ingest`, `persist`,
/// `files`, `search`) can mutate state without going through accessors. They
/// remain `pub(super)`-equivalent from outside `core::indexer`.
pub struct CodeIndexer {
    pub index_id: String,
    pub root_path: std::path::PathBuf,

    pub(super) embedder: Option<Arc<dyn Embedder>>,
    pub(super) store: Option<Arc<dyn VectorStore>>,

    /// In-memory chunk corpus. Kept hot for sub-millisecond query-time
    /// materialization (`search.rs` joins fused `(id, score)` pairs against
    /// this map without any I/O). Issue #28 added [`Self::corpus`] as the
    /// durable redb backing store: `chunks` is now a write-through cache of
    /// the redb `CHUNKS_TABLE`, not the source of truth for persistence.
    pub(super) chunks: Arc<RwLock<HashMap<String, RawChunk>>>,

    /// Durable redb-backed chunk corpus (issue #28). `None` for BM25-only or
    /// test indexers built without a data dir. When `Some`, every committed
    /// batch is written here transactionally (replacing the old full-rewrite
    /// `chunks.json` snapshot) and the warm-boot path rehydrates `chunks` +
    /// `entities` from it. Held behind an `Arc` so the fire-and-forget persist
    /// task can own a clone without borrowing `&self`.
    pub(super) corpus: Option<Arc<crate::core::corpus::CorpusStore>>,

    /// Per-file entities extracted by `chunk_ast`. Keyed by file path.
    pub(super) entities: Arc<RwLock<HashMap<String, Vec<RawEntity>>>>,

    /// Cached chunk embeddings, keyed by `chunk_id`. Populated whenever an
    /// embedder is wired (`add_chunk` writes here). Used by the MMR diversity
    /// pass (#28) which needs vectors for already-ranked chunks without paying
    /// a re-embed or HNSW round-trip per candidate.
    ///
    /// Bounded by `embedding_cache_cap()` to keep the daemon from holding the
    /// entire corpus's embeddings in RAM (issue #75). Evicted entries are
    /// gracefully re-embedded on demand (MMR falls back to relevance-only when
    /// an entry is missing). Use `LruCache::put` / `peek` / `pop`.
    pub(super) chunk_embeddings: Arc<RwLock<LruCache<String, Vec<f32>>>>,

    /// Persistent BM25 index kept hot alongside the HNSW index. Mutated by
    /// `add_chunk` / `index_files_batch` / `remove_*` so the search hot path
    /// just acquires a read lock and runs `score_query_all` instead of
    /// rebuilding the entire posting list every query (was O(N) over all
    /// chunks; on a 115k-chunk index that dominated p50 latency by ~9s).
    pub(super) bm25: Arc<RwLock<Bm25Index>>,

    /// LRU cache of query → embedding, keyed by `hash_query`. Skips the embedder
    /// entirely on repeated queries — the daemon's "zero cold-start" promise.
    pub(super) query_cache: Arc<Mutex<LruCache<u64, Vec<f32>>>>,

    /// Call graph derived from the chunk corpus. Rebuilt cheaply after each
    /// corpus mutation; reads via `Arc::clone` are lock-free.
    pub(super) symbol_graph: Arc<RwLock<Arc<SymbolGraph>>>,

    /// Optional ONNX NER for `NaturalLanguagePhrase` extraction from doc
    /// comments (issue #23). Always present, but inert unless both the `ner`
    /// feature is compiled in and `~/.trusty-search/models/ner.onnx` exists.
    pub(super) ner: crate::core::ner::NerExtractor,

    /// Coalescing state for `spawn_incremental_persist` (memory-explosion fix).
    /// See [`PersistState`] for the full protocol description.
    pub(super) persist_state: Arc<PersistState>,

    /// Per-index domain vocabulary used by `QueryClassifier::classify_with_domain`
    /// at search time. Sourced from `trusty-search.yaml`'s `domain_terms:` field
    /// and forwarded by the daemon when constructing the indexer.
    ///
    /// Why: a query like "PMS integration" carries no syntactic signal the
    /// generic regex chain can match (no `fn`, `class`, `callers of`, …),
    /// so it falls into `Unknown` and gets generic weights. Per-index
    /// vocabulary lets the classifier nudge such queries to `Definition`
    /// intent, which routes them to the lexical-heavy weighting that finds
    /// the underlying symbol.
    /// What: a `Vec<String>` of case-insensitive substrings. Empty = standard
    /// classifier behaviour.
    /// Test: `tests::search_uses_domain_terms_when_provided`.
    pub(super) domain_terms: Vec<String>,

    /// Process-relative clock base for [`Self::last_activity_ms`].
    ///
    /// Why: `Instant` is not `Copy`-into-atomic, so we store activity as a
    /// `u64` millisecond offset from this monotonic base, which makes the
    /// touch on the search/ingest hot path a single relaxed atomic store
    /// instead of a lock acquisition.
    /// What: captured once at construction.
    pub(super) created_at: Instant,

    /// Milliseconds (relative to [`Self::created_at`]) of the most recent
    /// query or ingest activity. Used by [`Self::evict_chunks_if_idle`] to
    /// decide whether the in-memory `chunks` map can be dropped.
    ///
    /// Why: a lock-free activity timestamp lets a background ticker reclaim the
    /// heap held by idle indexes without contending with live searches.
    /// What: `AtomicU64`, updated by [`Self::touch_activity`] (relaxed store)
    /// on every search and successful commit.
    pub(super) last_activity_ms: Arc<AtomicU64>,

    /// `true` once the in-memory `chunks` map has been evicted because the
    /// index was idle and a durable corpus is wired.
    ///
    /// Why: the in-memory readers (`grep_fallback_search`, `all_chunks`,
    /// `enumerate_chunks`, the `fetch_chunks_for_ids` fallback) must know to
    /// rehydrate from redb before reading an empty map. A genuinely empty
    /// corpus (never indexed) must NOT trigger repeated rehydration attempts,
    /// so this flag distinguishes "evicted, reload on demand" from "empty".
    /// What: set by [`Self::evict_chunks_if_idle`], cleared by
    /// [`Self::ensure_chunks_loaded`] and by any ingest that repopulates the
    /// map.
    /// Test: `idle_eviction_drops_and_lazily_rehydrates_chunks`.
    pub(super) chunks_evicted: Arc<AtomicBool>,
}

/// Coalescing state for `spawn_incremental_persist`.
///
/// Why: prior to this guard, every call to `commit_parsed_batch` spawned a
/// fire-and-forget tokio task that cloned the **entire** chunk corpus
/// (every `RawChunk.content` String) into a `Vec<RawChunk>` and serialized
/// it to JSON. On a 200k-chunk corpus that's ~400 MB of `Vec<RawChunk>`
/// plus another ~800 MB of serialized `Vec<u8>` per task. A reindex emits
/// one commit per 128 files, so a 76 800-file repo would stack ~600 of
/// these tasks. With no concurrency limit, RAM ballooned to 46–174 GB
/// before the OS killed the daemon (observed on ~/Duetto/cto and
/// ~/Duetto/repos/duetto). The `TRUSTY_MEMORY_LIMIT_MB` poller could not
/// catch it because the runaway allocator was a detached task ladder, not
/// the reindex loop itself.
///
/// What: `in_flight` guarantees only one persist task is alive at a time
/// for this index; `dirty` lets later commits coalesce — when the running
/// task completes it re-runs once if `dirty` was set during its snapshot,
/// guaranteeing the on-disk file converges to the latest in-memory state
/// without ever allocating more than ~1× the corpus footprint.
///
/// Test: `tests::test_persist_coalesces_concurrent_calls`.
#[derive(Debug, Default)]
pub(crate) struct PersistState {
    /// True while a persist task is actively snapshotting + writing.
    pub(crate) in_flight: AtomicBool,
    /// Set by every caller before checking `in_flight`. The active task clears
    /// this before snapshotting; if any caller re-sets it during the snapshot
    /// the task loops once more so the final on-disk file reflects the latest
    /// committed state.
    pub(crate) dirty: AtomicBool,
    /// Monotonic count of committed batches that have requested a persist
    /// (issue #29). `spawn_incremental_persist` increments this and only
    /// actually spawns the (expensive) HNSW snapshot every
    /// [`HNSW_SNAPSHOT_BATCH_INTERVAL`] batches — redb already gives per-batch
    /// chunk durability, so a full `Index::save` after *every* 128-file batch
    /// (~110 saves on a 14k-file reindex, each hundreds of ms) is wasted I/O.
    /// A `force` persist (reindex completion, shutdown flush) bypasses the
    /// throttle so final state is always durable.
    pub(crate) batch_counter: AtomicU32,
}

impl CodeIndexer {
    /// Construct a bare indexer without an embedder/store. Call
    /// [`Self::with_components`] before invoking [`Self::search`] — otherwise
    /// search returns `Ok(vec![])` (BM25-only fallback uses the same path).
    pub fn new(index_id: impl Into<String>, root_path: impl Into<std::path::PathBuf>) -> Self {
        let cap =
            NonZeroUsize::new(QUERY_CACHE_CAPACITY).expect("QUERY_CACHE_CAPACITY must be non-zero");
        let emb_cap = NonZeroUsize::new(embedding_cache_cap())
            .expect("embedding_cache_cap must be non-zero (env var filtered)");
        Self {
            index_id: index_id.into(),
            root_path: root_path.into(),
            embedder: None,
            store: None,
            corpus: None,
            chunks: Arc::new(RwLock::new(HashMap::new())),
            entities: Arc::new(RwLock::new(HashMap::new())),
            chunk_embeddings: Arc::new(RwLock::new(LruCache::new(emb_cap))),
            bm25: Arc::new(RwLock::new(Bm25Index::new())),
            query_cache: Arc::new(Mutex::new(LruCache::new(cap))),
            symbol_graph: Arc::new(RwLock::new(Arc::new(SymbolGraph::new()))),
            ner: crate::core::ner::NerExtractor::try_load(),
            persist_state: Arc::new(PersistState::default()),
            domain_terms: Vec::new(),
            created_at: Instant::now(),
            last_activity_ms: Arc::new(AtomicU64::new(0)),
            chunks_evicted: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Record that this index was just queried or ingested.
    ///
    /// Why: the idle-eviction ticker needs a cheap, lock-free "when was this
    /// index last touched?" signal so it never drops the in-memory `chunks`
    /// map out from under an actively-used index.
    /// What: stores the milliseconds elapsed since [`Self::created_at`] into
    /// [`Self::last_activity_ms`] with `Relaxed` ordering (a stale read by the
    /// ticker only delays eviction by one tick — never causes incorrect
    /// behaviour). Saturates at `u64::MAX` for absurdly long-lived processes.
    /// Test: `idle_eviction_drops_and_lazily_rehydrates_chunks` touches then
    /// asserts eviction is skipped within the window.
    pub(super) fn touch_activity(&self) {
        let ms = self.created_at.elapsed().as_millis().min(u64::MAX as u128) as u64;
        self.last_activity_ms.store(ms, Ordering::Relaxed);
    }

    /// Milliseconds since the last recorded activity (query/ingest).
    ///
    /// Why: lets the eviction logic compare elapsed idle time against the
    /// configured window without exposing the raw atomic.
    /// What: `created_at.elapsed() - last_activity_ms`, floored at 0.
    /// Test: covered via `evict_chunks_if_idle`'s behaviour tests.
    fn idle_duration(&self) -> std::time::Duration {
        let now_ms = self.created_at.elapsed().as_millis().min(u64::MAX as u128) as u64;
        let last = self.last_activity_ms.load(Ordering::Relaxed);
        std::time::Duration::from_millis(now_ms.saturating_sub(last))
    }

    /// Number of chunks currently resident in the in-memory `chunks` map.
    ///
    /// Why: tests and the eviction ticker want a direct read of the in-memory
    /// footprint (distinct from the durable `corpus.chunk_count()`), so they
    /// can verify eviction actually dropped the map and rehydration refilled
    /// it.
    /// What: returns `self.chunks.read().len()`.
    /// Test: `idle_eviction_drops_and_lazily_rehydrates_chunks`.
    pub async fn in_memory_chunk_count(&self) -> usize {
        self.chunks.read().await.len()
    }

    /// Drop the in-memory `chunks` map when the index has been idle longer
    /// than `idle_threshold` and a durable corpus can repopulate it.
    ///
    /// Why: see [`DEFAULT_CHUNKS_IDLE_EVICT_SECS`] — the raw chunk-text map is
    /// the single largest idle-heap contributor per index and is unused on the
    /// query hot path once a redb corpus is wired. Reclaiming it on idle shrinks
    /// a quiet daemon back to its durable baseline without affecting an active
    /// one.
    /// What: a no-op when (a) `idle_threshold` is zero (eviction disabled), (b)
    /// no durable `corpus` is wired (the map is then the *only* copy and cannot
    /// be safely dropped), (c) the map is already empty, or (d) the index has
    /// been active within the window. Otherwise clears the map, marks
    /// [`Self::chunks_evicted`], and logs an `info` with the reclaimed count.
    /// BM25 and the symbol graph are intentionally left hot — they are separate
    /// structures the query path still reads, and they hold no large `String`
    /// content. Returns the number of chunks evicted (0 when skipped).
    /// Test: `idle_eviction_drops_and_lazily_rehydrates_chunks`.
    pub async fn evict_chunks_if_idle(&self, idle_threshold: std::time::Duration) -> usize {
        if idle_threshold.is_zero() {
            return 0;
        }
        // Without a durable corpus the in-memory map is the only copy — dropping
        // it would lose data, so never evict in BM25-only / test mode.
        if self.corpus.is_none() {
            return 0;
        }
        if self.idle_duration() < idle_threshold {
            return 0;
        }
        let mut chunks = self.chunks.write().await;
        if chunks.is_empty() {
            return 0;
        }
        let evicted = chunks.len();
        chunks.clear();
        chunks.shrink_to_fit();
        drop(chunks);
        self.chunks_evicted.store(true, Ordering::Relaxed);
        tracing::info!(
            "index '{}': evicted {} in-memory chunks after {}s idle \
             (durable corpus retained; lazily rehydrates on next access)",
            self.index_id,
            evicted,
            idle_threshold.as_secs(),
        );
        evicted
    }

    /// Repopulate the in-memory `chunks` map from the durable corpus if it was
    /// previously evicted while idle.
    ///
    /// Why: the in-memory readers (`grep_fallback_search`, `all_chunks`,
    /// `raw_chunks_snapshot`, `enumerate_chunks`, and the `fetch_chunks_for_ids`
    /// fallback) must observe a populated map. After an idle eviction the map
    /// is empty *and* `chunks_evicted` is set; this restores it lazily on the
    /// next such access so eviction is transparent to callers.
    /// What: a fast no-op (single relaxed atomic load) when the map was never
    /// evicted. When evicted, reloads every chunk from `CorpusStore` on a
    /// blocking worker and refills the map, then clears the flag. Concurrency
    /// is safe: the flag is cleared only after a successful refill, and a
    /// double refill (two readers racing) is idempotent because each inserts
    /// the same `id → chunk` rows. Errors are logged at `warn` and leave the
    /// flag set so a later access retries — a transient redb read failure must
    /// not permanently blank the map.
    /// Test: `idle_eviction_drops_and_lazily_rehydrates_chunks`.
    pub(super) async fn ensure_chunks_loaded(&self) {
        if !self.chunks_evicted.load(Ordering::Relaxed) {
            return;
        }
        let Some(corpus) = self.corpus.clone() else {
            // No corpus to reload from — clear the flag so we don't spin.
            self.chunks_evicted.store(false, Ordering::Relaxed);
            return;
        };
        let index_id = self.index_id.clone();
        let loaded = tokio::task::spawn_blocking(move || corpus.load_all_chunks()).await;
        match loaded {
            Ok(Ok(chunks)) => {
                let n = chunks.len();
                let mut map = self.chunks.write().await;
                for chunk in chunks {
                    map.insert(chunk.id.clone(), chunk);
                }
                drop(map);
                self.chunks_evicted.store(false, Ordering::Relaxed);
                tracing::info!(
                    "index '{index_id}': rehydrated {n} chunks from redb after idle eviction"
                );
            }
            Ok(Err(e)) => tracing::warn!(
                "index '{index_id}': failed to rehydrate chunks from redb ({e}); \
                 will retry on next access"
            ),
            Err(e) => tracing::warn!(
                "index '{index_id}': chunk rehydration task panicked ({e}); \
                 will retry on next access"
            ),
        }
    }

    /// Builder-style setter for the per-index domain vocabulary.
    ///
    /// Why: lets the daemon attach `trusty-search.yaml`'s `domain_terms:`
    /// without leaking the field into every constructor call site.
    /// What: stores the vector verbatim (case-insensitive matching happens
    /// inside `classify_with_domain`).
    /// Test: see `tests::search_uses_domain_terms_when_provided`.
    pub fn with_domain_terms(mut self, terms: Vec<String>) -> Self {
        self.domain_terms = terms;
        self
    }

    /// Replace the per-index domain vocabulary in place. Used by the daemon
    /// when restoring a persisted index — we already have an indexer via
    /// `build_indexer_with_persisted_state` and just need to attach the
    /// vocabulary alongside it.
    pub fn set_domain_terms(&mut self, terms: Vec<String>) {
        self.domain_terms = terms;
    }

    /// Returns a cheap `Arc` snapshot of the current symbol graph.
    ///
    /// Why: the `GET /indexes/{id}/graph` endpoint (issue #128) needs to read
    /// the whole symbol graph from `src/service/server.rs`, but the
    /// `symbol_graph` field is `pub(super)` and guarded by a lock. This exposes
    /// a public, lock-free-after-clone accessor: it holds the read lock only
    /// long enough to bump the `Arc` refcount, then hands the caller an
    /// independent snapshot that won't block indexing.
    /// What: clones the inner `Arc<SymbolGraph>` while holding the read lock.
    /// Test: covered by the `graph_handler` integration path; the underlying
    /// `SymbolGraph` accessors are unit-tested in `core::symbol_graph::tests`.
    pub async fn snapshot_symbol_graph(&self) -> Arc<SymbolGraph> {
        Arc::clone(&*self.symbol_graph.read().await)
    }

    /// Borrow the (optional) durable corpus store.
    ///
    /// Why: the reindex orchestrator and symbol-graph paths need the
    /// `CorpusStore` without exposing every internal field. The `Option` and
    /// `Arc::clone` are cheap and let the caller hold the store independently
    /// of any read lock.
    /// What: returns `Some(Arc::clone)` when the corpus is wired, `None` for
    /// BM25-only / test indexers built without a data dir.
    /// Test: indirectly via `service::reindex` graph rebuild trigger.
    pub fn corpus_store(&self) -> Option<Arc<crate::core::corpus::CorpusStore>> {
        self.corpus.as_ref().map(Arc::clone)
    }

    /// Attach the embedder and vector store so the full hybrid pipeline can run.
    /// Builder-style; returns `self` for chaining.
    pub fn with_components(
        mut self,
        embedder: Arc<dyn Embedder>,
        store: Arc<dyn VectorStore>,
    ) -> Self {
        self.embedder = Some(embedder);
        self.store = Some(store);
        self
    }

    /// Attach a durable redb-backed [`crate::core::corpus::CorpusStore`]
    /// (issue #28).
    ///
    /// Why: the daemon resolves one `index.redb` per index under its data dir
    /// and wires it in before warm-boot. Test / BM25-only indexers that have
    /// no data dir simply skip this call and run with `corpus: None` — the
    /// ingest path treats a missing corpus store as "in-memory only", so they
    /// behave exactly as before issue #28.
    /// What: stores the `Arc` so both the ingest commit path and the
    /// fire-and-forget persist task can reach it.
    /// Test: `tests::test_corpus_store_roundtrip` builds an indexer with a
    /// `CorpusStore`, commits a batch, and asserts a fresh indexer restores it.
    pub fn set_corpus_store(&mut self, corpus: Arc<crate::core::corpus::CorpusStore>) {
        self.corpus = Some(corpus);
    }

    /// Swap in a new durable corpus store, returning the one it replaced
    /// (issue #28, Phase 4).
    ///
    /// Why: a `--force` reindex stages its rebuilt corpus in a temp
    /// `index.redb.tmp`. The orchestrator swaps that staging store onto the
    /// indexer for the duration of the reindex so every `commit_parsed_batch`
    /// writes the new corpus to the temp file (never touching the live
    /// `index.redb`), then swaps the finalized store back afterwards. Returning
    /// the previous store lets the caller drop the live store's open handle
    /// before the atomic rename — redb keeps the file mapped while any handle
    /// is alive, so the rename-over must happen with no live `Arc` left.
    /// What: replaces `self.corpus` with `Some(corpus)` and returns the prior
    /// value.
    /// Test: `tests::test_force_reindex_atomic_corpus_swap`.
    pub fn swap_corpus_store(
        &mut self,
        corpus: Arc<crate::core::corpus::CorpusStore>,
    ) -> Option<Arc<crate::core::corpus::CorpusStore>> {
        self.corpus.replace(corpus)
    }

    /// Take the durable corpus store out of the indexer, leaving `None`
    /// (issue #28, Phase 4).
    ///
    /// Why: to atomically rename the staging corpus file over the live one,
    /// every `Arc<CorpusStore>` clone pointing at either file must first be
    /// dropped (redb holds the file open as long as a handle exists). The
    /// reindex orchestrator calls this to extract the staging store so it can
    /// drop the last `Arc`, perform the rename, and then re-open + re-install
    /// a store pointing at the now-swapped `index.redb`.
    /// What: `Option::take` on `self.corpus`.
    /// Test: `tests::test_force_reindex_atomic_corpus_swap`.
    pub fn take_corpus_store(&mut self) -> Option<Arc<crate::core::corpus::CorpusStore>> {
        self.corpus.take()
    }

    /// True iff a durable corpus store is currently wired (issue #28).
    ///
    /// Why: the `--force` reindex orchestrator only performs the atomic
    /// staging-file swap when the index actually has a durable corpus —
    /// BM25-only / test indexers run with `corpus: None` and must skip the
    /// swap entirely.
    /// What: `self.corpus.is_some()`.
    /// Test: covered by `tests::test_force_reindex_atomic_corpus_swap`.
    pub fn has_corpus_store(&self) -> bool {
        self.corpus.is_some()
    }

    /// Whether this indexer has an embedder wired (issue #601).
    ///
    /// Why: the reindex non-empty gate must distinguish "no embedder configured
    /// → legitimately produces zero vectors (BM25-only / test indexer)" from
    /// "embedder present but produced zero vectors → silent embed failure". The
    /// gate only fires in the latter case, so it consults this accessor before
    /// declaring a zero-vector reindex a failure.
    /// What: `self.embedder.is_some()`.
    /// Test: `validate::reindex_outcome` unit tests model both branches; the
    /// end-to-end wiring is covered by the BM25-only reindex tests, which must
    /// still complete (no embedder → not failed).
    pub fn has_embedder(&self) -> bool {
        self.embedder.is_some()
    }
}
