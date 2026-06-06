//! Multi-language AST-aware code indexer.
//!
//! Why: Semantic code search needs function-level chunks with precise
//! `{file, function_name, start_line, end_line}` metadata so results link
//! back to exact locations. Splitting by AST boundaries (via tree-sitter)
//! produces chunks that respect language semantics — far better than
//! naive line-window slicing.
//! What: Exposes [`CodeChunk`] (the search result shape) and
//! [`CodeIndexer`] (the orchestrator that walks files, parses them with
//! tree-sitter, embeds function bodies, and writes them into a
//! [`crate::memory::MemoryStore`] under [`crate::memory::Segment::CodeIndex`]).
//! Test: See unit tests in `indexer.rs` — AST chunking per language,
//! markdown heading split, fallback to line windows, and an end-to-end
//! index+search round-trip via a mock store/embedder.
//!
//! NOTE: `#[allow(dead_code)]` is set while callers are still being wired
//! (the PM loop and workflow engine will gain an indexing command in a
//! follow-up issue). Tests exercise the public surface so this is safe.

#![allow(dead_code)]

pub mod indexer;
pub mod query_classifier;
pub mod service;
pub mod service_client;
pub mod watcher;

#[allow(unused_imports)]
pub use indexer::{CodeChunk, CodeIndexer};
#[allow(unused_imports)]
pub use service_client::SearchDaemonClient;
#[allow(unused_imports)]
pub use watcher::FileWatcher;
