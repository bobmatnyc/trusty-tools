//! Context window budgeting (#69) — public surface.
//!
//! Why: Long multi-turn workflows push prompt token counts toward the model's
//! hard context limit, causing surprise failures. Proactively trimming once
//! usage crosses the soft threshold keeps requests within the cache-friendly
//! zone and preserves the initial system/goals block AND the live turn.
//! What: This module holds the thin public surface — `ContextManager` (the
//! per-agent budget handle), `TrimOutcome` (its trim result), and
//! `context_window` (per-model ceilings). The `trim_to_budget` strategies and
//! their machinery live in the sibling `context::trim` module (split out under
//! the 500-SLOC file cap, issue #610).
//! Test: `context_window_known_models`; the trim behavior is covered by
//! `context::trim`'s own tests.

use std::collections::HashMap;

/// Return the nominal context window (in tokens) for known model families.
///
/// Why: OpenRouter does not expose per-model context windows in a uniform
/// field, so the ceiling is baked in here. Falls back to a conservative
/// 128k for anything we don't recognize.
/// What: Simple string-prefix matching on the model name.
/// Test: `context_window_known_models`.
pub fn context_window(model: &str) -> u32 {
    let m = model.to_ascii_lowercase();
    if m.contains("claude-opus") || m.contains("claude-sonnet") || m.contains("claude-haiku") {
        200_000
    } else if m.contains("gpt-5.1-codex") {
        400_000
    } else {
        // gpt-4, unknown: 128k is a safe conservative ceiling.
        128_000
    }
}

/// Outcome of a `trim_to_budget` pass.
///
/// Why: The trimmer has TWO ways to shed tokens — evicting whole messages
/// (oldest-first) and truncating an oversized message's content in place.
/// Callers must be able to tell them apart: a truncation is NOT an eviction, so
/// a truncation-only pass must still surface its result (the message vector
/// changed) but must not be logged as "trimmed messages evicted".
/// What: Two independent counters. `evicted` counts whole messages dropped;
/// `truncated` counts messages whose `content` was shortened in place.
/// Test: `trim_truncates_single_oversized_message`,
/// `trim_drops_oldest_evictable` (both in `context::trim`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TrimOutcome {
    /// Number of whole messages evicted (dropped) from the history.
    pub evicted: usize,
    /// Number of messages whose `content` was truncated in place.
    pub truncated: usize,
}

impl TrimOutcome {
    /// True when the pass changed the message vector at all (evicted or
    /// truncated at least one message). Callers use this to decide whether to
    /// re-serialize the trimmed messages or return the originals untouched.
    pub fn changed(&self) -> bool {
        self.evicted > 0 || self.truncated > 0
    }
}

/// Shared context manager — cheap to clone (`HashMap` is cloned, which is
/// acceptable because it's only used for per-agent budget caching).
///
/// Its `trim_to_budget` method is implemented in the sibling `context::trim`
/// module.
#[derive(Debug, Clone)]
pub struct ContextManager {
    /// Soft threshold as a fraction of the model's context window (0..=1).
    pub soft_threshold: f32,
    /// Cached per-agent budgets (unused today; reserved for future agent-level
    /// overrides so callers can preallocate budgets without re-walking config).
    #[allow(dead_code)]
    budgets: HashMap<String, u32>,
}

impl ContextManager {
    /// Construct with the given soft threshold (clamped to [0.1, 1.0]).
    pub fn new(soft_threshold: f32) -> Self {
        Self {
            soft_threshold: soft_threshold.clamp(0.1, 1.0),
            budgets: HashMap::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_window_known_models() {
        assert_eq!(context_window("anthropic/claude-sonnet-4-6"), 200_000);
        assert_eq!(context_window("claude-opus-4"), 200_000);
        assert_eq!(context_window("openai/gpt-4o"), 128_000);
        assert_eq!(context_window("openai/gpt-5.1-codex"), 400_000);
        assert_eq!(context_window("some-unknown-model"), 128_000);
    }
}
