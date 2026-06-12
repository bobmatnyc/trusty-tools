//! redb-backed durable chunk corpus (issue #28).
//!
//! Why: prior to this module the chunk corpus was persisted as a single
//! `chunks.json` file rewritten in full after every committed batch. On a
//! 200k-chunk corpus that JSON blob is ~400 MB; serializing it on every batch
//! commit (a reindex emits one commit per 128 files) caused the
//! memory-explosion documented in `PersistState` and forced a full re-read of
//! the entire file into a `HashMap` on every daemon restart. redb gives us:
//!   * crash-safe, atomic per-batch commits (no half-written file window),
//!   * O(batch) incremental writes instead of O(corpus) full rewrites,
//!   * the option to stream chunks back at startup without holding two copies
//!     (the JSON `Vec<RawChunk>` plus the live `HashMap`) in RAM at once.
//!
//! What: [`CorpusStore`] wraps a `redb::Database` with two tables — one keyed
//! by `chunk_id` holding the serialized [`RawChunk`], one keyed by file path
//! holding the serialized per-file [`RawEntity`] list. Values are serialized
//! with `serde_json` (already a workspace dependency; no new crate, and the
//! human-readable form keeps `redb` dumps debuggable).
//!
//! Test: see the `tests` submodule — `roundtrip` writes chunks + entities and
//! reads them back into a fresh store; `missing_db_is_empty` covers the
//! first-run / post-upgrade fallback; `delete_removes_chunk` covers eviction.

pub mod contrib;
mod corpus_ops;
mod kg_ops;
mod meta_ops;
mod store_impl;
mod tables;
#[cfg(test)]
mod tests;
mod types;

pub use self::store_impl::CorpusStore;
pub use self::types::PersistedKgNode;
