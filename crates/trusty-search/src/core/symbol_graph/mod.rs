//! `SymbolGraph`: petgraph-backed call graph derived from the chunk corpus.
//!
//! Why: query intent like "who calls `authenticate`?" or "what does
//! `process_request` delegate to?" can't be answered well by BM25/HNSW
//! alone. A directed call graph (caller → callee) lets the search pipeline
//! expand around a hit, surfacing adjacent code at a discounted score
//! (KG-expansion = 0.7 × trigger RRF score).
//!
//! What: a `petgraph::DiGraph<SymbolNode, ()>` keyed by symbol name (the
//! `function_name` recorded on each `RawChunk` — qualified for Rust methods,
//! e.g. `Foo::bar`). Edges point from caller symbol to callee symbol. The
//! graph is cheap to rebuild from the corpus and is held in
//! `Arc<SymbolGraph>` so search handlers can read concurrently without
//! locking.
//!
//! Test: see `tests.rs` — covers basic build, `callers_of`, `callees_of`,
//! 1-hop and 2-hop traversal, qualified-method names, and unknown-symbol
//! queries.
//!
//! # Module layout
//!
//! | File | Contents |
//! |------|----------|
//! | `types.rs` | `SymbolNode`, `ChunkTuple`, `SymbolGraph` struct, cap constant |
//! | `build.rs` | Construction, persistence (`save_to_corpus`, `load_from_corpus`), `edge_kind_breakdown` |
//! | `internals.rs` | Private build-pass helpers (symbol registration, edge wiring, BFS utilities) |
//! | `query.rs` | Read-only traversal: `callers_of`, `callees_of`, `neighbors_by_edge`, BFS engine |
//! | `mutate.rs` | `update_file`, `remove_file` |
//! | `tests.rs` | Unit tests |

mod build;
mod internals;
mod mutate;
mod query;
mod types;

#[cfg(test)]
mod tests_advanced;
#[cfg(test)]
mod tests_basic;

// Re-export the public surface so call sites use
// `crate::core::symbol_graph::{SymbolGraph, SymbolNode, ChunkTuple, max_kg_nodes}`.
pub use types::{max_kg_nodes, ChunkTuple, SymbolGraph, SymbolNode};
