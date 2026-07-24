//! Send-time context-window trimming: the `trim_to_budget` strategies (#69,
//! #3776) and their supporting machinery.
//!
//! Why: Extracted verbatim from `context::manager` (issue #610 500-SLOC file
//! cap) so `manager.rs` stays a thin public surface (`ContextManager`,
//! `TrimOutcome`, `context_window`) while the four-round-reviewed trimming logic
//! — water-fill truncation, the recency window, and pairing-atomic eviction —
//! lives here with its own tests. Zero behavior change from the move.
//! What: An `impl ContextManager` block carrying `trim_to_budget`, plus the free
//! helpers it needs (`recency_window_start`, `clamp_recency_to_pairings`,
//! `water_fill_cap`, `estimate_tokens`, `truncate_message_content`, and the
//! `tool_call_id` pairing helpers).
//! Test: See `mod tests` (in `trim/tests.rs`).

use std::collections::HashMap;

use super::manager::{ContextManager, TrimOutcome, context_window};

#[cfg(test)]
mod tests;

/// Minimum tokens to retain when truncating an oversized single message, so a
/// truncated tool result still carries usable signal (≈1 KB of text).
const MIN_TRUNCATED_TOKENS: u32 = 256;

/// Number of trailing messages to protect as the "live turn" when no `user`
/// message is found in the evictable region (a tools-only continuation). The
/// normal case protects from the last user message onward; this is only the
/// fallback so the newest activity keeps full fidelity regardless.
const RECENCY_FALLBACK: usize = 2;

