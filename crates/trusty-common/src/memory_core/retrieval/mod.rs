//! 4-layer progressive retrieval: L0 (identity) -> L1 (essential) ->
//! L2 (on-demand vector) -> L3 (deep search).
//!
//! Why: LLM context windows are precious. Always loading L0+L1 (~900 tokens)
//! gives the agent baseline grounding; L2/L3 are paid only when the query
//! demands them. This dramatically improves cost and latency vs. dumping the
//! whole memory store into context.
//! What: Thin re-export facade over submodules after the 830-SLOC (#607)
//! split. All logic lives in `embedder`, `types`, `handle`, and `layers`.
//! Test: `cargo test -p trusty-common --features memory-core retrieval::`
//! exercises L0/L1 cache and L2 vector retrieval end-to-end.

mod embedder;
mod handle;
mod layers;
mod types;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod timeout_tests;

// ── Public re-exports ────────────────────────────────────────────────────────

// Shared embedder
#[cfg(any(test, feature = "embedder-test-support"))]
pub use embedder::seed_shared_embedder_with_mock;
pub use embedder::shared_embedder;

// Core types
pub use types::{
    CrossPalaceResult, L0Identity, L1Essential, RecallResult, RememberOptions, RetrievalLayers,
};

// Palace handle
pub use handle::PalaceHandle;

// Layer functions
pub use layers::{
    expand_query, recall, recall_across_palaces, recall_across_palaces_with_default_embedder,
    recall_deep, recall_deep_with_default_embedder, recall_with_default_embedder,
    rescore_l1_by_similarity, retrieve_l0_l1, retrieve_l2, retrieve_l3,
};
