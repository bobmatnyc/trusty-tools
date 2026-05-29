//! AST-aware code indexer: walks source trees and produces function-level chunks.
//!
//! Why: Downstream semantic search needs chunks that are both small enough to
//! embed usefully and large enough to be meaningful. Splitting on AST function
//! boundaries gives exactly that — a single function (or method) becomes one
//! chunk with precise `{file, function_name, start_line, end_line}`. That in
//! turn lets search results be rendered as clickable `path:line` references.
//! What: [`CodeChunk`] is the result shape surfaced to callers;
//! [`CodeIndexer`] orchestrates file reads, AST extraction via tree-sitter,
//! embedding via the injected [`crate::memory::Embedder`], and persistence via
//! the injected [`crate::memory::MemoryStore`] under
//! [`crate::memory::Segment::CodeIndex`].
//! Test: Unit tests in the `tests` submodule cover each language's chunker,
//! markdown heading split, fallback for files with no function nodes, and a
//! full index+search round-trip using a mock store and embedder.
//!
//! Module layout (split for the 500-line cap, #365):
//! - [`chunker`] — language detection + tree-sitter / markdown / fallback
//!   extraction (pure, disk- and embedder-free).
//! - [`index`] — the [`CodeIndexer`] write path: construction, warm/cool-down
//!   lifecycle, and `index_file` / `remove_file` / `index_directory`.
//! - [`search`] — the [`CodeIndexer`] read path: query embedding cache,
//!   vector search, hybrid RRF fusion, and knowledge-graph expansion.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub(crate) mod chunker;
pub(crate) mod index;
pub(crate) mod search;

#[cfg(test)]
mod tests;

/// Maximum number of characters kept per chunk text payload.
///
/// Why: Embedding models and downstream display both have practical limits;
/// ~2000 chars is a reasonable upper bound for most function bodies and keeps
/// payloads small in redb.
pub(crate) const MAX_CHUNK_CHARS: usize = 2000;

/// Reciprocal Rank Fusion constant (industry standard k=60).
///
/// Why: RRF is parameter-free across score distributions; the only knob is
/// the smoothing constant `k`. 60 is the value Cormack/Clarke/Buettcher
/// recommended in the original paper and is the default in Elastic, Vespa,
/// and most production hybrid-search stacks.
pub(crate) const RRF_K: f32 = 60.0;

/// Maximum number of distinct query embeddings cached at once (#376 D2).
///
/// Why: Repeated queries within a session (a user iterating on the same
/// search, or the LLM re-asking the same thing on retries) shouldn't
/// re-pay the FastEmbedder cost (~10–30ms). 256 entries is plenty for a
/// session and bounds memory at ~256 * 384 floats ≈ 400 KB.
pub(crate) const QUERY_CACHE_CAPACITY: usize = 256;

/// Default cool-down window after which an idle search index is evicted.
///
/// Why: Default chosen for the user-facing knob in `[search]
/// cool_after_minutes`. 15 minutes balances "never cold under interactive
/// use" against "don't pin a multi-MB HNSW for an idle PM session".
/// What: Used by `CodeIndexer::with_default_cool_after` and by the config
/// loader (`CodeIndexer::new`) when no override is supplied.
/// Test: `cool_down_evicts_after_inactivity` (with a small override).
pub const DEFAULT_COOL_AFTER_MINUTES: u64 = 15;

pub use index::CodeIndexer;

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
