//! AST-aware code chunker built on tree-sitter.
//!
//! Why: a sliding-window chunker fragments declarations and produces noisy
//! BM25/vector candidates because a single function may straddle two windows.
//! AST-aware chunking yields one chunk per top-level declaration, making
//! `function_name`, `chunk_type`, and `calls` accurate enough to drive both
//! semantic search and the knowledge-graph CALLS edges (#5, #17).
//!
//! What: `chunk_ast(file, content, language) -> (Vec<RawChunk>, Vec<RawEntity>)`
//! parses with tree-sitter, walks top-level declarations into chunks, populates
//! per-chunk fields (calls, inherits_from, nlp_keywords, …), splits oversized
//! chunks into sub-chunks with stable parent IDs, and emits a flat entity list
//! in the same pass. Unknown extensions fall back to `chunk_text()`.
//!
//! Module structure (issue #1177 split):
//!   - `parsers`  — per-thread cached tree-sitter parsers, `ParserKind`, `parse_with_cached`
//!   - `types`    — `ChunkType`, `RawChunk`, `chunk_text`
//!   - `document` — `chunk_document` and per-format chunkers (md/yaml/toml/json/txt/xml)
//!   - `ast`      — `language_for`, `chunk_ast` (the public entry point)
//!   - `classify` — file-type classifier helpers
//!   - `inherits` — inherits-from extraction helpers
//!   - `walk`     — AST walker and oversized-chunk splitter
//!
//! Test: `core/chunker/tests.rs` — covers function/method chunking, qualified
//! method names, calls extraction, named-type entities, large-function
//! splitting, unknown-language fallback, and doc-comment NLP keywords.

mod classify;
mod inherits;
mod walk;

mod ast;
mod document;
mod parsers;
mod types;

#[cfg(test)]
mod tests;

// Public re-exports — all external `crate::core::chunker::*` call sites
// remain unchanged after the split.
pub use ast::chunk_ast;
pub use document::chunk_document;
pub use types::{chunk_text, ChunkType, RawChunk};
