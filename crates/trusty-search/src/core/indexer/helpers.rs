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

use std::time::Duration;

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
/// in-memory `chunks` HashMap, BM25 corpus, and per-file entity map are
/// evicted to reclaim heap.
///
/// Why (issue #2166): a production daemon (77 indexes) held ~13 GB resident
/// against ~3.9 GB of on-disk data. The original 300s window meant an index
/// only had to go 5 minutes without a query or ingest to become eligible for
/// eviction, but most idle indexes on a multi-project host sit idle far
/// longer than that between bursts of activity — so 300s rarely fired in
/// practice while the daemon accumulated dozens of permanently-hot,
/// rarely-queried indexes. Lowering the default to 60s (matched to the
/// ticker's own 60s cadence — see
/// `service::server::tickers::spawn_idle_chunk_eviction_ticker`) means an
/// index that goes even one tick without activity gets reclaimed, trading a
/// small amount of rehydration latency on the next query (a redb read,
/// typically well under 100ms) for materially lower steady-state RSS on
/// hosts tracking many projects. Still fully overridable per-deployment.
///
/// Correction (issue #3683 slice 1): "typically well under 100ms" held for
/// the small indexes this default was tuned against, but was off by roughly
/// three orders of magnitude for a 315K-chunk index on NFS-backed storage
/// (27-40s/scan), where this aggressive window turned nearly every query into
/// a cold start. Slice 1 fixed what used to happen on a slow rehydrate (a
/// synchronous, discard-on-cancel scan inline in the query path — see
/// `idle_evict::ensure_corpus_rehydrated`) so a slow scan degrades a query
/// instead of livelocking the whole index, but deliberately left this
/// constant and the eviction cadence untouched.
///
/// Retuning (issue #3683 slice 2): 60s matched hosts of small, cheap indexes
/// but meant a large-corpus index went cold roughly once a minute even under
/// light interactive traffic. Raised to 300s (5 min, the pre-#2166 value) as
/// a flat FLOOR, now additionally scaled per-index by measured/estimated
/// rehydrate cost — see [`scaled_idle_evict_threshold`] and
/// `CodeIndexer::cost_scaled_idle_threshold` in `idle_evict.rs`. A cheap
/// index (sub-millisecond rehydrate) still idle-evicts at roughly this flat
/// floor; an expensive one (the i-0076 315K-chunk / 27-40s-scan corpus) earns
/// proportionally more idle time before eviction, directly addressing the
/// thrash-eviction root cause in the #3683 production incident.
pub(crate) const DEFAULT_CHUNKS_IDLE_EVICT_SECS: u64 = 300;

/// Resolve the in-memory-chunks / BM25 / entities idle-eviction window (in
/// seconds) from the environment, falling back to
/// [`DEFAULT_CHUNKS_IDLE_EVICT_SECS`].
///
/// Why: operators on memory-constrained hosts may want a tighter window
/// (evict sooner) while large-corpus hosts that re-query frequently may want
/// to disable eviction entirely. Issue #2162 extended this single window to
/// also gate BM25 corpus + per-file entity eviction, so both structures ride
/// the same operator-tunable knob instead of adding a second env var.
/// What: reads `TRUSTY_CHUNKS_IDLE_EVICT_SECS` as `u64` seconds. A value of
/// `0` **disables** idle eviction (chunks, BM25, and entities all stay hot).
/// Unset / unparseable falls back to default.
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

// ─── Cost-scaled idle-eviction window (issue #3683 slice 2) ────────────────

/// On-disk chunks a rehydrate scan is estimated to process per millisecond,
/// used only as a same-process fallback before an index has ever actually
/// been rehydrated (see `CodeIndexer::rehydrate_cost_estimate_ms`).
///
/// Why: the #3683 production RCA measured 27-40s cold rehydrate scans for a
/// 315,423-chunk NFS-backed corpus — roughly 0.086-0.127 ms/chunk. `10`
/// (i.e. 0.1 ms/chunk, ⇒ ~31.5s for that corpus) sits inside that measured
/// band. Once a real scan completes, its MEASURED duration always takes
/// precedence over this estimate.
const ESTIMATED_CHUNKS_PER_MS: u64 = 10;

