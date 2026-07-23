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

/// Minimum tokens to retain when truncating an oversized single message, so a
/// truncated tool result still carries usable signal (≈1 KB of text).
const MIN_TRUNCATED_TOKENS: u32 = 256;

/// Outcome of a `trim_to_budget` pass.
///
/// Why: The trimmer now has TWO ways to shed tokens — evicting whole messages
/// (oldest-first, the original behavior) and truncating a single oversized
/// message in place (new; see `trim_to_budget`). Callers must be able to tell
/// them apart: a truncation is NOT an eviction, so a truncation-only pass must
/// still surface its result (the message vector changed) but must not be logged
/// as "trimmed messages evicted".
/// What: Two independent counters. `evicted` counts whole messages dropped;
/// `truncated` counts messages whose `content` was shortened in place.
/// Test: `trim_truncates_single_oversized_message`,
/// `trim_drops_oldest_evictable`.
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
    /// from eviction. A single oversized message — in practice an uncompressed
    /// MCP tool result, which no `compress_tool_output` filter shrinks and which
    /// can be multiple megabytes — must NOT collapse the live conversation. With
    /// pure oldest-first whole-message eviction it did: the giant result forced
    /// eviction of the user's question AND the assistant's tool-call turn (both
    /// older and small), and then itself, leaving only the protected system
    /// message — so the follow-up completion was sent with no question and no
    /// tool output and the model answered blind (issue: MCP-result context
    /// eviction).
    /// What: Two strategies, in order:
    ///   1. **Truncate oversized messages (water-fill).** Compute a per-message
    ///      token cap `C` — the largest cap such that capping every evictable
    ///      message at `C` fits the budget — and truncate each evictable message
    ///      whose content exceeds `C` down to `C` in place. This handles ONE or
    ///      MANY oversized messages (e.g. the model calling `grep` several times,
    ///      each returning multiple megabytes) equitably, preserving every turn
    ///      and every assistant/tool-call pairing so the model keeps its question
    ///      and a bounded slice of each tool result. Only string `content` is
    ///      truncatable; the cap is floored at `MIN_TRUNCATED_TOKENS` so a
    ///      truncated result still carries quotable signal.
    ///   2. **Oldest-first eviction (fallback).** If truncation alone can't fit
    ///      the budget (e.g. a genuinely long multi-turn history of small turns,
    ///      or non-truncatable content still over budget), drop evictable entries
    ///      from the front until the remaining total fits — the original #69
    ///      behavior. The returned `TrimOutcome` reports both counts.
    /// Returns `(trimmed_messages, TrimOutcome)`.
    /// Test: `trim_truncates_single_oversized_message`,
    /// `trim_truncates_multiple_oversized_messages`, `trim_drops_oldest_evictable`,
    /// `trim_respects_protected_count`.
    pub fn trim_to_budget(
        &self,
        mut messages: Vec<serde_json::Value>,
        model: &str,
        protected_count: usize,
    ) -> (Vec<serde_json::Value>, TrimOutcome) {
        let budget = (context_window(model) as f32 * self.soft_threshold) as u32;
        let estimates: Vec<u32> = messages.iter().map(estimate_tokens).collect();
        let total: u32 = estimates.iter().sum();

        if total <= budget {
            return (messages, TrimOutcome::default());
        }

        let protected_count = protected_count.min(messages.len());

        // Strategy 1: water-fill truncation of oversized messages. Reduce every
        // over-cap evictable message to a shared per-message token cap so one OR
        // several megabyte tool results can't force eviction of the surrounding
        // conversation.
        let protected_tokens: u32 = estimates[..protected_count].iter().sum();
        let cap = water_fill_cap(&estimates[protected_count..], budget.saturating_sub(protected_tokens));
        let mut truncated = 0usize;
        if cap >= MIN_TRUNCATED_TOKENS {
            for idx in protected_count..messages.len() {
                if estimates[idx] > cap && truncate_message_content(&mut messages[idx], cap) {
                    truncated += 1;
                }
            }
            // If truncation alone brought us under budget, we're done — no turn
            // was evicted, so every question/tool-call/result pairing survives.
            let total_after: u32 = messages.iter().map(estimate_tokens).sum();
            if total_after <= budget {
                return (
                    messages,
                    TrimOutcome {
                        evicted: 0,
                        truncated,
                    },
                );
            }
        }

        // Strategy 2 (fallback): oldest-first whole-message eviction (#69).
        // Runs on the (possibly already-truncated) messages when truncation
        // couldn't fit the budget on its own.
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
        (result, TrimOutcome { evicted, truncated })
    }
}

