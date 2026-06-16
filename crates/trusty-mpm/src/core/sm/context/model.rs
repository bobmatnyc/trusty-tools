//! Rolling-context data model for the SM context engine (DOC-14 §7.1).
//!
//! Why: the Session Manager keeps an *infinite* conversation by storing the last
//! N rounds verbatim and folding everything older into a growing compressed
//! summary (§7). That requires a small, serialisable data model — a [`Round`]
//! (one operator turn + the SM reply + any tool traces), a [`ToolTrace`] for the
//! delegated-tool calls inside a round, and the [`SmConversation`] container that
//! holds the compressed block, the bounded recent-rounds window, a monotonic
//! round counter, and a running token estimate that drives the compaction
//! trigger. Splitting the pure data types into their own file keeps the engine
//! (`engine.rs`) and the compaction/persistence logic focused and under the SLOC
//! cap.
//! What: defines [`ToolTrace`], [`Round`], and [`SmConversation`] — all
//! `serde`-serialisable for the atomic state file (§7.4) — plus a couple of tiny
//! constructors/helpers used by the engine and tests. No I/O, no LLM, no clock
//! reads here: timestamps are passed IN so callers (and tests) stay deterministic.
//! Test: `model_tests.rs` covers `Round::new`, the round token/char accounting,
//! and `SmConversation` serde round-trips.

use std::collections::VecDeque;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A single delegated-tool invocation captured inside a conversation round.
///
/// Why: the SM delegates work to t-mpm sessions and tools; §7.1 records the tool
/// traces alongside the user/assistant text so a compaction call can preserve
/// "what was actually done" (session ids, commands) rather than only the prose.
/// Keeping it a small typed struct (not a bare string) lets future tickets add
/// structured fields without breaking the state-file schema.
/// What: a `name` (the tool/verb invoked) and a `summary` (a short human-readable
/// description of the call + its result). Both are plain owned strings so the
/// round is trivially serialisable and cloneable.
/// Test: `tool_trace_serde_roundtrip` in `model_tests.rs`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolTrace {
    /// The tool / verb that was invoked (e.g. `"session_new"`, `"memory_remember"`).
    pub name: String,
    /// A short description of the call and its outcome.
    pub summary: String,
}

impl ToolTrace {
    /// Construct a [`ToolTrace`] from a name and summary.
    ///
    /// Why: call sites (and the compaction-prompt renderer) build traces from
    /// `&str` literals constantly; a tiny constructor keeps them terse.
    /// What: converts both arguments into owned `String`s.
    /// Test: `tool_trace_serde_roundtrip`.
    pub fn new(name: impl Into<String>, summary: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            summary: summary.into(),
        }
    }

    /// Approximate character footprint of this trace (name + summary).
    ///
    /// Why: the round-level token estimate (§7.2) sums the characters of every
    /// part of a round, including tool traces, before applying the chars/4
    /// heuristic. Centralising the count keeps the estimate consistent.
    /// What: returns `name.len() + summary.len()` (byte length is a fine proxy
    /// for the heuristic).
    /// Test: exercised by `round_char_len_sums_all_parts`.
    pub fn char_len(&self) -> usize {
        self.name.len() + self.summary.len()
    }
}

/// One verbatim conversation round: operator turn + SM reply + tool traces.
///
/// Why: §7.1 keeps the last N rounds verbatim so the SM has exact recent context;
/// a round bundles the operator message, the SM's reply, the wall-clock time the
/// round closed, and any tool calls made while answering. Timestamps are stored
/// (not derived) so persistence is faithful and tests are deterministic.
/// What: `user` + `assistant` text, a `ts` close timestamp ([`DateTime<Utc>`] to
/// match the SM memory module's chrono convention), and a `tool_calls` vector.
/// Test: `round_new_sets_fields`, `round_char_len_sums_all_parts`,
/// `round_serde_roundtrip` in `model_tests.rs`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Round {
    /// The operator's message for this round.
    pub user: String,
    /// The SM's reply for this round.
    pub assistant: String,
    /// Wall-clock instant the round closed (passed in; never read from the clock
    /// here, for deterministic tests).
    pub ts: DateTime<Utc>,
    /// Tool/delegation traces produced while answering this round.
    pub tool_calls: Vec<ToolTrace>,
}

