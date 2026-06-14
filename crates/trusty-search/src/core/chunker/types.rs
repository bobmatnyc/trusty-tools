//! Core types: `ChunkType`, `RawChunk`, and the sliding-window `chunk_text` fallback.
//!
//! Why: these types are the shared currency of every chunker sub-module.
//! Keeping them isolated prevents circular imports and makes the public API
//! surface obvious.
//!
//! What: defines `ChunkType` (semantic category of a chunk), `RawChunk`
//! (the full chunk record), and `chunk_text` (overlapping sliding-window
//! fallback for unknown extensions and sub-chunking oversized AST chunks).
//!
//! Test: `test_overlapping_chunks` and `test_chunk_id_format` in
//! `core/chunker/tests.rs`.

use serde::{Deserialize, Serialize};

/// Semantic category assigned to each extracted chunk.
///
/// Why: query-type routing and the symbol graph use chunk types to distinguish
/// definitions from usages, and to apply type-specific scoring boosts.
/// What: one variant per recognized declaration kind; `Unknown` is the
/// serde-deserialization default so older index versions round-trip cleanly.
/// Test: every language-specific test in `core/chunker/tests.rs` verifies that
/// the correct `ChunkType` is assigned.
///
/// `Default` is `Unknown` so chunks deserialized from older index versions
/// (which lacked `chunk_type`) round-trip cleanly.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum ChunkType {
    #[default]
    Unknown,
    Function,
    Method,
    Class,
    Struct,
    Impl,
    Module,
    Trait,
    Enum,
    Test,
    Constant,
    TypeAlias,
    Docstring,
    /// Free-form code that doesn't fit a more specific category.
    FreeCode,
    /// Legacy alias for `FreeCode` — retained for backwards-compatible deserialization.
    Code,
}

impl ChunkType {
    /// Return a stable string label for use in chunk IDs.
    ///
    /// Why: chunk IDs embed the type name so two chunks from the same file and
    /// line-range but with different types are still distinct.
    /// What: a `match` over every variant returning a `&'static str`.
    /// Test: covered transitively by `document_chunk` and language tests.
    pub(super) fn as_str(&self) -> &'static str {
        match self {
            Self::Unknown => "Unknown",
            Self::Function => "Function",
            Self::Method => "Method",
            Self::Class => "Class",
            Self::Struct => "Struct",
            Self::Impl => "Impl",
            Self::Module => "Module",
            Self::Trait => "Trait",
            Self::Enum => "Enum",
            Self::Test => "Test",
            Self::Constant => "Constant",
            Self::TypeAlias => "TypeAlias",
            Self::Docstring => "Docstring",
            Self::FreeCode => "FreeCode",
            Self::Code => "Code",
        }
    }
}

/// A code chunk extracted from a source file.
///
/// Why: this struct is the unit of indexing for both BM25 and the HNSW vector
/// store; every field drives search, KG expansion, or UI display.
/// What: holds line-range, content, metadata (function name, language, chunk
/// type), and relational fields (calls, inherits, parent/child chunk IDs).
/// Test: every chunker test that calls `chunk_ast` asserts fields on returned
/// `RawChunk` values.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawChunk {
    pub id: String,
    pub file: String,
    pub start_line: usize,
    pub end_line: usize,
    pub content: String,
    pub function_name: Option<String>,
    pub language: Option<String>,

    // Issue #4 / #17 additions
    pub chunk_type: ChunkType,
    pub calls: Vec<String>,
    pub inherits_from: Vec<String>,
    pub chunk_depth: usize,
    pub parent_chunk_id: Option<String>,
    pub child_chunk_ids: Vec<String>,
    pub nlp_keywords: Vec<String>,
    pub nlp_code_refs: Vec<String>,

    /// Entity-derived virtual terms appended to this chunk's BM25 document
    /// at index time (issue #19). Not displayed to users; used only to give
    /// BM25 extra surface area to match symbolic queries against.
    #[serde(default)]
    pub virtual_terms: Vec<String>,
}

impl RawChunk {
    /// Build a generic `Code` chunk — used by `chunk_text` and the unknown-extension fallback.
    ///
    /// Why: several code paths (sliding-window fallback, empty-AST fallback)
    /// all need a minimal chunk with default-zero relational fields; this
    /// helper avoids repeating the struct literal.
    /// What: constructs a `RawChunk` with `chunk_type = Code`, empty calls /
    /// inherits / keywords, and the supplied id / file / line-range / content.
    /// Test: `test_overlapping_chunks` and `test_unknown_language_fallback`.
    pub(super) fn generic(
        id: String,
        file: String,
        start_line: usize,
        end_line: usize,
        content: String,
    ) -> Self {
        Self {
            id,
            file,
            start_line,
            end_line,
            content,
            function_name: None,
            language: None,
            chunk_type: ChunkType::Code,
            calls: Vec::new(),
            inherits_from: Vec::new(),
            chunk_depth: 0,
            parent_chunk_id: None,
            child_chunk_ids: Vec::new(),
            nlp_keywords: Vec::new(),
            nlp_code_refs: Vec::new(),
            virtual_terms: Vec::new(),
        }
    }
}

/// Overlapping sliding-window chunker. Retained for unknown extensions and as
/// the backing routine for sub-chunking oversized AST chunks.
///
/// Why: AST-aware chunking requires a supported grammar; for unknown extensions
/// or files that fail to parse, a sliding window is the safe fallback that
/// still produces indexable text.
/// What: splits `content` into windows of `window` lines, advancing by
/// `stride` lines each step; each window becomes one `RawChunk`.
/// Test: `test_overlapping_chunks` and `test_chunk_id_format` in
/// `core/chunker/tests.rs`.
pub fn chunk_text(file: &str, content: &str, window: usize, stride: usize) -> Vec<RawChunk> {
    let lines: Vec<&str> = content.lines().collect();
    let mut chunks = Vec::new();
    let mut start = 0usize;

    while start < lines.len() {
        let end = (start + window).min(lines.len());
        let text = lines[start..end].join("\n");
        chunks.push(RawChunk::generic(
            format!("{}:{}:{}", file, start + 1, end),
            file.to_string(),
            start + 1,
            end,
            text,
        ));
        if end == lines.len() {
            break;
        }
        start += stride;
    }
    chunks
}
