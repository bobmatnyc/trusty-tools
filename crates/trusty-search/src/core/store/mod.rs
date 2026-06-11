//! Vector store module — HNSW-backed ANN store behind an async trait.
//!
//! Why: provides a seam between the code-indexer pipeline and the concrete
//! usearch HNSW implementation so tests can swap in mock backends without
//! touching production call sites.
//! What: re-exports `VectorHit`, `VectorStore` (trait), and `UsearchStore`
//! (the primary concrete impl).
//! Test: see `tests` submodule for async unit tests.

#[cfg(test)]
mod tests;
mod types;
mod usearch_impl;
mod usearch_store;

pub use self::types::{VectorHit, VectorStore};
pub use self::usearch_store::UsearchStore;
