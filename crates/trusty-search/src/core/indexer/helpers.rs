//! Free-function helpers for [`CodeIndexer`].
//!
//! Why: the original `mod.rs` bundled a mix of constant/env readers, codec
//! helpers, and score-adjustment free functions alongside the struct
//! definition and constructors. Extracting them here reduces `mod.rs` below
//! the 500-line cap while keeping each helper easy to find by concern.
//! What: env readers (`embedding_cache_cap`, `idle_evict_secs`,
//! `max_chunks_per_index`, `embed_batch_size`), codec helpers
//! (`hash_query`, `build_compact_snippet`, `resolve_chunk_file`,
//! `raw_to_code_chunk`, `populate_virtual_terms`), and score helpers
//! (`file_type_score_multiplier`, `is_struct_definition_chunk_type`,
//! `is_function_definition_chunk_type`, `definition_boost_query_tokens`,
//! `compute_match_reason`).
//! Test: see `indexer::tests` — every function here is exercised transitively
//! by the search and ingest integration tests; several have dedicated unit
//! tests (`test_embed_batch_size_env_clamp`,
//! `idle_evict_secs_default_and_env_override`, etc.).

use crate::core::chunker::RawChunk;
use crate::core::entity::RawEntity;

use super::CodeChunk;

// ─── Batch / cache sizing ────────────────────────────────────────────────────

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
///
/// Why: lets operators tune the in-memory embedding LRU without a recompile.
/// What: reads `TRUSTY_EMBEDDING_CACHE` as a positive usize; falls back to
/// [`DEFAULT_EMBEDDING_CACHE_CAP`] when unset, zero, or unparseable.
/// Test: covered indirectly by every test that constructs a `CodeIndexer`.
pub(crate) fn embedding_cache_cap() -> usize {
    std::env::var("TRUSTY_EMBEDDING_CACHE")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|&n: &usize| n > 0)
        .unwrap_or(DEFAULT_EMBEDDING_CACHE_CAP)
}

/// Default idle window (seconds) after which a durably-backed index's
/// in-memory `chunks` HashMap is evicted to reclaim heap.
pub(crate) const DEFAULT_CHUNKS_IDLE_EVICT_SECS: u64 = 300;

/// Resolve the in-memory-chunks idle-eviction window (in seconds) from the
/// environment, falling back to [`DEFAULT_CHUNKS_IDLE_EVICT_SECS`].
///
/// Why: operators on memory-constrained hosts may want a tighter window
/// (evict sooner) while large-corpus hosts that re-query frequently may want
/// to disable eviction entirely.
/// What: reads `TRUSTY_CHUNKS_IDLE_EVICT_SECS` as `u64` seconds. A value of
/// `0` **disables** idle eviction. Unset / unparseable falls back to default.
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

/// Default hard cap on chunks per index.
const DEFAULT_MAX_CHUNKS_PER_INDEX: usize = 200_000;

/// Read the per-index chunk cap from the environment, with a sane default.
///
/// Why: limits RSS growth on large monorepos.
/// What: reads `TRUSTY_MAX_CHUNKS` as a positive usize; falls back to
/// [`DEFAULT_MAX_CHUNKS_PER_INDEX`] when unset, zero, or unparseable.
/// Test: covered indirectly by every ingest test.
pub(crate) fn max_chunks_per_index() -> usize {
    std::env::var("TRUSTY_MAX_CHUNKS")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|&n: &usize| n > 0)
        .unwrap_or(DEFAULT_MAX_CHUNKS_PER_INDEX)
}

/// Default safety-net batch size when `TRUSTY_MAX_BATCH_SIZE` is unset.
const DEFAULT_EMBED_BATCH_SIZE: usize = 64;
/// Floor for env-clamped batch size.
const EMBED_BATCH_MIN: usize = 32;
/// Ceiling for env-clamped batch size.
const EMBED_BATCH_MAX: usize = 512;

/// Read the embedding batch size from `TRUSTY_MAX_BATCH_SIZE`, clamped to
/// `[EMBED_BATCH_MIN, EMBED_BATCH_MAX]`. Falls back to
/// `DEFAULT_EMBED_BATCH_SIZE` when unset or unparseable.
///
/// Why: large repos can exhaust process memory if batches grow unbounded.
/// What: parses env, clamps via `.clamp()`.
/// Test: see `tests::test_embed_batch_size_env_clamp`.
pub(crate) fn embed_batch_size() -> usize {
    std::env::var("TRUSTY_MAX_BATCH_SIZE")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|&n| n > 0)
        .map(|n| n.clamp(EMBED_BATCH_MIN, EMBED_BATCH_MAX))
        .unwrap_or(DEFAULT_EMBED_BATCH_SIZE)
}

// ─── Codec helpers ───────────────────────────────────────────────────────────

