//! Tool-calling dream consolidation — structured summaries, ER facts, and
//! tombstone archival (epic #2866, PoC).
//!
//! Why: The legacy `semantic_consolidation` module parses free-text LLM
//! replies and only ever ADDS canonical drawers — palaces grow forever and
//! the knowledge graph stays shallow. This sibling module upgrades the
//! contract: a cluster of memories goes to a cheap tool-calling model via the
//! existing [`crate::chat::ChatProvider`] surface, which returns a typed
//! `{summary, inferences[], facts[]}` block through one `emit_consolidation`
//! tool call. The summary + facts are stored, and the source drawers are
//! tombstoned (never deleted) with `superseded_by` provenance so default
//! recall stops surfacing superseded content.
//! What: Re-exports the config/stats/parse surface from `types` and the
//! fail-open pass orchestrator from `pass`. Default-OFF; enable via
//! `DreamConfig::consolidation.enabled`.
//! Test: `dream_consolidation::tests` (parse validation, mock-provider full
//! pass, fail-open paths, recall filtering).

mod pass;
mod types;

#[cfg(test)]
mod tests;

pub use pass::dream_consolidation_pass;
pub use types::{
    ConsolidationFact, ConsolidationOutput, DREAM_CONSOLIDATION_PROVENANCE, DREAM_SUMMARY_TAG,
    DreamConsolidationConfig, DreamConsolidationStats, EMIT_CONSOLIDATION_TOOL, EmitParseError,
    SUPERSEDED_BY_PREDICATE, emit_consolidation_tool, parse_emit_consolidation,
};