/// Estimate rehydrate cost (milliseconds) for a corpus of `chunk_count`
/// chunks, calibrated against the #3683 production incident (see
/// [`ESTIMATED_CHUNKS_PER_MS`]).
///
/// Why: an index that has never rehydrated in this process's lifetime has no
/// measured cost yet; a cheap, redb-metadata-only chunk count
/// (`CorpusStore::chunk_count`) is the best same-process signal available
/// without forcing a scan just to estimate one.
/// What: integer division — a corpus small enough to divide to zero
/// estimates zero extra cost (falls back to the flat base window in
/// [`scaled_idle_evict_threshold`]).
/// Test: `estimate_rehydrate_cost_ms_matches_production_incident_band`.
pub(crate) fn estimate_rehydrate_cost_ms(chunk_count: u64) -> u64 {
    chunk_count / ESTIMATED_CHUNKS_PER_MS
}

/// Default milliseconds of rehydrate cost that earn one additional multiple
/// of the base idle-eviction window (issue #3683 slice 2). Override via
/// `TRUSTY_REHYDRATE_COST_SCALE_UNIT_MS`; `0` disables cost-scaling entirely
/// (every index uses the flat base window, i.e. pre-slice-2 behaviour modulo
/// the raised [`DEFAULT_CHUNKS_IDLE_EVICT_SECS`]).
const DEFAULT_REHYDRATE_COST_SCALE_UNIT_MS: u64 = 1_000;

/// Ceiling on the cost-scaled idle-eviction window: 6 hours.
///
/// Why: without a cap, a pathologically large corpus could compute a window
/// of many days, effectively disabling idle-eviction for it entirely. The
/// memory-pressure sweep (`CodeIndexer::reclaim_memory_now`, issue #2846)
/// remains the backstop for genuine memory pressure regardless of this cap.
const MAX_SCALED_IDLE_EVICT_SECS: u64 = 6 * 60 * 60;

