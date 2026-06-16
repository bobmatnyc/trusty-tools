//! Rolling auto-compaction context engine for the Session Manager (DOC-14 §7).
//!
//! Why: the SM needs *effectively infinite* conversation context without ever
//! truncating — it keeps the last N rounds verbatim and folds everything older
//! into a growing, faithful compressed summary, then assembles the working prompt
//! in a precise order on every request (§7). This module is that engine. SM-5
//! delivers the context engine in isolation; it deliberately does NOT wire into
//! any endpoint or the agent loop (those are SM-7 / SM-8) — the compaction LLM
//! call is dependency-injected through the SM-2 [`LlmProvider`] trait, and the
//! storage root is injectable, so the whole engine is unit-testable with a mock
//! provider and a tempdir.
//! What: a thin facade re-exporting the four concerns —
//! [`model`] (the §7.1 data types), [`compaction`] (the §7.3 faithful-summary
//! call + the chars/4 token heuristic), [`persist`] (the §7.4 atomic state file),
//! and [`engine`] ([`SmContextEngine`] — the §7.2 trigger + §7.5 working-prompt
//! assembly). The compaction call routes through `super::providers`.
//! Test: each submodule carries its own `*_tests.rs`; together they cover window
//! eviction, evicted-content survival, the goal/session-id golden, the
//! token-budget trigger, default-vs-override model selection, atomic-persist
//! round-trip, and §7.5 assembly order.

pub mod compaction;
pub mod engine;
pub mod model;
pub mod persist;

#[cfg(test)]
mod mock_provider;

pub use compaction::{
    COMPRESS_SUMMARY_SYSTEM_PROMPT, FAITHFUL_SUMMARY_SYSTEM_PROMPT, estimate_tokens, fold_rounds,
    resummarise,
};
pub use engine::{SmContextEngine, SmContextError};
pub use model::{Round, SmConversation, ToolTrace};
pub use persist::{ConversationStore, ConversationStoreError, ConversationStoreResult};
