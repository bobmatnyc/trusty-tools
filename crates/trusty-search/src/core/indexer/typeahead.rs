//! Typeahead (autocomplete) types returned by the `/typeahead` endpoint and
//! the `typeahead` MCP tool.
//!
//! Why: the search pipeline already exposes `CodeChunk` (full result) and
//! `SearchQuery`/`SearchStage` (query builder). Typeahead is a lightweight
//! projection of those results — 6 hits maximum, compact snippets — intended
//! for per-keystroke autocomplete. Keeping the projection type here rather
//! than in the HTTP layer lets both the HTTP handler and the MCP tool share
//! the same well-tested DTO.
//! What: `TypeaheadHit` is a slimmed view of `CodeChunk` tagged with which
//! retrieval lane produced it (`lexical`, `semantic`, or `kg`).
//! `TypeaheadResponse` is the full response envelope.
//! Test: `typeahead` integration test in `crates/trusty-search/tests/typeahead.rs`.

use serde::{Deserialize, Serialize};

use super::types::CodeChunk;

/// Mode selector for the typeahead endpoint.
///
/// Why: lexical-only is safe for per-keystroke use (no ONNX call, <30 ms);
/// blended adds semantic + KG for richer suggestions when the caller can
/// afford the latency (intended for debounced invocation).
/// What: `Lexical` drives `SearchStage::Lexical`; `Blended` drives
/// `SearchStage::Semantic` with `expand_graph = true`.
/// Test: `typeahead_blended_mode_returns_results_tagged_by_source` (ignored,
/// needs ONNX).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum TypeaheadMode {
    /// BM25 + grep-fallback only. Always available. Default mode.
    #[default]
    Lexical,
    /// Full hybrid (BM25 + HNSW + KG expansion). Intended for debounced use.
    Blended,
}

/// A single typeahead suggestion.
///
/// Why: `CodeChunk` is too heavy for autocomplete — it carries full source
/// content and many metadata fields. `TypeaheadHit` carries only the fields
/// a completion UI needs, keeping the response small and fast to serialise.
/// What: `label` is the display string (function/symbol name when available,
/// file basename otherwise). `path` and `start_line` let the UI deep-link to
/// the exact location. `score` enables client-side reranking if needed.
/// `source` tags which retrieval lane produced this hit so clients can style
/// results differently (e.g. highlight semantic matches vs lexical).
/// `snippet` is the 7-line compact excerpt from `compact_snippet`.
/// Test: `typeahead_returns_expected_fields` in `tests/typeahead.rs`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TypeaheadHit {
    /// Display label: function/symbol name when available, else file basename.
    pub label: String,
    /// Root-relative path of the source file.
    pub path: String,
    /// 1-based start line of the matching chunk.
    pub start_line: usize,
    /// Fused RRF score from the search pipeline.
    pub score: f32,
    /// Which retrieval lane produced this hit: `"lexical"`, `"semantic"`, or
    /// `"kg"`.
    pub source: String,
    /// 7-line compact snippet from `compact_snippet`, or the first 200 chars
    /// of `content` when `compact_snippet` is absent.
    pub snippet: String,
}

impl TypeaheadHit {
    /// Classify the retrieval source from a `CodeChunk`'s `match_reason`.
    ///
    /// Why: the search pipeline records how each chunk was found in
    /// `match_reason` (`"hybrid"`, `"hybrid+kg"`, `"bm25"`, `"vector"`,
    /// `"fallback:ripgrep"`). For blended-mode typeahead we need to translate
    /// that into the three-valued `source` tag (`"lexical"`, `"semantic"`,
    /// `"kg"`) that clients consume.
    /// What: `"hybrid+kg"` → `"kg"`, `"vector"` → `"semantic"`,
    /// everything else (`"hybrid"`, `"bm25"`, `"fallback:ripgrep"`) → `"lexical"`.
    /// Test: `classify_source_*` unit tests below.
    pub fn classify_source(match_reason: &str) -> &'static str {
        if match_reason.contains("kg") {
            "kg"
        } else if match_reason == "vector" {
            "semantic"
        } else {
            "lexical"
        }
    }

    /// Map a `CodeChunk` into a `TypeaheadHit`, overriding `source` with the
    /// supplied value.
    ///
    /// Why: the HTTP handler and MCP tool both convert `CodeChunk` → `TypeaheadHit`;
    /// centralising the mapping here avoids duplicate field extraction logic.
    /// What: derives `label` from `function_name` (preferred) or the file
    /// basename; derives `path` from the stored `path` field (root-relative,
    /// issue #674) or falls back to `file`; truncates `snippet` from
    /// `compact_snippet` or `content`.
    /// Test: `typeahead_returns_expected_fields` in `tests/typeahead.rs`.
    pub fn from_chunk(chunk: &CodeChunk, source: &str) -> Self {
        let label = chunk
            .function_name
            .as_deref()
            .filter(|n| !n.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| {
                // Fall back to the file basename.
                let file_path = chunk.path.as_deref().unwrap_or(chunk.file.as_str());
                std::path::Path::new(file_path)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or(file_path)
                    .to_owned()
            });

        let path = chunk.path.clone().unwrap_or_else(|| chunk.file.clone());

        let snippet = chunk
            .compact_snippet
            .clone()
            .unwrap_or_else(|| chunk.content.chars().take(200).collect());

        TypeaheadHit {
            label,
            path,
            start_line: chunk.start_line,
            score: chunk.score,
            source: source.to_owned(),
            snippet,
        }
    }
}