/// Resolve the rehydrate-cost scaling unit (milliseconds) from
/// `TRUSTY_REHYDRATE_COST_SCALE_UNIT_MS`, falling back to
/// [`DEFAULT_REHYDRATE_COST_SCALE_UNIT_MS`] when unset or unparseable. `0` is
/// returned verbatim — [`scaled_idle_evict_threshold`] treats it as "disable
/// scaling", matching the `TRUSTY_CHUNKS_IDLE_EVICT_SECS=0` "disable"
/// precedent.
fn rehydrate_cost_scale_unit_ms() -> u64 {
    std::env::var("TRUSTY_REHYDRATE_COST_SCALE_UNIT_MS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_REHYDRATE_COST_SCALE_UNIT_MS)
}

/// Scale `base_secs` by measured/estimated rehydrate cost (issue #3683 slice
/// 2 — the #3683 RCA's Defect 2 retuning: "an index whose rehydrate is
/// expensive should idle far longer before eviction than a cheap one").
///
/// Why: a flat idle window (previously 60s) treated a 315K-chunk NFS-backed
/// index (27-40s rehydrate) identically to a handful-of-chunks index
/// (sub-millisecond rehydrate) — thrash-evicting the expensive one on every
/// idle tick and turning nearly every query into a cold start (the #3683
/// production incident). Scaling proportionally to cost means the sweep only
/// thrashes indexes it can cheaply afford to re-warm.
/// What: `base_secs == 0` (eviction disabled) always returns
/// [`Duration::ZERO`] regardless of cost — scaling a disabled window is
/// meaningless. Otherwise computes
/// `base_secs * (1 + rehydrate_cost_ms / scale_unit_ms)`, capped at
/// [`MAX_SCALED_IDLE_EVICT_SECS`]. A `scale_unit_ms` of `0`
/// (`TRUSTY_REHYDRATE_COST_SCALE_UNIT_MS=0`) disables scaling — every index
/// gets exactly `base_secs`, regardless of cost.
/// Test: `scaled_idle_evict_threshold_disabled_when_base_is_zero`,
/// `scaled_idle_evict_threshold_env_override_and_scaling`.
pub(crate) fn scaled_idle_evict_threshold(base_secs: u64, rehydrate_cost_ms: u64) -> Duration {
    if base_secs == 0 {
        return Duration::ZERO;
    }
    let scale_unit = rehydrate_cost_scale_unit_ms();
    let multiples = rehydrate_cost_ms
        .checked_div(scale_unit)
        .unwrap_or(0)
        .saturating_add(1);
    let scaled = base_secs
        .saturating_mul(multiples)
        .min(MAX_SCALED_IDLE_EVICT_SECS);
    Duration::from_secs(scaled)
}

/// Default idle window (seconds) after which a live index's FSEvents watcher is
/// suspended to stop burning CPU / `fseventsd` load on a project nobody is using.
///
/// Why: 900s (15 min) sits well above the 60s chunk/BM25/entity-eviction
/// window ([`DEFAULT_CHUNKS_IDLE_EVICT_SECS`]) so the escalation is gradual — a briefly
/// idle index keeps its watcher (cheap incremental indexing on the next save),
/// and only a genuinely-dormant one drops it. The watcher resumes on the next
/// query, so suspension is invisible to an active user.
pub(crate) const DEFAULT_WATCH_IDLE_SUSPEND_SECS: u64 = 900;

/// Resolve the watcher idle-suspend window (in seconds) from the environment,
/// falling back to [`DEFAULT_WATCH_IDLE_SUSPEND_SECS`].
///
/// Why: the FSEvents watch is the CPU/`fseventsd` cost of a registered index;
/// operators watching hundreds of projects want to release idle watches, while
/// those on a single active repo may want to disable suspension entirely.
/// What: reads `TRUSTY_WATCH_IDLE_SUSPEND_SECS` as `u64` seconds. A value of `0`
/// **disables** suspension (watchers stay hot). Unset / unparseable falls back
/// to the default.
/// Test: `watch_idle_suspend_secs_default_and_env_override`.
pub(crate) fn watch_idle_suspend_secs() -> u64 {
    match std::env::var("TRUSTY_WATCH_IDLE_SUSPEND_SECS") {
        Ok(v) if !v.is_empty() => match v.parse::<u64>() {
            Ok(n) => n,
            Err(_) => {
                tracing::warn!(
                    "indexer: TRUSTY_WATCH_IDLE_SUSPEND_SECS={v:?} is not a valid u64; \
                     using default ({DEFAULT_WATCH_IDLE_SUSPEND_SECS}s)"
                );
                DEFAULT_WATCH_IDLE_SUSPEND_SECS
            }
        },
        _ => DEFAULT_WATCH_IDLE_SUSPEND_SECS,
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

/// Retrieval lane that surfaced a search result, rendered verbatim into
/// `CodeChunk.match_reason`.
///
/// Why: the five `match_reason` labels are a documented external MCP/HTTP API
/// contract (they appear verbatim in `search` / `search_similar` JSON). A
/// typed enum makes the producer exhaustive and self-documenting while
/// [`MatchReason::as_str`] pins the wire strings byte-for-byte, so a stray
/// literal can no longer drift out of the contract (issue #2695). The
/// `CodeChunk.match_reason` field stays `String` on purpose — see
/// [`compute_match_reason`] for the boundary rationale.
/// What: one variant per documented lane; [`MatchReason::as_str`] and the
/// [`std::fmt::Display`] impl both emit exactly `"vector"`, `"bm25"`,
/// `"hybrid"`, `"hybrid+kg"`, or `"fallback:ripgrep"`.
/// Test: `match_reason_labels_are_byte_identical` (helpers unit tests) pins
/// every rendered string; `test_compute_match_reason_fallback_label` pins the
/// producer arms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MatchReason {
    /// HNSW (semantic) lane only.
    Vector,
    /// BM25 (lexical) lane only.
    Bm25,
    /// Both HNSW and BM25 fused.
    Hybrid,
    /// Knowledge-graph neighbour expansion (no direct HNSW/BM25 hit).
    HybridKg,
    /// Exact-substring ripgrep fallback when both primary lanes were empty.
    FallbackRipgrep,
}

impl MatchReason {
    /// Render the variant as its documented wire label.
    ///
    /// Why: `CodeChunk.match_reason` is a `String` on the wire; this is the
    /// single point that maps the typed variant to its byte-identical label,
    /// keeping the external contract in one place.
    /// What: returns the `'static` string for each variant.
    /// Test: `match_reason_labels_are_byte_identical`.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            MatchReason::Vector => "vector",
            MatchReason::Bm25 => "bm25",
            MatchReason::Hybrid => "hybrid",
            MatchReason::HybridKg => "hybrid+kg",
            MatchReason::FallbackRipgrep => "fallback:ripgrep",
        }
    }
}

