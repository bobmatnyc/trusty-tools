//! Vector store module — HNSW-backed ANN store behind an async trait.
//!
//! Why: provides a seam between the code-indexer pipeline and the concrete
//! usearch HNSW implementation so tests can swap in mock backends without
//! touching production call sites.
//! What: re-exports `VectorHit`, `VectorStore` (trait), and `UsearchStore`
//! (the primary concrete impl).
//! Test: see `tests` submodule for async unit tests.

// #9414: the search beam and query floor, scaled with index size.
mod hnsw_tuning;
pub(crate) mod path_match;
// #2936: reaps staging files a SIGKILLed process left behind. Its own file so
// `usearch_store.rs` stays under the 500-SLOC production cap.
mod staging_reap;
#[cfg(test)]
mod tests;
// #2936: staging-file reaping and the abort-race guarantee against a real save.
mod snapshot_publish;
#[cfg(test)]
mod snapshot_tests;
#[cfg(test)]
mod tests_2936;
#[cfg(test)]
mod tests_9414;
// #9450: survivors of heavy remove churn stay reachable.
#[cfg(test)]
mod clustered_vectors;
#[cfg(test)]
mod tests_9450;
// #9450 fix round: writes during a compaction, queued callers, crash safety.
#[cfg(test)]
mod compact_9450_tests;
#[cfg(test)]
mod tests_close_8232;
// #8778: a rewrite that collapses ids leaves no orphan vector.
#[cfg(test)]
mod tests_rewrite_8778;
mod types;
// #8167/#8232: releasing the snapshot mapping when its index is deleted.
mod usearch_close;
// #9450: graph compaction after remove churn, and the one-time load heal.
mod usearch_compact;
// #9450: the vector copy a compaction builds from, so writers never wait
// for the build.
mod usearch_compact_snapshot;
// #6826: the whole view↔heap demotion state machine (the #2164 clean-store
// demote and the write-cooldown demote), in its own file so
// `usearch_store.rs` stays under the 500-SLOC production cap.
mod usearch_demote;
mod usearch_impl;
// Issue #4707: snapshot-adoption recovery for the #1711 guard. Kept in its own
// file so `usearch_store.rs` stays under the 500-SLOC production cap.
mod usearch_recover;
// #6822: the in-place scalar-precision backfill. Its own file so
// `usearch_store.rs` stays under the 500-SLOC production cap.
mod usearch_requant;
mod usearch_store;

pub use self::types::{
    relative_key, CompactMode, CompactReport, DemoteStats, ReindexProbe, RequantizeReport,
    StagedSwapOutcome, VectorHit, VectorStore,
};
pub use self::usearch_store::UsearchStore;
