//! Core data types for the search pipeline.
//!
//! Why: extracted from `indexer/mod.rs` (issue #607) to keep that file
//! under the 500-SLOC hard cap. All the externally-visible structs and enums
//! used throughout the indexer belong here.
//! What: `CodeChunk`, `SearchMode`, `SearchQuery`, `SearchStage`,
//! `ChunkSnapshot`, `ParsedBatch`, `CommitTimings`, and the small default-value
//! free functions.
//! Test: these types are exercised by virtually every search and ingest test in
//! `indexer::tests` and `tests/integration_tests.rs`.

use serde::{Deserialize, Serialize};

use crate::core::chunker::{ChunkType, RawChunk};
use crate::core::entity::RawEntity;

/// A search result returned to callers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeChunk {
    /// Collision-safe ID: "{path}:{start}:{end}"
    pub id: String,
    /// Absolute path to the source file on the current host.
    pub file: String,
    /// Portable root-relative path stored in the corpus (e.g. `src/lib.rs`).
    ///
    /// Why (issue #674): `file` is always resolved to an absolute host path
    /// at query time so existing consumers do not break, but operators on
    /// EFS / NFS mounts or multi-host pipelines need a path that survives the
    /// directory changing. `path` exposes the root-relative form directly.
    /// `None` only for pre-#402 legacy chunks whose stored `file` was absolute.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default)]
    pub language: Option<String>,
    pub start_line: usize,
    pub end_line: usize,
    pub content: String,
    pub function_name: Option<String>,
    pub score: f32,
    /// Compact 7-line snippet for token-efficient output.
    pub compact_snippet: Option<String>,
    /// How this result was found: "hybrid", "hybrid+kg", "bm25", "vector", "fallback:ripgrep"
    pub match_reason: String,

    // Issue #29 — structural metadata propagated from RawChunk / entity extractor.
    /// Structural kind of this chunk (Function, Struct, Trait, …).
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

    // Issue #10 — cross-project search fan-out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index_id: Option<String>,

    // Issue #122 — branch-aware search.
    #[serde(default)]
    pub on_branch: bool,

    /// Issue #75: short label explaining why this chunk was archive-downranked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archive_reason: Option<String>,
}

/// File-type filter mode for the unified search tool (issue #77).
///
/// Why: the same hybrid index serves four distinct callers — code search,
/// text/doc search, data search, and an unrestricted mode. Hard file-type
/// filtering replaces a penalty matrix to avoid cross-contamination.
/// What: `Code` (default) returns only source-code extensions. `Text` returns
/// only prose / documentation. `Data` returns only structured-data / config.
/// `All` disables the filter.
/// Test: covered by `docs_penalty::tests` and per-mode integration tests in
/// `indexer::tests`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum SearchMode {
    /// Return only source-code chunks. Default for backward compatibility.
    #[default]
    Code,
    /// Return only documentation / prose chunks.
    Text,
    /// Return only structured-data / config / schema chunks.
    Data,
    /// No file-type filter — return whatever the index produced.
    All,
}

/// Stage selector for a single search query (issue #109, Phase 1).
///
/// Why: lets callers (HTTP `?stage=...` or the per-lane MCP tools added in
/// #138) force a specific lane combination even when the index is fully
/// ready.
/// What: `Lexical` runs BM25 + grep only. `Semantic` runs BM25 + HNSW via
/// RRF. `Graph` runs the full BM25 + HNSW + KG expansion pipeline.
/// `None` keeps the legacy adaptive behaviour.
/// Test: `stage_1_completes_and_search_works_before_embedding` (lexical),
/// `search_semantic_stage_skips_kg_expansion` (semantic), and
/// `search_graph_stage_forces_kg_expansion_on_definition_query` (graph).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SearchStage {
    /// BM25 + grep-fallback only.
    Lexical,
    /// BM25 + HNSW vector lane fused via RRF. Skips KG expansion.
    Semantic,
    /// BM25 + HNSW + KG expansion.
    Graph,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch_files: Option<Vec<String>>,
    #[serde(default = "SearchQuery::default_branch_boost")]
    pub branch_boost: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,

    /// Direction of the docs/source downranking penalty (issue #77).
    #[serde(default)]
    pub mode: SearchMode,

    /// Drop archived / deprecated / legacy chunks entirely (issue #74).
    #[serde(default)]
    pub exclude_archived: bool,

    /// Staged-pipeline lane selector (issue #109, Phase 1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stage: Option<SearchStage>,

    /// Optional refining query for `search_kg` (issue #147).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refine_query: Option<String>,
}