impl ContextManager {
    /// Trim a message history to fit within `soft_threshold` of the model's
    /// context window, protecting BOTH the system/goals header and the live turn.
    ///
    /// Why: Two guarantees must hold at once.
    ///   - **The live conversation must never collapse.** An uncompressed MCP
    ///     tool result (no `compress_tool_output` filter shrinks it) can be many
    ///     megabytes. Pure oldest-first whole-message eviction evicted the user's
    ///     question AND the assistant tool-call turn AND the result itself,
    ///     leaving only the protected system message — the model answered blind
    ///     (issue #3776).
    ///   - **The newest turn must keep full fidelity when possible.** Oldest-first
    ///     eviction used to implicitly preserve the newest turn intact; naive
    ///     water-fill truncation lost that for uniformly-sized long conversations
    ///     (it shrank the newest message too). We restore it with an explicit
    ///     recency window.
    /// What: Three regions — the protected header (`0..protected_count`), an OLD
    /// history region, and a RECENCY window (the "live turn": from the last user
    /// message to the end, so it spans the current question + all trailing
    /// assistant/tool activity). Strategies apply in increasing order of damage:
    ///   1. **Water-fill truncate the OLD region only.** Compute the largest
    ///      per-message token cap that fits the budget with the header AND the
    ///      recency window kept at FULL fidelity, and truncate over-cap OLD
    ///      messages down to it. The newest turn is untouched.
    ///   2. **Evict oldest OLD messages.** If truncation alone can't fit (cap
    ///      below `MIN_TRUNCATED_TOKENS`, or non-truncatable content), drop OLD
    ///      messages from the front — never the header, never the recency window
    ///      (the original #69 behavior, now recency-safe).
    ///   3. **Water-fill truncate the RECENCY window (last resort).** Only when
    ///      the live turn itself exceeds the budget — the original oversized-MCP-
    ///      result shape, where the huge message IS in the live turn — truncate
    ///      within the recency window so the question and every assistant/tool
    ///      pairing survive rather than being evicted.
    /// Returns `(trimmed_messages, TrimOutcome)`.
    /// Test: `trim_truncates_single_oversized_message`,
    /// `trim_truncates_multiple_oversized_messages`,
    /// `trim_preserves_newest_turn_in_uniform_history`, `trim_drops_oldest_evictable`,
    /// `trim_respects_protected_count_greater_than_len`.
    pub fn trim_to_budget(
        &self,
        messages: Vec<serde_json::Value>,
        model: &str,
        protected_count: usize,
    ) -> (Vec<serde_json::Value>, TrimOutcome) {
        let budget = (context_window(model) as f32 * self.soft_threshold) as u32;
        let total: u32 = messages.iter().map(estimate_tokens).sum();
        if total <= budget {
            return (messages, TrimOutcome::default());
        }

        let protected_count = protected_count.min(messages.len());
        let recency_start = recency_window_start(&messages, protected_count);

        // Split into header | old | recent. The header is never touched; the
        // recency window is truncated only as a last resort (Strategy 3).
        let mut iter = messages.into_iter();
        let header: Vec<serde_json::Value> = iter.by_ref().take(protected_count).collect();
        let mut old: Vec<serde_json::Value> = iter
            .by_ref()
            .take(recency_start - protected_count)
            .collect();
        let mut recent: Vec<serde_json::Value> = iter.collect();

        let sum = |v: &[serde_json::Value]| -> u32 { v.iter().map(estimate_tokens).sum() };
        let header_tokens = sum(&header);
        let recent_tokens_full = sum(&recent);
        let mut evicted = 0usize;
        let mut truncated = 0usize;

        // Strategy 1: water-fill truncate the OLD region, keeping the header AND
        // the recency window at full fidelity.
        let avail_old = budget.saturating_sub(header_tokens + recent_tokens_full);
        let old_est: Vec<u32> = old.iter().map(estimate_tokens).collect();
        let cap = water_fill_cap(&old_est, avail_old);
        if cap >= MIN_TRUNCATED_TOKENS {
            for (i, m) in old.iter_mut().enumerate() {
                if old_est[i] > cap && truncate_message_content(m, cap) {
                    truncated += 1;
                }
            }
            if header_tokens + sum(&old) + recent_tokens_full <= budget {
                return (
                    reassemble(header, old, recent),
                    TrimOutcome { evicted, truncated },
                );
            }
        }

        // Strategy 2: evict oldest OLD messages (never header, never recency).
        // Pairing-aware and adjacency-independent: when the front message is an
        // assistant `tool_calls` message, sweep the ENTIRE `old` Vec for every
        // `tool` result referencing any of its ids (they need not be contiguous —
        // adversarial/replayed history can interleave groups); symmetrically,
        // when the front is a `tool` result, evict its declaring assistant and
        // all sibling results as one group. Either way a whole group leaves
        // together, so no orphaned `tool_call_id` (which providers reject) can
        // ever remain.
        while header_tokens + sum(&old) + recent_tokens_full > budget && !old.is_empty() {
            let front = old.remove(0);
            evicted += 1;
            let group_ids = assistant_tool_call_ids(&front)
                .or_else(|| tool_result_call_id(&front).and_then(|id| declarer_ids_for(&old, id)));
            if let Some(ids) = group_ids {
                let before = old.len();
                old.retain(|m| {
                    if let Some(a_ids) = assistant_tool_call_ids(m) {
                        !a_ids.iter().any(|x| ids.iter().any(|g| g == x))
                    } else if let Some(tid) = tool_result_call_id(m) {
                        !ids.iter().any(|g| g.as_str() == tid)
                    } else {
                        true
                    }
                });
                evicted += before - old.len();
            }
        }
        if header_tokens + sum(&old) + recent_tokens_full <= budget {
            return (
                reassemble(header, old, recent),
                TrimOutcome { evicted, truncated },
            );
        }

        // Strategy 3 (last resort): the recency window itself exceeds the budget
        // (the oversized message is IN the live turn — the #3776 MCP shape).
        // Truncate within the recency window so the question/tool pairings
        // survive rather than being evicted. `old` is empty here (Strategy 2 ran
        // it to exhaustion). Floor the cap at `MIN_TRUNCATED_TOKENS` so each
        // retained message still carries quotable signal even if that nudges the
        // total slightly over the SOFT budget (still far within the hard window).
        let avail_recent = budget.saturating_sub(header_tokens + sum(&old));
        let recent_est: Vec<u32> = recent.iter().map(estimate_tokens).collect();
        let cap = water_fill_cap(&recent_est, avail_recent).max(MIN_TRUNCATED_TOKENS);
        for (i, m) in recent.iter_mut().enumerate() {
            if recent_est[i] > cap && truncate_message_content(m, cap) {
                truncated += 1;
            }
        }
        (
            reassemble(header, old, recent),
            TrimOutcome { evicted, truncated },
        )
    }
}

/// Reassemble the header, old, and recency regions back into one message vector.
fn reassemble(
    header: Vec<serde_json::Value>,
    old: Vec<serde_json::Value>,
    recent: Vec<serde_json::Value>,
) -> Vec<serde_json::Value> {
    let mut out = header;
    out.reserve(old.len() + recent.len());
    out.extend(old);
    out.extend(recent);
    out
}

