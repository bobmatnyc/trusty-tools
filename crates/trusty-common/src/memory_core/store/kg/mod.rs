//! Temporal knowledge graph — public `KnowledgeGraph` API.
//!
//! Why: Some facts are relational and time-bounded ("Alice worked at Acme from
//! 2020 to 2023"). Vector search alone can't represent that; a triple store
//! with `valid_from`/`valid_to` intervals can. As of issue #44 the backing
//! store is redb (pure-Rust, embedded, transactional) — see `kg_redb.rs` for
//! the storage engine. The legacy SQLite `sqlite-kg` migration path was
//! removed in issue #989 (all palaces confirmed migrated).
//! What: `Triple` record + `KnowledgeGraph` handle. Every method delegates to
//! `KgStoreRedb`; async methods run blocking redb work on `tokio::task::
//! spawn_blocking` so the async reactor isn't stalled.
//! Test: Asserting the SAME (s,p,o) twice closes the first interval and opens
//! a new one; `query_active` returns only the latest. Asserting a DIFFERENT
//! object under the same (s,p) depends on the predicate (#4810): a functional
//! predicate — one listed in `kg_store::FUNCTIONAL_PREDICATES`, such as
//! `is_alias_for` — still supersedes, while every other predicate is
//! multi-valued and both objects stay active. Tests in kg/tests.rs exercise
//! the public API; storage-engine tests live in `kg_redb.rs`.

mod adjacency;
// #4670: progressive (seed + expand) exploration over the resident adjacency.
mod explore;
mod graph;
mod ops;
mod types;

#[cfg(test)]
mod tests;

pub use explore::{ExpandDirection, SeedNode};
pub use graph::KnowledgeGraph;
pub use types::{AdjacencyDesync, KgEdge, Triple};