/// Largest per-message token cap `C` such that capping every entry of
/// `estimates` at `C` keeps their sum within `avail` (the tokens available to
/// the evictable region after the protected header).
///
/// Why: When one or more messages are oversized, we want to shrink them
/// equitably — the biggest ones give up the most — rather than delete whole
/// turns. This is the classic "water-filling" cap: messages already smaller
/// than `C` keep their full size; those above `C` are truncated down to `C`.
/// What: Sorts the estimates ascending and walks them; at step `k` the
/// `n - k` largest remaining messages would each be capped at
/// `remaining_budget / (n - k)`. The first message that doesn't fit fully at
/// that share fixes the cap. Returns 0 when nothing fits (caller then floors or
/// falls back to eviction).
/// Test: `water_fill_cap_*`, `trim_truncates_multiple_oversized_messages`.
fn water_fill_cap(estimates: &[u32], avail: u32) -> u32 {
    if estimates.is_empty() {
        return 0;
    }
    let mut sorted: Vec<u32> = estimates.to_vec();
    sorted.sort_unstable();
    let n = sorted.len();
    let mut remaining = avail as u64;
    for (k, &e) in sorted.iter().enumerate() {
        let items_left = (n - k) as u64;
        let share = remaining / items_left;
        if (e as u64) <= share {
            // This message fits fully at its natural size; it consumes `e` and
            // the rest of the budget spreads over the larger remaining ones.
            remaining -= e as u64;
        } else {
            // `e` (and everything larger) exceeds its equal share — cap here.
            return share as u32;
        }
    }
    // Everything fit naturally (caller only invokes this when over budget, so
    // this is defensive): no truncation needed.
    u32::MAX
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

/// Truncate a message's `content` string in place to at most `target_tokens`
/// tokens, appending an elision marker.
///
/// Why: Lets `trim_to_budget` shrink one oversized message (an uncompressed MCP
/// tool result) instead of evicting the surrounding conversation. Only string
/// `content` can be truncated safely — messages whose `content` is absent
/// (e.g. an assistant tool-call message with `content: null`) or non-string are
/// left untouched so the caller falls back to eviction.
/// What: Keeps the first `target_tokens * 4` bytes (matching `estimate_tokens`'
/// 4-chars-per-token model, minus a small reserve for the marker), snapped down
/// to a UTF-8 char boundary, then appends a marker naming the dropped byte
/// count. Returns `true` iff the content was a string longer than the target
/// and was actually shortened.
/// Test: `truncate_message_content_shortens_string`,
/// `truncate_message_content_ignores_non_string`,
/// `trim_truncates_single_oversized_message`.
fn truncate_message_content(message: &mut serde_json::Value, target_tokens: u32) -> bool {
    // Reserve a little room so the appended marker doesn't push us back over
    // the per-message target.
    let max_bytes = (target_tokens as usize)
        .saturating_mul(4)
        .saturating_sub(128);
    let Some(serde_json::Value::String(s)) = message.get("content") else {
        return false;
    };
    if s.len() <= max_bytes {
        return false;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    let dropped = s.len() - end;
    let kept = &s[..end];
    let new_content =
        format!("{kept}\n…[trusty-agents: truncated {dropped} bytes to fit the context budget]");
    if let Some(slot) = message.get_mut("content") {
        *slot = serde_json::Value::String(new_content);
        return true;
    }
    false
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
        let (out, outcome) = mgr.trim_to_budget(msgs.clone(), "claude-sonnet-4-6", 1);
        assert_eq!(outcome, TrimOutcome::default());
        assert!(!outcome.changed());
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn trim_drops_oldest_evictable() {
        // Eviction fallback: MANY moderate messages whose fair per-message cap
        // falls below MIN_TRUNCATED_TOKENS, so truncation can't help and the
        // original #69 oldest-first eviction fires. Budget is ~12.8k tokens
        // (10% of gpt-4's 128k); 120 messages of ~500 tokens each (~60k total)
        // give a fair share of ~106 tokens/msg < the 256-token floor.
        let mid = "a".repeat(2_000); // ~500 tokens
        let mgr = ContextManager::new(0.1);
        let mut msgs = vec![json!({"role":"system","content":"sys"})];
        for i in 0..120 {
            let role = if i % 2 == 0 { "user" } else { "assistant" };
            msgs.push(json!({"role": role, "content": mid.clone()}));
        }
        let (out, outcome) = mgr.trim_to_budget(msgs, "gpt-4", 1);
        assert!(outcome.evicted >= 1, "expected at least one eviction");
        assert_eq!(outcome.truncated, 0, "cap below floor: evict, not truncate");
        // Protected system message must survive at the front.
        assert_eq!(out[0]["role"], "system");
        assert!(out.len() < 121, "history must shrink");
    }

    /// Why: The real demo path — the model calls an MCP tool (`grep`) SEVERAL
    /// times, accumulating MULTIPLE megabyte results. The single-dominant-message
    /// truncation didn't cover this and eviction still collapsed the turn to the
    /// system prompt (observed live as `evicted=7 before=8 after=1`). Water-fill
    /// truncation must shrink ALL oversized results and evict nothing.
    /// What: system + three interleaved (user? no — assistant tool-call + huge
    /// tool result) blocks, each result multi-MB; assert all survive, all three
    /// results truncated, none evicted, and the set fits budget.
    /// Test: This test.
    #[test]
    fn trim_truncates_multiple_oversized_messages() {
        let mgr = ContextManager::new(0.5); // sonnet: budget = 100k tokens
        let huge = "grep match with file path and code line\n".repeat(160_000); // ~6 MB
        let mut msgs = vec![
            json!({"role":"system","content":"You are izzie."}),
            json!({"role":"user","content":"Find estimate_tokens, water_fill_cap, and trim_to_budget."}),
        ];
        for id in ["call_1", "call_2", "call_3"] {
            msgs.push(json!({
                "role":"assistant","content":null,
                "tool_calls":[{"id":id,"type":"function",
                    "function":{"name":"grep","arguments":"{\"pattern\":\"x\"}"}}]
            }));
            msgs.push(json!({"role":"tool","tool_call_id":id,"content":huge.clone()}));
        }
        let original_len = msgs.len();
        let (out, outcome) = mgr.trim_to_budget(msgs, "anthropic/claude-sonnet-4-6", 1);

        assert_eq!(outcome.evicted, 0, "must not evict any turn");
        assert_eq!(outcome.truncated, 3, "all three huge results truncated");
        assert_eq!(out.len(), original_len, "every turn must survive");
        // The question and all three tool pairings are intact.
        assert_eq!(out[1]["content"], "Find estimate_tokens, water_fill_cap, and trim_to_budget.");
        for id in ["call_1", "call_2", "call_3"] {
            assert!(
                out.iter().any(|m| m["tool_call_id"] == id),
                "tool result for {id} must survive"
            );
        }
        // Each truncated result still carries quotable signal.
        let tool_msgs: Vec<&str> = out
            .iter()
            .filter(|m| m["role"] == "tool")
            .map(|m| m["content"].as_str().unwrap())
            .collect();
        assert_eq!(tool_msgs.len(), 3);
        for t in &tool_msgs {
            assert!(t.contains("grep match"), "must retain leading matches");
            assert!(t.contains("truncated"), "elision marker present");
        }
        let total: u32 = out.iter().map(estimate_tokens).sum();
        let budget = (context_window("anthropic/claude-sonnet-4-6") as f32 * 0.5) as u32;
        assert!(total <= budget, "trimmed total {total} must fit budget {budget}");
    }

    #[test]
    fn trim_respects_protected_count_greater_than_len() {
        let mgr = ContextManager::new(0.1);
        let msgs = vec![json!({"role":"system","content":"s"})];
        // protected_count=5 > len=1 should not panic.
        let (out, outcome) = mgr.trim_to_budget(msgs, "gpt-4", 5);
        assert!(!outcome.changed());
        assert_eq!(out.len(), 1);
    }

    /// Why: THE regression for the demo-blocking bug. An MCP tool result (e.g.
    /// `grep` via trusty-search) is not shrunk by any `compress_tool_output`
    /// filter and can be multiple megabytes. Under the old pure oldest-first
    /// eviction, this single huge tool message forced eviction of the user's
    /// question AND the assistant tool-call turn AND itself, collapsing a
    /// 4-message conversation to just the protected system message — so the
    /// follow-up completion answered with zero context.
    /// What: Build the exact live shape — system, user question, assistant
    /// tool-call, oversized `tool` result — and assert the trimmer TRUNCATES
    /// the result in place (does NOT evict) so every turn (question +
    /// tool-call/tool pairing) survives.
    /// Test: This test.
    #[test]
    fn trim_truncates_single_oversized_message() {
        let mgr = ContextManager::new(0.5); // sonnet: budget = 100k tokens
        // ~7 MB tool result — vastly over the 100k-token (~400 KB) budget.
        let huge = "grep match line with some file path and code\n".repeat(160_000);
        let original_len = huge.len();
        let msgs = vec![
            json!({"role":"system","content":"You are izzie, a helpful assistant."}),
            json!({"role":"user","content":"Where is estimate_tokens defined?"}),
            json!({
                "role":"assistant",
                "content": null,
                "tool_calls":[{
                    "id":"call_1",
                    "type":"function",
                    "function":{"name":"grep","arguments":"{\"pattern\":\"estimate_tokens\"}"}
                }]
            }),
            json!({"role":"tool","tool_call_id":"call_1","content": huge}),
        ];
        let (out, outcome) = mgr.trim_to_budget(msgs, "anthropic/claude-sonnet-4-6", 1);

        // The live conversation is NOT evicted: all four turns survive.
        assert_eq!(outcome.evicted, 0, "must not evict any turn");
        assert_eq!(outcome.truncated, 1, "must truncate the oversized result");
        assert_eq!(out.len(), 4, "every turn (incl. the question) must survive");

        // Roles/pairing preserved so the follow-up request is well-formed.
        assert_eq!(out[0]["role"], "system");
        assert_eq!(out[1]["role"], "user");
        assert_eq!(out[1]["content"], "Where is estimate_tokens defined?");
        assert_eq!(out[2]["role"], "assistant");
        assert_eq!(out[3]["role"], "tool");
        assert_eq!(out[3]["tool_call_id"], "call_1");

        // The tool result was shortened but still carries usable signal.
        let kept = out[3]["content"].as_str().unwrap();
        assert!(kept.len() < original_len, "result must be shortened");
        assert!(
            kept.contains("grep match line"),
            "truncated result must retain leading matches to quote"
        );
        assert!(kept.contains("truncated"), "elision marker must be present");

        // And the whole set now fits the budget.
        let total: u32 = out.iter().map(estimate_tokens).sum();
        let budget = (context_window("anthropic/claude-sonnet-4-6") as f32 * 0.5) as u32;
        assert!(total <= budget, "trimmed total {total} must fit budget {budget}");
    }

    /// Why: The water-fill cap is the core of the truncation strategy — it must
    /// pick the largest cap whose capped sum fits the available budget.
    /// What: A single dominant message gets (avail − others); several equal
    /// oversized messages split the remainder equally; an empty slice yields 0.
    /// Test: This test.
    #[test]
    fn water_fill_cap_computes_equitable_cap() {
        // Empty region.
        assert_eq!(water_fill_cap(&[], 1000), 0);
        // Single dominant message: keeps avail minus the small ones.
        // [5, 1_000_000], avail 500 -> 5 fits, cap = 495.
        assert_eq!(water_fill_cap(&[5, 1_000_000], 500), 495);
        // Three equal huge messages split the budget: 90_000 / 3 = 30_000.
        assert_eq!(water_fill_cap(&[1_000_000, 1_000_000, 1_000_000], 90_000), 30_000);
        // Mixed: small fits fully, rest split. [10, 10, 10_000], avail 1000
        // -> 10 + 10 consumed, cap = 980 over the last one.
        assert_eq!(water_fill_cap(&[10, 10, 10_000], 1000), 980);
    }

    /// Why: The token estimate for the MCP-shaped `tool` message must be sane —
    /// proportional to its content bytes — so the trimmer's size accounting is
    /// correct for this shape (it is what triggers truncation).
    /// What: A `tool` message with a known-length string content estimates to
    /// ~len/4 tokens; a huge one estimates huge (justifying truncation).
    /// Test: This test.
    #[test]
    fn estimate_tokens_sane_for_mcp_tool_message() {
        let small = json!({"role":"tool","tool_call_id":"c1","content":"a".repeat(400)});
        assert_eq!(estimate_tokens(&small), 100); // 400 bytes / 4

        let huge = json!({"role":"tool","tool_call_id":"c1","content":"a".repeat(4_000_000)});
        assert_eq!(estimate_tokens(&huge), 1_000_000); // 4 MB / 4
    }

    /// Why: Truncation must only touch string content; a null/absent or
    /// structured content must be left alone so the caller can fall back to
    /// eviction rather than corrupt the message.
    /// What: A string content longer than target is shortened (returns true); a
    /// null content is untouched (returns false).
    /// Test: This test.
    #[test]
    fn truncate_message_content_string_vs_non_string() {
        let mut str_msg = json!({"role":"tool","content":"x".repeat(10_000)});
        assert!(truncate_message_content(&mut str_msg, 256));
        assert!(str_msg["content"].as_str().unwrap().contains("truncated"));

        let mut null_msg = json!({"role":"assistant","content":null,"tool_calls":[]});
        assert!(!truncate_message_content(&mut null_msg, 256));
        assert!(null_msg["content"].is_null());
    }
}