impl Round {
    /// Construct a [`Round`] from its parts.
    ///
    /// Why: the engine records rounds from `(user, assistant, ts, tools)`; an
    /// explicit constructor documents the field order and converts the text
    /// arguments without callers repeating `.to_string()`.
    /// What: builds a `Round` with the given text, timestamp, and tool traces.
    /// Test: `round_new_sets_fields`.
    pub fn new(
        user: impl Into<String>,
        assistant: impl Into<String>,
        ts: DateTime<Utc>,
        tool_calls: Vec<ToolTrace>,
    ) -> Self {
        Self {
            user: user.into(),
            assistant: assistant.into(),
            ts,
            tool_calls,
        }
    }

    /// Total character footprint of this round (user + assistant + traces).
    ///
    /// Why: the running [`SmConversation::token_estimate`] is updated per round by
    /// converting characters to an approximate token count (§7.2 chars/4). The
    /// round owns the canonical char count so the engine never double-counts or
    /// forgets the tool traces.
    /// What: sums `user.len()`, `assistant.len()`, and every trace's `char_len()`.
    /// Test: `round_char_len_sums_all_parts`.
    pub fn char_len(&self) -> usize {
        self.user.len()
            + self.assistant.len()
            + self
                .tool_calls
                .iter()
                .map(ToolTrace::char_len)
                .sum::<usize>()
    }
}

/// The SM's live conversation state: compressed history + recent verbatim window.
///
/// Why: this is the §7.1 data model — the in-memory (and on-disk, §7.4) buffer
/// that gives the SM effectively infinite context. `compressed_context` grows as
/// old rounds are folded in; `recent_rounds` is the bounded verbatim window;
/// `total_rounds` is a monotonic counter that never decreases even as rounds are
/// evicted; `token_estimate` is the running trigger input (§7.2).
/// What: a `serde`-serialisable struct with the four §7.1 fields. It carries no
/// behaviour beyond tiny accessors/mutators the engine drives — the trigger,
/// compaction, and assembly logic live in sibling modules so this type stays a
/// plain, persistable value.
/// Test: `conversation_default_is_empty`, `conversation_serde_roundtrip`, and the
/// engine/persist tests that drive its fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SmConversation {
    /// Running compressed summary of all rounds older than the verbatim window.
    pub compressed_context: String,
    /// The last ≤N rounds, verbatim, in chronological order (front = oldest).
    pub recent_rounds: VecDeque<Round>,
    /// Monotonic count of every round ever added (never decremented on eviction).
    pub total_rounds: u64,
    /// Running token estimate of the assembled context, driving the §7.2 trigger.
    pub token_estimate: usize,
}

impl SmConversation {
    /// A fresh, empty conversation.
    ///
    /// Why: a new `conv_id` starts with no history; a named constructor reads
    /// clearer at call sites than `Default::default()` and documents intent.
    /// What: returns the all-empty/zero default.
    /// Test: `conversation_default_is_empty`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of rounds currently held verbatim in the window.
    ///
    /// Why: the round-count trigger (§7.2a) and the acceptance tests both ask
    /// "how many rounds are in the window now?"; exposing it avoids reaching into
    /// the `VecDeque` at every call site.
    /// What: returns `recent_rounds.len()`.
    /// Test: engine tests (`window_evicts_oldest_round`).
    pub fn window_len(&self) -> usize {
        self.recent_rounds.len()
    }
}

#[cfg(test)]
#[path = "model_tests.rs"]
mod tests;
