//! Context window budgeting (#69).
//!
//! Why: Long multi-turn workflows push prompt token counts toward the model's
//! hard context limit, causing surprise failures. Proactively evicting the
//! oldest non-protected turns once usage crosses ~50% of the window keeps
//! requests comfortably within the cache-friendly zone and preserves the
//! initial system/goals block.
//! What: `ContextManager` holds a soft threshold (fraction of the window);
//! `trim_to_budget` walks the messages, estimating tokens cheaply, and
//! removes the oldest evictable entries until the total fits.
//! Test: See unit tests — we assert protected messages are never evicted and
//! that the return count matches the number trimmed.

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

/// Shared context manager — cheap to clone (`HashMap` is cloned, which is
/// acceptable because it's only used for per-agent budget caching).
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

    /// Trim a message history to fit within `soft_threshold` of the model's
    /// context window.
    ///
    /// Why: Protects the system/goals header (first `protected_count` entries)
    /// from eviction; everything beyond is fair game in oldest-first order.
    /// What: Sums rough token estimates across all messages. If under budget,
    /// returns the input unchanged. Otherwise drops evictable entries from the
    /// front until the remaining total fits.
    /// Returns `(trimmed_messages, evicted_count)`.
    /// Test: `trim_drops_oldest_evictable`, `trim_respects_protected_count`.
    pub fn trim_to_budget(
        &self,
        messages: Vec<serde_json::Value>,
        model: &str,
        protected_count: usize,
    ) -> (Vec<serde_json::Value>, usize) {
        let budget = (context_window(model) as f32 * self.soft_threshold) as u32;
        let total: u32 = messages.iter().map(estimate_tokens).sum();

        if total <= budget {
            return (messages, 0);
        }

        let protected_count = protected_count.min(messages.len());
        let mut iter = messages.into_iter();
        let protected: Vec<serde_json::Value> = iter.by_ref().take(protected_count).collect();
        let mut evictable: Vec<serde_json::Value> = iter.collect();

        let protected_tokens: u32 = protected.iter().map(estimate_tokens).sum();

        // MIN-6 (#103): Maintain a running sum of evictable tokens instead of
        // re-summing every iteration. This turns the eviction loop from O(n²)
        // into O(n) — important when a long history needs many evictions.
        let mut remaining: u32 = evictable.iter().map(estimate_tokens).sum();
        let mut evicted = 0usize;
        while protected_tokens + remaining > budget && !evictable.is_empty() {
            remaining = remaining.saturating_sub(estimate_tokens(&evictable[0]));
            evictable.remove(0);
            evicted += 1;
        }

        let mut result = protected;
        result.extend(evictable);
        (result, evicted)
    }
}

/// Rough token estimate for a chat message: 4 chars ≈ 1 token.
///
/// Why: A real tokenizer per model would be heavier than the benefit; the
/// estimator only needs to be monotonic to drive eviction correctly.
/// What: Reads `content` as a string (falls back to stringifying the full
/// value when `content` isn't a plain string — e.g. multi-part blocks).
/// Test: Indirectly via `trim_*` tests.
pub fn estimate_tokens(message: &serde_json::Value) -> u32 {
    let content_str = match message.get("content") {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
        None => message.to_string(),
    };
    ((content_str.len() as u32) / 4).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn context_window_known_models() {
        assert_eq!(context_window("anthropic/claude-sonnet-4-6"), 200_000);
        assert_eq!(context_window("claude-opus-4"), 200_000);
        assert_eq!(context_window("openai/gpt-4o"), 128_000);
        assert_eq!(context_window("openai/gpt-5.1-codex"), 400_000);
        assert_eq!(context_window("some-unknown-model"), 128_000);
    }

    #[test]
    fn trim_noop_when_under_budget() {
        let mgr = ContextManager::new(0.5);
        let msgs = vec![json!({"role":"system","content":"hi"})];
        let (out, n) = mgr.trim_to_budget(msgs.clone(), "claude-sonnet-4-6", 1);
        assert_eq!(n, 0);
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn trim_drops_oldest_evictable() {
        // Force tiny budget by using a threshold that makes virtually any
        // payload exceed it: combine a big dummy message + small threshold.
        // We can't set threshold below 0.1, so instead build messages large
        // enough that 10% of a 128k context (~12.8k tokens) is exceeded.
        let big = "a".repeat(80_000); // ~20k tokens
        let mgr = ContextManager::new(0.1);
        let msgs = vec![
            json!({"role":"system","content":"sys"}),
            json!({"role":"user","content":big.clone()}),
            json!({"role":"assistant","content":big.clone()}),
            json!({"role":"user","content":big.clone()}),
        ];
        let (out, n) = mgr.trim_to_budget(msgs, "gpt-4", 1);
        assert!(n >= 1, "expected at least one eviction");
        // Protected system message must survive.
        assert_eq!(out[0]["role"], "system");
    }

    #[test]
    fn trim_respects_protected_count_greater_than_len() {
        let mgr = ContextManager::new(0.1);
        let msgs = vec![json!({"role":"system","content":"s"})];
        // protected_count=5 > len=1 should not panic.
        let (out, n) = mgr.trim_to_budget(msgs, "gpt-4", 5);
        assert_eq!(n, 0);
        assert_eq!(out.len(), 1);
    }
}