impl std::fmt::Display for MatchReason {
    /// Why: lets the enum flow through `format!`/`write!` and `.to_string()`
    /// without callers reaching for [`MatchReason::as_str`] explicitly.
    /// What: delegates to [`MatchReason::as_str`] so `Display` and `as_str`
    /// can never disagree.
    /// Test: `match_reason_labels_are_byte_identical`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Map (`in_hnsw`, `in_bm25`, `in_kg`) booleans to the [`MatchReason`] lane
/// that surfaced the result.
///
/// Why: lifted out of `search` to keep the materialization loop short and to
/// make the precedence rules unit-testable in isolation. The result is a
/// typed [`MatchReason`] rather than a bare label so the producer is
/// exhaustive; the caller renders it with [`MatchReason::as_str`] at the
/// single assignment site.
///
/// Boundary note (issue #2695): `CodeChunk.match_reason` stays `String`
/// rather than becoming `MatchReason`. The field is serialized across the
/// MCP/HTTP wire and legitimately holds values outside these five lanes
/// (e.g. the empty placeholder in `output.rs`, `"test"` fixtures, benchmark
/// `"grep"` rows), and its downstream consumer `classify_source` does
/// substring matching. Typing only the producer keeps the wire byte-identical
/// with the smallest blast radius.
/// What: direct hits (HNSW and/or BM25) take precedence over KG-only paths.
/// `(false,false,false)` returns [`MatchReason::FallbackRipgrep`] for the grep
/// lane.
/// Test: covered indirectly by `test_kg_expansion_marks_neighbours_with_hybrid_kg`
/// and `test_compute_match_reason_fallback_label`.
pub(crate) fn compute_match_reason(in_v: bool, in_b: bool, in_kg: bool) -> MatchReason {
    match (in_v, in_b, in_kg) {
        (true, true, _) => MatchReason::Hybrid,
        (true, false, _) => MatchReason::Vector,
        (false, true, _) => MatchReason::Bm25,
        (false, false, true) => MatchReason::HybridKg,
        (false, false, false) => MatchReason::FallbackRipgrep,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pin every [`MatchReason`] label to its documented wire string,
    /// byte-for-byte, via both `as_str()` and `Display` (issue #2695).
    ///
    /// These five strings are an external MCP/HTTP API contract; any change
    /// here is a breaking wire change and must fail this test loudly.
    #[test]
    fn match_reason_labels_are_byte_identical() {
        let cases = [
            (MatchReason::Vector, "vector"),
            (MatchReason::Bm25, "bm25"),
            (MatchReason::Hybrid, "hybrid"),
            (MatchReason::HybridKg, "hybrid+kg"),
            (MatchReason::FallbackRipgrep, "fallback:ripgrep"),
        ];
        for (variant, expected) in cases {
            assert_eq!(variant.as_str(), expected, "as_str drifted for {variant:?}");
            assert_eq!(
                variant.to_string(),
                expected,
                "Display drifted for {variant:?}"
            );
        }
    }

    /// Watcher idle-suspend: `watch_idle_suspend_secs` honours the default and
    /// the `TRUSTY_WATCH_IDLE_SUSPEND_SECS` override, including `0` (disabled)
    /// and an unparseable value (falls back to default).
    #[test]
    fn watch_idle_suspend_secs_default_and_env_override() {
        let prior = std::env::var("TRUSTY_WATCH_IDLE_SUSPEND_SECS").ok();

        // Unset → default.
        // SAFETY: this test is the only reader/writer of this env var.
        unsafe { std::env::remove_var("TRUSTY_WATCH_IDLE_SUSPEND_SECS") };
        assert_eq!(watch_idle_suspend_secs(), DEFAULT_WATCH_IDLE_SUSPEND_SECS);

        // Valid override wins.
        // SAFETY: see above.
        unsafe { std::env::set_var("TRUSTY_WATCH_IDLE_SUSPEND_SECS", "120") };
        assert_eq!(watch_idle_suspend_secs(), 120);

        // Zero disables (returned verbatim; the ticker treats 0 as "off").
        // SAFETY: see above.
        unsafe { std::env::set_var("TRUSTY_WATCH_IDLE_SUSPEND_SECS", "0") };
        assert_eq!(watch_idle_suspend_secs(), 0);

        // Garbage falls back to default (with a warn).
        // SAFETY: see above.
        unsafe { std::env::set_var("TRUSTY_WATCH_IDLE_SUSPEND_SECS", "nope") };
        assert_eq!(watch_idle_suspend_secs(), DEFAULT_WATCH_IDLE_SUSPEND_SECS);

        // Restore.
        // SAFETY: see above.
        unsafe {
            match prior {
                Some(v) => std::env::set_var("TRUSTY_WATCH_IDLE_SUSPEND_SECS", v),
                None => std::env::remove_var("TRUSTY_WATCH_IDLE_SUSPEND_SECS"),
            }
        }
    }

    /// Sanity-pins [`ESTIMATED_CHUNKS_PER_MS`] against the #3683 production
    /// incident's own measured band (315,423 chunks, 27-40s cold scans), so a
    /// future edit to the calibration constant fails loudly if it drifts
    /// outside the band it was chosen to match.
    #[test]
    fn estimate_rehydrate_cost_ms_matches_production_incident_band() {
        let est = estimate_rehydrate_cost_ms(315_423);
        assert!(
            (27_000..=40_000).contains(&est),
            "estimate {est}ms falls outside the #3683 measured 27-40s band"
        );
    }

    /// A disabled idle-eviction window (`base_secs == 0`) must never scale up
    /// regardless of cost — scaling a disabled window is meaningless, and
    /// this path must never even read the scaling env var.
    #[test]
    fn scaled_idle_evict_threshold_disabled_when_base_is_zero() {
        assert_eq!(
            scaled_idle_evict_threshold(0, 1_000_000),
            Duration::ZERO,
            "base_secs=0 must stay disabled no matter the rehydrate cost"
        );
    }

    /// `scaled_idle_evict_threshold` honours the default scaling unit, the
    /// `TRUSTY_REHYDRATE_COST_SCALE_UNIT_MS` override (including `0` =
    /// disabled), and caps at [`MAX_SCALED_IDLE_EVICT_SECS`] — all in one
    /// test function. `#[serial_test::serial]` (this env var's only other
    /// reader is `indexer::tests_idle_evict::cost_scaled_idle_threshold_scales_with_rehydrate_cost`,
    /// also tagged serial) avoids the cross-test env-mutation race class from
    /// #3629.
    #[test]
    #[serial_test::serial]
    fn scaled_idle_evict_threshold_env_override_and_scaling() {
        let prior = std::env::var("TRUSTY_REHYDRATE_COST_SCALE_UNIT_MS").ok();

        // Default scale unit (1000ms/multiple): a cheap index (no cost) gets
        // exactly the base window...
        // SAFETY: this test is the only reader/writer of this env var.
        unsafe { std::env::remove_var("TRUSTY_REHYDRATE_COST_SCALE_UNIT_MS") };
        assert_eq!(
            scaled_idle_evict_threshold(300, 0),
            Duration::from_secs(300),
            "zero rehydrate cost must keep the flat base window"
        );
        // ...while an expensive index (30s measured/estimated cost, in the
        // #3683 incident's ballpark) idles far longer before eviction.
        let costly = scaled_idle_evict_threshold(300, 30_000);
        assert_eq!(
            costly,
            Duration::from_secs(300 * 31),
            "30s of cost should earn 30 extra base-window multiples at the default 1000ms unit"
        );
        assert!(
            costly > Duration::from_secs(300 * 10),
            "an expensive index must idle MUCH longer than a cheap one, not just marginally"
        );

        // A coarser explicit override changes the earn rate.
        // SAFETY: see above.
        unsafe { std::env::set_var("TRUSTY_REHYDRATE_COST_SCALE_UNIT_MS", "10000") };
        assert_eq!(
            scaled_idle_evict_threshold(300, 30_000),
            Duration::from_secs(300 * 4),
            "30s of cost / 10s-per-multiple unit == 3 extra multiples (+1 base) == 4x"
        );

        // 0 disables scaling outright: every index gets the flat base
        // window regardless of cost.
        // SAFETY: see above.
        unsafe { std::env::set_var("TRUSTY_REHYDRATE_COST_SCALE_UNIT_MS", "0") };
        assert_eq!(
            scaled_idle_evict_threshold(300, 30_000),
            Duration::from_secs(300),
            "scale unit 0 must disable scaling entirely"
        );

        // A pathologically large cost must cap at MAX_SCALED_IDLE_EVICT_SECS
        // (6h) rather than growing unbounded.
        // SAFETY: see above.
        unsafe { std::env::set_var("TRUSTY_REHYDRATE_COST_SCALE_UNIT_MS", "1") };
        assert_eq!(
            scaled_idle_evict_threshold(300, 10_000_000),
            Duration::from_secs(MAX_SCALED_IDLE_EVICT_SECS),
            "an extreme cost must cap the scaled window rather than grow unbounded"
        );

        // Restore.
        // SAFETY: see above.
        unsafe {
            match prior {
                Some(v) => std::env::set_var("TRUSTY_REHYDRATE_COST_SCALE_UNIT_MS", v),
                None => std::env::remove_var("TRUSTY_REHYDRATE_COST_SCALE_UNIT_MS"),
            }
        }
    }
}