/// Index at which the live-turn "recency window" begins.
///
/// Why: The current turn (the question the model must answer, plus every
/// assistant/tool message produced since) begins at the most recent user
/// message. Protecting `[recency_start, len)` from truncation/eviction keeps the
/// newest turn at full fidelity and — critically — keeps the question alive even
/// when the oversized message is a tool result later in the same turn (#3776).
/// What: Scans backward for the last `role == "user"` message; falls back to the
/// last `RECENCY_FALLBACK` messages when none is present (e.g. a tools-only
/// continuation). The raw boundary is then made **pairing-atomic** via
/// [`clamp_recency_to_pairings`] so it never splits an assistant `tool_calls`
/// group from its `tool` results — otherwise Strategy 2 could evict the
/// assistant while a paired result survives in the window, producing an orphaned
/// `tool_call_id` that OpenAI/OpenRouter reject outright (code-critic HIGH).
/// Always `>= protected_count` and `<= len`.
/// Test: `trim_truncates_single_oversized_message`,
/// `trim_preserves_newest_turn_in_uniform_history`,
/// `trim_keeps_tool_call_pairing_atomic_without_user_message`.
fn recency_window_start(messages: &[serde_json::Value], protected_count: usize) -> usize {
    let len = messages.len();
    let base = (protected_count..len)
        .rev()
        .find(|&i| messages[i].get("role").and_then(|r| r.as_str()) == Some("user"))
        .unwrap_or_else(|| len.saturating_sub(RECENCY_FALLBACK).max(protected_count));
    clamp_recency_to_pairings(messages, base, protected_count)
}

/// Extract the `tool_call_id`s an assistant message declares (its `tool_calls`
/// array), or `None` when the message is not an assistant tool-call message.
fn assistant_tool_call_ids(message: &serde_json::Value) -> Option<Vec<String>> {
    if message.get("role").and_then(|r| r.as_str()) != Some("assistant") {
        return None;
    }
    let calls = message.get("tool_calls").and_then(|v| v.as_array())?;
    let ids: Vec<String> = calls
        .iter()
        .filter_map(|c| c.get("id").and_then(|v| v.as_str()).map(str::to_string))
        .collect();
    (!ids.is_empty()).then_some(ids)
}

/// The `tool_call_id` a `tool` message answers, or `None` for other roles.
fn tool_result_call_id(message: &serde_json::Value) -> Option<&str> {
    if message.get("role").and_then(|r| r.as_str()) != Some("tool") {
        return None;
    }
    message.get("tool_call_id").and_then(|v| v.as_str())
}

/// Find the assistant message in `messages` that declares `target_id` and return
/// its FULL set of `tool_call_id`s (so evicting it takes every sibling result
/// with it). `None` when no such declarer is present in the slice.
fn declarer_ids_for(messages: &[serde_json::Value], target_id: &str) -> Option<Vec<String>> {
    messages.iter().find_map(|m| {
        let ids = assistant_tool_call_ids(m)?;
        ids.iter().any(|x| x == target_id).then_some(ids)
    })
}

/// Move `start` earlier until the window `[start, len)` never contains a `tool`
/// result whose declaring assistant message sits *before* `start`.
///
/// Why: A `tool` message is only valid alongside the assistant `tool_calls`
/// message that declared its id. If the recency boundary split such a group, the
/// eviction fallback (Strategy 2) would drop the assistant from the OLD region
/// while the result stayed in the window — an orphaned `tool_call_id` the
/// provider rejects. Pulling the boundary back to the earliest referenced
/// assistant keeps every group whole inside one region.
/// What: Builds a `tool_call_id → declaring-assistant-index` map (one forward
/// scan), then expands the window leftward to a fixed point. Applied to BOTH the
/// user-anchored and fallback boundaries — cheap and future-proof.
/// Test: `trim_keeps_tool_call_pairing_atomic_without_user_message`.
fn clamp_recency_to_pairings(
    messages: &[serde_json::Value],
    mut start: usize,
    protected_count: usize,
) -> usize {
    let mut declarer: HashMap<String, usize> = HashMap::new();
    for (i, m) in messages.iter().enumerate() {
        if let Some(ids) = assistant_tool_call_ids(m) {
            for id in ids {
                declarer.entry(id).or_insert(i);
            }
        }
    }
    if declarer.is_empty() {
        return start;
    }
    loop {
        let mut earliest = start;
        for m in &messages[start..] {
            if let Some(id) = tool_result_call_id(m)
                && let Some(&d) = declarer.get(id)
                && d < earliest
            {
                earliest = d;
            }
        }
        if earliest >= start {
            return start;
        }
        start = earliest.max(protected_count);
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
/// Test: `water_fill_cap_computes_equitable_cap`,
/// `trim_truncates_multiple_oversized_messages`.
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
/// Test: `estimate_tokens_sane_for_mcp_tool_message`, and indirectly via
/// `trim_truncates_single_oversized_message`.
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
/// Test: `truncate_message_content_string_vs_non_string`,
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