/// Stable u64 hash of a query string. Used as the LRU cache key so we don't
/// retain the full string twice (LRU stores the embedding payload only).
///
/// Why: avoids keeping two copies of the query text in the cache.
/// What: `DefaultHasher::finish()` over `query`.
/// Test: covered indirectly by every search that hits the embedding cache.
pub(crate) fn hash_query(query: &str) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    query.hash(&mut h);
    h.finish()
}

/// Build a 7-line snippet centered on the chunk content for token-efficient
/// output.
///
/// Why: long chunks are expensive in LLM prompts; a 7-line header gives enough
/// context to identify the construct without burning tokens.
/// What: returns the first 7 lines when content exceeds 7 lines; otherwise
/// returns `content` verbatim.
/// Test: covered indirectly by every search test that sets `compact: true`.
pub(crate) fn build_compact_snippet(content: &str) -> String {
    let lines: Vec<&str> = content.lines().collect();
    if lines.len() <= 7 {
        return content.to_string();
    }
    lines[..7].join("\n")
}

/// Resolve a stored chunk `file` string to an absolute path string.
///
/// Why (issue #402): newly indexed chunks store `file` relative to
/// `root_path`. Older indexes still carry absolute paths. This helper
/// normalises both forms.
/// What: if `raw_file` starts with the OS path separator it is returned
/// as-is; otherwise `root_path.join(raw_file)` is returned.
/// Test: `tests::resolve_chunk_file_relative_becomes_absolute` and
///       `tests::resolve_chunk_file_absolute_passthrough`.
pub(crate) fn resolve_chunk_file(raw_file: &str, root_path: &std::path::Path) -> String {
    if std::path::Path::new(raw_file).is_absolute() {
        raw_file.to_string()
    } else {
        root_path.join(raw_file).to_string_lossy().into_owned()
    }
}

/// Materialize a `RawChunk` into a `CodeChunk` with the given score, match
/// reason, and optional compact snippet.
///
/// Why: four call sites used to inline the same 18-field struct literal.
/// Consolidating removes ~60 lines of duplication.
/// What: clones every metadata field and derives `chunk_depth` (clamped to
/// u8). Resolves `raw.file` to absolute via [`resolve_chunk_file`].
/// Test: covered indirectly by every search/materialization test.
pub(crate) fn raw_to_code_chunk(
    raw: &RawChunk,
    score: f32,
    match_reason: &str,
    compact_snippet: Option<String>,
    root_path: &std::path::Path,
) -> CodeChunk {
    let chunk_depth: u8 = raw.chunk_depth.min(u8::MAX as usize) as u8;
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
/// Why: two call sites used the same dedupe-by-entity-text loop. Extracting
/// prevents drift.
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

// ─── Score helpers ───────────────────────────────────────────────────────────

/// Score multiplier applied to a chunk for Definition-intent queries (issue
/// #92).
///
/// Why: Definition queries should surface the canonical declaration, not doc
/// files that mention the symbol many times.
/// What: returns `0.5` for known doc/config extensions, `1.0` otherwise.
/// Test: covered by `test_file_type_multiplier_demotes_docs`.
pub(crate) fn file_type_score_multiplier(path: &str) -> f32 {
    const DOC_EXTENSIONS: &[&str] = &[".md", ".txt", ".toml", ".yaml", ".yml", ".json"];
    let lower = path.to_ascii_lowercase();
    if DOC_EXTENSIONS.iter().any(|ext| lower.ends_with(ext)) {
        0.5
    } else {
        1.0
    }
}

/// Structural-definition score boost for Definition-intent queries (issue
/// #117).
///
/// Why: queries with struct-name tokens were under-firing; a 2.0× multiplier
/// surfaces the canonical declaration without drowning other boosts.
/// What: a flat `2.0` multiplier applied in `apply_score_adjustments`.
/// Test: `test_struct_definition_boost_surfaces_struct_over_usage`.
pub(crate) const STRUCT_DEFINITION_BOOST: f32 = 2.0;

/// Decide whether `chunk_type` participates in the Definition-intent
/// structural boost for type declarations (issue #117).
///
/// Why: only chunks that ARE the declaration of a type are eligible.
/// What: returns `true` for `Struct`, `Enum`, `Class`, `Trait`, and
/// `TypeAlias`; `false` for everything else.
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
/// Why: function-name queries returned usage sites at rank 1 instead of
/// the canonical declaration. Extending the boost to function-like chunks
/// closes that gap.
/// What: returns `true` for `Function` and `Method`; `false` for everything
/// else. `Constant` is excluded to avoid boosting string-literal occurrences.
/// Test: covered by
/// `test_function_definition_boost_surfaces_function_over_string_literal_usage`.
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
/// lowercases each remaining token.
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
/// Why: lifted out of `search` to keep the materialization loop short and to
/// make the precedence rules unit-testable in isolation.
/// What: direct hits (HNSW and/or BM25) take precedence over KG-only paths.
/// `(false,false,false)` returns `"fallback:ripgrep"` for the grep lane.
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
