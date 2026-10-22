//! 4-layer progressive retrieval: L0 (identity) -> L1 (essential) ->
//! L2 (on-demand vector) -> L3 (deep search).
//!
//! Why: LLM context windows are precious. Always loading L0+L1 (~900 tokens)
//! gives the agent baseline grounding; L2/L3 are paid only when the query
//! demands them. This dramatically improves cost and latency vs. dumping the
//! whole memory store into context.
//! What: Thin re-export facade over submodules after the 830-SLOC (#607)
//! split. All logic lives in `embedder`, `types`, `handle`, and `layers`.
//! Test: `cargo test -p trusty-memory retrieval::`
//! exercises L0/L1 cache and L2 vector retrieval end-to-end.

// #4906: the deferred-embed lane (retry + durable failure ledger) and the
// vector-coverage health/repair surface that answers "which drawers have no
// vector" and re-embeds them.
// #9881: recall access recording, buffered and flushed in batches.
mod access;
mod chunk_backfill;
mod chunk_health;
mod chunk_progress;
// #9735: `pub(crate)` so the dream merge can refuse a split part.
pub(crate) mod chunk_split;
// #9278 T3c-1: split out of `handle` (500-SLOC cap).
mod closets;
mod deferred_embed;
// #9278 A2b: split out of `embed_repair` (500-SLOC cap).
mod alias_repair;
mod embed_repair;
mod embed_store;
mod embedder;
mod handle;
// #9279: L2/L3 candidate scoring and the exact short-id token boost.
mod candidates;
mod id_tokens;
// #9719/#9722: the live L1 cache and the open-time snapshot gap-fill.
mod l1;
mod layers;
pub(crate) mod open_sweep;
// #9390: the bounded refill past expired slots every recall lane shares.
mod refill;
// #5037: the minimum-score gate `layers` never had — `truncate(top_k)` was its
// only length control.
mod relevance;
// ADR-0027 T9: room/wing recall scoping, shared by L2 and the list paths.
mod scope;
// ADR-0028 D3/D4/D5 (#4886): Tier C admission and atomic retire-on-write.
mod tier_c;
// #9390: a slot's re-verify command, stored as a `verify_with` KG triple.
mod tier_c_verify;
mod types;
// #6366: the write critical section and its ceiling. Split out of `handle` so
// the pipeline sits in one future a timeout can drop as a unit — and so
// `handle.rs` stays under the 500-SLOC cap.
mod write_pipeline;

// #5902: `pub(crate)` only so `share::tests` can reuse this module's
// always-failing `DeadEmbedder` instead of declaring its own.
#[cfg(test)]
pub(crate) mod embed_repair_tests;
// #9433: `pub(crate)` so `tier_c_tests` can reuse its log `Capture`.
// #9275: `{uuid}#n` chunk rows through `PalaceHandle`.
#[cfg(test)]
mod chunk_backfill_measure_tests;
#[cfg(test)]
mod chunk_backfill_tests;
#[cfg(test)]
mod chunk_health_tests;
#[cfg(test)]
mod chunk_key_tests;
// #9735 PR 1b: the oversized-drawer split.
#[cfg(test)]
mod chunk_split_tests;
#[cfg(test)]
mod embed_store_tests;
#[cfg(test)]
mod embedder_tests;
#[cfg(test)]
mod l1_live_tests;
#[cfg(test)]
mod op_budget_remember_tests;
#[cfg(test)]
pub(crate) mod recall_log_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tier_c_tests;
#[cfg(test)]
mod tier_c_verify_tests;
#[cfg(test)]
mod timeout_tests;
#[cfg(test)]
mod write_pipeline_tests;

// ── Public re-exports ────────────────────────────────────────────────────────
// See #9278 (A4): `pub` only for what a caller outside the library names;
// everything else is `pub(crate)` and recorded in
// docs/reference/trusty-memory-engine-api-audit.tsv.

// Shared embedder
#[cfg(any(test, feature = "embedder-test-support"))]
pub use embedder::{seed_shared_embedder_with, seed_shared_embedder_with_mock};
pub use embedder::{shared_embedder, shared_embedder_initialized};

// Core types
// #9390: a committed slot whose verify command was not stored.
pub use tier_c_verify::VerifyNotStored;
#[cfg(feature = "server")]
pub(crate) use types::{CrossPalaceRecall, CrossPalaceResult};
pub use types::{ForgetOutcome, RecallResult, RememberOptions};

// Palace handle
pub use handle::PalaceHandle;

// #4906: vector-coverage health and the repair backfill. `RetryPolicy` is
// public because it is a field of `VectorBackfillOptions`.
pub use deferred_embed::RetryPolicy;
// #9275: the chunk-count verdict on `EmbedHealth`, and the chunked embed
// primitive trusty-memory's kuzu import writes a drawer's vectors with.
// #9278 (ruling 2026-10-14): the chunk backfill surface stays crate-only.
// #9735 PR 2: the server-only `chunk_backfill_ticker` is its caller.
#[cfg(any(test, feature = "server"))]
pub(crate) use chunk_backfill::{SliceBounds, SliceReport, SliceStop};
pub use chunk_health::ChunkCheck;
#[cfg(any(test, feature = "server"))]
pub(crate) use chunk_progress::ChunkMarker;
#[cfg(test)]
pub(crate) use chunk_progress::ChunkState;
#[cfg(feature = "server")]
pub(crate) use embed_repair::{AliasAudit, AliasRepairOptions, AliasRepairOutcome};
pub use embed_repair::{EmbedHealth, VectorBackfillOptions, VectorBackfillReport};
pub use embed_store::embed_chunks;
// #9275: the dream merge and rebuild share the bounded chunked embed.
pub(crate) use embed_store::{ChunkPlanner, embed_chunks_within, embed_planned_within};

// Recall scoping (ADR-0027 T9)
#[cfg(any(test, feature = "server"))]
pub(crate) use scope::{RecallScope, list_drawers_in_wing, scope_admits};
// #9719: L0 + L1 under a recall scope; the BM25 warming lane uses it.
#[cfg(any(test, feature = "server"))]
pub(crate) use l1::retrieve_l0_l1_scoped;

// #9390: the expired-slot refill bound the BM25 warming lane also applies.
#[cfg(feature = "server")]
pub(crate) use refill::next_refill_window;

// #5037: relevance floor, applied by the prompt-context injection.
#[cfg(feature = "server")]
pub(crate) use relevance::{DEFAULT_RELEVANCE_FLOOR, FloorOutcome, apply_relevance_floor};

// ADR-0028 Tier C admission (#4886). The write half (`persist_with_retirement`)
// stays crate-internal — every writer reaches it through
// `PalaceHandle::remember_with_options`, which is the single enforcement point.
#[cfg(test)]
pub(crate) use tier_c::TIER_C_DEFAULT_TTL_HOURS;
#[cfg(feature = "server")]
pub(crate) use tier_c::{TierCAdmission, admit_tier_c};

// Layer functions
#[cfg(feature = "server")]
pub(crate) use layers::{dedup_extend, recall_deep_scoped, recall_scoped};
#[cfg(any(test, feature = "server"))]
pub(crate) use layers::{expand_query, recall_across_palaces_reporting, retrieve_l2_scoped};
#[cfg(test)]
pub(crate) use layers::{
    recall, recall_across_palaces_with_default_embedder, recall_deep, recall_deep_in_room,
    recall_in_room, rescore_l1_by_similarity, retrieve_l0_l1, retrieve_l2, retrieve_l3,
    retrieve_l3_scoped,
};
pub use layers::{recall_deep_with_default_embedder, recall_with_default_embedder};
