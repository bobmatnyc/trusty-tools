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

// #4906: the deferred-embed lane (retry + durable failure ledger) and the
// vector-coverage health/repair surface that answers "which drawers have no
// vector" and re-embeds them.
mod deferred_embed;
mod embed_repair;
mod embedder;
mod handle;
mod layers;
// #5037: the minimum-score gate `layers` never had — `truncate(top_k)` was its
// only length control.
mod relevance;
// ADR-0027 T9: room/wing recall scoping, shared by L2 and the list paths.
mod scope;
// ADR-0028 D3/D4/D5 (#4886): Tier C admission and atomic retire-on-write.
mod tier_c;
mod types;

#[cfg(test)]
mod embed_repair_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tier_c_tests;
#[cfg(test)]
mod timeout_tests;

// ── Public re-exports ────────────────────────────────────────────────────────

// Shared embedder
#[cfg(any(test, feature = "embedder-test-support"))]
pub use embedder::seed_shared_embedder_with_mock;
pub use embedder::{shared_embedder, shared_embedder_initialized};

// Core types
pub use types::{
    CrossPalaceResult, ForgetOutcome, L0Identity, L1Essential, RecallResult, RememberOptions,
    RetrievalLayers,
};

// Palace handle
pub use handle::PalaceHandle;

// #4906: vector-coverage health and the repair backfill. `RetryPolicy` is
// public because a caller choosing to run a longer policy than the write-path
// default is a legitimate operator decision.
pub use deferred_embed::RetryPolicy;
pub use embed_repair::{
    AliasAudit, AliasRepairOptions, AliasRepairOutcome, AliasRepairReport, EmbedHealth,
    VectorBackfillOptions, VectorBackfillReport,
};

// Recall scoping (ADR-0027 T9)
pub use scope::{RecallScope, list_drawers_in_wing, scope_admits};

// #5037: relevance floor. Public because the consumer that must apply it —
// trusty-memory's prompt-context injection — lives in another crate.
pub use relevance::{DEFAULT_RELEVANCE_FLOOR, FloorOutcome, apply_relevance_floor};

// ADR-0028 Tier C admission (#4886). The write half (`persist_with_retirement`)
// stays crate-internal — every writer reaches it through
// `PalaceHandle::remember_with_options`, which is the single enforcement point.
pub use tier_c::{
    FACT_KEY_MAX_LEN, TIER_C_DEFAULT_TTL_HOURS, TierCAdmission, TierCRefusal, admit_tier_c,
    validate_fact_key,
};

// Layer functions
pub use layers::{
    expand_query, recall, recall_across_palaces, recall_across_palaces_with_default_embedder,
    recall_deep, recall_deep_in_room, recall_deep_scoped, recall_deep_with_default_embedder,
    recall_in_room, recall_scoped, recall_with_default_embedder, rescore_l1_by_similarity,
    retrieve_l0_l1, retrieve_l2, retrieve_l2_scoped, retrieve_l3, retrieve_l3_scoped,
};