impl SearchQuery {
    /// Default RRF score multiplier for branch-modified chunks (issue #122).
    pub fn default_branch_boost() -> f32 {
        1.5_f32
    }
}

impl Default for SearchQuery {
    /// Why: with the addition of branch-aware fields (issue #122) and the
    /// mode field (issue #77), call sites that previously built a 4-field
    /// `SearchQuery` would otherwise all need to spell out the new
    /// None/default fields.
    /// What: fills every field with its documented default.
    /// Test: covered by every search call site that uses `..Default::default()`.
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

/// On-disk shape of a chunk corpus snapshot (issue #85).
///
/// Why: BM25 + the symbol graph are both derivable from the chunk corpus, so
/// persisting just the chunks (and the per-file entity lists) is enough to
/// warm-boot the whole search pipeline.
/// What: versioned wrapper around `Vec<RawChunk>` plus the entities map.
/// Test: covered by `tests::test_save_chunks_roundtrip`.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct ChunkSnapshot {
    pub(crate) version: u32,
    pub(crate) chunks: Vec<RawChunk>,
    pub(crate) entities: Vec<(String, Vec<RawEntity>)>,
}

/// Output of the parse+embed phase: chunks paired with their (optional)
/// embeddings plus the per-file entity lists.
#[derive(Default)]
pub struct ParsedBatch {
    pub chunks: Vec<RawChunk>,
    /// `embeddings[i]` is `Some(vec)` iff an embedder was wired during parse.
    /// Always the same length as `chunks`.
    pub embeddings: Vec<Option<Vec<f32>>>,
    pub entities_by_file: Vec<(String, Vec<RawEntity>)>,
    pub parse_ms: u64,
    pub embed_ms: u64,
    pub vector_count: usize,
}

impl ParsedBatch {
    /// Filter out all chunks (and their paired embeddings and entity lists) for
    /// files that match the `exclude` predicate, returning the reduced batch.
    ///
    /// Why (issue #1002): when a pre-commit `remove_file_no_kg_rebuild` call
    /// fails, inserting the new chunks for that file on top of the surviving
    /// stale chunks would produce duplicate search results. Calling this method
    /// before `commit_parsed_batch` skips those files entirely — the insert
    /// is deferred to the next `--force` reindex when the remove can succeed.
    /// What: retains only (chunk, embedding) pairs where `keep(&chunk.file)` is
    /// true; filters `entities_by_file` with the same predicate. Timing fields
    /// are left unchanged (they reflect actual work done, not committed chunks).
    /// Test: `retain_files_filters_chunks_and_entities` in `indexer::tests`.
    pub(crate) fn retain_files<F: Fn(&str) -> bool>(mut self, keep: F) -> Self {
        // `chunks` and `embeddings` are parallel vecs of the same length.
        // Iterate with indices so we can collect the matching pairs in one pass.
        let mut new_chunks = Vec::with_capacity(self.chunks.len());
        let mut new_embeddings = Vec::with_capacity(self.embeddings.len());
        for (chunk, embedding) in self.chunks.drain(..).zip(self.embeddings.drain(..)) {
            if keep(&chunk.file) {
                new_chunks.push(chunk);
                new_embeddings.push(embedding);
            }
        }
        self.chunks = new_chunks;
        self.embeddings = new_embeddings;
        self.entities_by_file
            .retain(|(file, _)| keep(file.as_str()));
        self
    }
}

/// Per-batch timings emitted by [`crate::core::indexer::CodeIndexer::commit_parsed_batch`].
///
/// Why: surfaced to the reindex orchestrator so it can accumulate per-subsystem
/// totals across all batches and emit them in the SSE `complete` event.
#[derive(Debug, Default, Clone, Copy)]
pub struct CommitTimings {
    pub chunks: usize,
    pub bm25_ms: u64,
    pub vector_upsert_ms: u64,
    pub kg_ms: u64,
    /// Issue #100: chunks dropped by the per-index `TRUSTY_MAX_CHUNKS` cap.
    pub chunks_dropped_by_cap: usize,
}