/// The full response envelope for the typeahead endpoint.
///
/// Why: wraps the hits list with metadata (mode used, latency) so clients
/// can display source-lane badges and monitor performance without a second
/// call.
/// What: `hits` is ordered by descending `score`. `mode` echoes back the
/// effective `TypeaheadMode` so clients that omit the parameter know which
/// lane ran. `latency_ms` is the server-side handler duration.
/// Test: `typeahead_returns_hits` in `tests/typeahead.rs`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TypeaheadResponse {
    /// Ordered (descending score) list of suggestions. At most `limit` entries.
    pub hits: Vec<TypeaheadHit>,
    /// The effective mode used (`"lexical"` or `"blended"`).
    pub mode: String,
    /// Server-side handler latency in milliseconds.
    pub latency_ms: u64,
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_source_kg_hit_tagged_kg() {
        assert_eq!(TypeaheadHit::classify_source("hybrid+kg"), "kg");
    }

    #[test]
    fn classify_source_vector_hit_tagged_semantic() {
        assert_eq!(TypeaheadHit::classify_source("vector"), "semantic");
    }

    #[test]
    fn classify_source_bm25_hit_tagged_lexical() {
        assert_eq!(TypeaheadHit::classify_source("bm25"), "lexical");
        assert_eq!(TypeaheadHit::classify_source("hybrid"), "lexical");
        assert_eq!(TypeaheadHit::classify_source("fallback:ripgrep"), "lexical");
    }

    #[test]
    fn from_chunk_prefers_function_name_as_label() {
        use crate::core::chunker::ChunkType;
        let chunk = CodeChunk {
            id: "src/auth.rs:10:20".to_string(),
            file: "src/auth.rs".to_string(),
            path: Some("src/auth.rs".to_string()),
            language: None,
            start_line: 10,
            end_line: 20,
            content: "fn authenticate() {}".to_string(),
            function_name: Some("authenticate".to_string()),
            score: 0.5,
            compact_snippet: Some("fn authenticate() {}".to_string()),
            match_reason: "bm25".to_string(),
            chunk_type: ChunkType::Function,
            calls: vec![],
            inherits_from: vec![],
            chunk_depth: 0,
            index_id: None,
            on_branch: false,
            archive_reason: None,
        };
        let hit = TypeaheadHit::from_chunk(&chunk, "lexical");
        assert_eq!(hit.label, "authenticate");
        assert_eq!(hit.source, "lexical");
        assert_eq!(hit.start_line, 10);
    }

    #[test]
    fn from_chunk_falls_back_to_basename_when_no_function_name() {
        use crate::core::chunker::ChunkType;
        let chunk = CodeChunk {
            id: "src/auth.rs:1:5".to_string(),
            file: "src/auth.rs".to_string(),
            path: Some("src/auth.rs".to_string()),
            language: None,
            start_line: 1,
            end_line: 5,
            content: "use std::io;".to_string(),
            function_name: None,
            score: 0.1,
            compact_snippet: None,
            match_reason: "bm25".to_string(),
            chunk_type: ChunkType::Module,
            calls: vec![],
            inherits_from: vec![],
            chunk_depth: 0,
            index_id: None,
            on_branch: false,
            archive_reason: None,
        };
        let hit = TypeaheadHit::from_chunk(&chunk, "lexical");
        assert_eq!(hit.label, "auth.rs");
        // snippet falls back to first 200 chars of content
        assert_eq!(hit.snippet, "use std::io;");
    }
}
