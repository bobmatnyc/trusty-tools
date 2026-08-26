//! Vector store module — HNSW-backed ANN store behind an async trait.
//!
//! Why: provides a seam between the code-indexer pipeline and the concrete
//! usearch HNSW implementation so tests can swap in mock backends without
//! touching production call sites.
//! What: re-exports `VectorHit`, `VectorStore` (trait), and `UsearchStore`
//! (the primary concrete impl).
//! Test: see `tests` submodule for async unit tests.

pub(crate) mod path_match;
// #2936: reaps staging files a SIGKILLed process left behind. Its own file so
// `usearch_store.rs` stays under the 500-SLOC production cap.
mod staging_reap;
#[cfg(test)]
mod tests;
// #2936: staging-file reaping and the abort-race guarantee against a real save.
#[cfg(test)]
mod tests_2936;
mod types;
mod usearch_impl;
// Issue #4707: snapshot-adoption recovery for the #1711 guard. Kept in its own
// file so `usearch_store.rs` stays under the 500-SLOC production cap.
mod usearch_recover;
mod usearch_store;

pub use self::types::{StagedSwapOutcome, VectorHit, VectorStore};
pub use self::usearch_store::UsearchStore;
