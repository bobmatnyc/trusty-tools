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

/// Number of trailing messages to protect as the "live turn" when no `user`
/// message is found in the evictable region (a tools-only continuation). The
/// normal case protects from the last user message onward; this is only the
/// fallback so the newest activity keeps full fidelity regardless.
const RECENCY_FALLBACK: usize = 2;

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
    /// `trim_respects_protected_count`.
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
        // Pairing-aware: when the front message is an assistant `tool_calls`
        // message, evict its paired `tool` results in the SAME step so we never
        // leave an orphaned `tool_call_id` (which providers reject). Groups are
        // whole within the OLD region because the recency boundary is
        // pairing-atomic (see `clamp_recency_to_pairings`).
        while header_tokens + sum(&old) + recent_tokens_full > budget && !old.is_empty() {
            let front = old.remove(0);
            evicted += 1;
            if let Some(ids) = assistant_tool_call_ids(&front) {
                while old
                    .first()
                    .and_then(tool_result_call_id)
                    .is_some_and(|id| ids.iter().any(|d| d == id))
                {
                    old.remove(0);
                    evicted += 1;
                }
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

    /// Assert every `tool` message is paired with an assistant `tool_calls`
    /// entry that declares its id — i.e. no orphaned `tool_call_id` that a
    /// provider would reject.
    fn assert_no_orphans(msgs: &[serde_json::Value]) {
        use std::collections::HashSet;
        let declared: HashSet<&str> = msgs
            .iter()
            .filter_map(|m| m.get("tool_calls").and_then(|v| v.as_array()))
            .flatten()
            .filter_map(|c| c.get("id").and_then(|v| v.as_str()))
            .collect();
        for m in msgs {
            if m.get("role").and_then(|r| r.as_str()) == Some("tool") {
                let id = m.get("tool_call_id").and_then(|v| v.as_str()).unwrap();
                assert!(
                    declared.contains(id),
                    "orphaned tool result {id}: no surviving assistant tool_calls declares it"
                );
            }
        }
    }

    /// Why: THE code-critic HIGH. A tools-only continuation (NO user message)
    /// with a multi-tool-call turn: `assistant(tool_calls=[c1,c2,c3])` +
    /// `tool(c1)` + `tool(c2)`(huge) + `tool(c3)`(huge). The `RECENCY_FALLBACK`
    /// window would split the group (2 old / 2 recent); evicting the OLD region
    /// then drops the assistant while `tool(c2)`/`tool(c3)` survive — orphaned
    /// `tool_call_id`s the provider rejects. The pairing-atomic boundary must
    /// pull the whole group into the recency window so it truncates rather than
    /// evicts.
    /// What: Assert no message is evicted, the huge results are truncated, and
    /// no tool result is orphaned.
    /// Test: This test.
    #[test]
    fn trim_keeps_tool_call_pairing_atomic_without_user_message() {
        let mgr = ContextManager::new(0.5); // sonnet: budget = 100k tokens
        let huge = "grep hit path/to/file.rs:42\n".repeat(300_000); // ~8 MB each
        let msgs = vec![
            json!({"role":"system","content":"You are izzie."}),
            json!({"role":"assistant","content":null,"tool_calls":[
                {"id":"c1","type":"function","function":{"name":"grep","arguments":"{}"}},
                {"id":"c2","type":"function","function":{"name":"grep","arguments":"{}"}},
                {"id":"c3","type":"function","function":{"name":"grep","arguments":"{}"}}
            ]}),
            json!({"role":"tool","tool_call_id":"c1","content":"small first result"}),
            json!({"role":"tool","tool_call_id":"c2","content": huge.clone()}),
            json!({"role":"tool","tool_call_id":"c3","content": huge}),
        ];
        let (out, outcome) = mgr.trim_to_budget(msgs, "anthropic/claude-sonnet-4-6", 1);

        assert_eq!(
            outcome.evicted, 0,
            "must not evict — would orphan tool results"
        );
        assert!(outcome.truncated >= 2, "the two huge results must truncate");
        assert_no_orphans(&out);
        // The declaring assistant and all three results survive.
        assert!(
            out.iter().any(|m| m.get("tool_calls").is_some()),
            "assistant tool_calls group must survive"
        );
        for id in ["c1", "c2", "c3"] {
            assert!(
                out.iter().any(|m| m["tool_call_id"] == id),
                "result {id} must survive"
            );
        }
        let total: u32 = out.iter().map(estimate_tokens).sum();
        let budget = (context_window("anthropic/claude-sonnet-4-6") as f32 * 0.5) as u32;
        assert!(
            total <= budget,
            "trimmed total {total} must fit budget {budget}"
        );
    }

    /// Why: Strategy 2 eviction must itself be pairing-atomic. An OLD assistant
    /// tool-call with a huge NON-STRING content-block array is large (so
    /// evicting it alone fits the budget) yet non-truncatable (truncation only
    /// shortens string content); evicting it without its (small) result would
    /// orphan that result. The atomic-group eviction must take the result with
    /// it — otherwise a naive oldest-first loop stops after the big assistant
    /// and strands `tool(old1)`.
    /// What: system + huge-content-array assistant(old1) + tiny tool(old1) [OLD]
    /// + user + assistant(c1) + tiny tool(c1) [live]. Assert the whole old group
    /// is evicted together, the live turn survives, and nothing is orphaned.
    /// Test: This test.
    #[test]
    fn trim_evicts_tool_call_group_atomically_no_orphan() {
        let mgr = ContextManager::new(0.5); // sonnet: budget = 100k tokens
        // ~150k-token, non-truncatable (array content, not a plain string).
        let huge_text = "a".repeat(600_000);
        let msgs = vec![
            json!({"role":"system","content":"sys"}),
            json!({"role":"assistant","content":[{"type":"text","text": huge_text}],"tool_calls":[
                {"id":"old1","type":"function","function":{"name":"grep","arguments":"{}"}}
            ]}),
            json!({"role":"tool","tool_call_id":"old1","content":"tiny old result"}),
            json!({"role":"user","content":"the live question"}),
            json!({"role":"assistant","content":null,"tool_calls":[
                {"id":"c1","type":"function","function":{"name":"grep","arguments":"{}"}}
            ]}),
            json!({"role":"tool","tool_call_id":"c1","content":"tiny live result"}),
        ];
        let (out, outcome) = mgr.trim_to_budget(msgs, "anthropic/claude-sonnet-4-6", 1);

        assert!(outcome.evicted >= 2, "the whole old group must be evicted");
        assert_no_orphans(&out);
        // Old group (assistant + result) both gone — no half-evicted pairing.
        assert!(
            !out.iter().any(|m| m["tool_call_id"] == "old1"),
            "old result must be evicted with its assistant"
        );
        assert!(
            !out.iter().any(|m| m
                .get("tool_calls")
                .and_then(|v| v.as_array())
                .is_some_and(|a| a.iter().any(|c| c["id"] == "old1"))),
            "old assistant tool-call must be evicted"
        );
        // The live turn survives intact.
        assert!(out.iter().any(|m| m["content"] == "the live question"));
        assert_eq!(out[out.len() - 1]["tool_call_id"], "c1");
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
        assert_eq!(
            out[1]["content"],
            "Find estimate_tokens, water_fill_cap, and trim_to_budget."
        );
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
        assert!(
            total <= budget,
            "trimmed total {total} must fit budget {budget}"
        );
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
        assert!(
            total <= budget,
            "trimmed total {total} must fit budget {budget}"
        );
    }

    /// Why: Recency guarantee (code-critic MEDIUM). In a uniformly-sized long
    /// conversation — no megabyte outlier — the NEWEST turn must keep full
    /// fidelity, exactly as the old oldest-first eviction implicitly guaranteed.
    /// Naive water-fill shrank the newest message too; the recency window fixes
    /// that. This is the critic's empirical repro: 1 system + 10 × 12 KB
    /// messages under gpt-4's ~12.8k-token (10%) budget.
    /// What: The last user message and everything after it (the live turn) are
    /// returned byte-for-byte unchanged; older messages are truncated or evicted.
    /// Test: This test.
    #[test]
    fn trim_preserves_newest_turn_in_uniform_history() {
        let body = "x".repeat(12_000); // ~3k tokens each; 10 of them ≫ 12.8k budget
        let mgr = ContextManager::new(0.1); // gpt-4: budget = 12_800 tokens
        let mut msgs = vec![json!({"role":"system","content":"sys"})];
        for i in 0..10 {
            let role = if i % 2 == 0 { "user" } else { "assistant" };
            msgs.push(json!({"role": role, "content": body.clone()}));
        }
        // Newest turn = last user message (index 9) + trailing assistant (10).
        let newest_user_before = msgs[9]["content"].as_str().unwrap().to_string();
        let newest_asst_before = msgs[10]["content"].as_str().unwrap().to_string();

        let (out, outcome) = mgr.trim_to_budget(msgs, "gpt-4", 1);

        assert!(outcome.changed(), "must trim something");
        // The newest turn survived byte-for-byte — NOT shrunk.
        assert_eq!(out[9]["content"].as_str().unwrap(), newest_user_before);
        assert_eq!(out[10]["content"].as_str().unwrap(), newest_asst_before);
        assert_eq!(
            newest_asst_before.len(),
            12_000,
            "newest message kept at full 12k-byte fidelity"
        );
        // Older messages were shrunk (truncated and/or evicted).
        assert!(
            out.len() < 11 || outcome.truncated > 0,
            "old history must shrink"
        );
        // The system header is intact and first.
        assert_eq!(out[0]["role"], "system");
        // And the whole set now fits the budget.
        let total: u32 = out.iter().map(estimate_tokens).sum();
        assert!(total <= 12_800, "trimmed total {total} must fit budget");
    }

    /// Why: The recency window must NOT save the live turn at the cost of
    /// re-breaking #3776 — when the oversized message IS in the live turn (the
    /// MCP tool result immediately after the question), Strategy 3 must truncate
    /// it in place, and the question must survive.
    /// What: system + user question + assistant tool-call + huge tool result
    /// (all within the live turn); assert the result is truncated, nothing
    /// evicted, and the question is byte-for-byte intact.
    /// Test: This test.
    #[test]
    fn trim_truncates_oversized_result_inside_live_turn() {
        let mgr = ContextManager::new(0.5);
        let huge = "grep hit path/to/file.rs:42 code\n".repeat(300_000); // ~9 MB
        let msgs = vec![
            json!({"role":"system","content":"You are izzie."}),
            json!({"role":"user","content":"Where is trim_to_budget defined?"}),
            json!({
                "role":"assistant","content":null,
                "tool_calls":[{"id":"c1","type":"function",
                    "function":{"name":"grep","arguments":"{}"}}]
            }),
            json!({"role":"tool","tool_call_id":"c1","content": huge}),
        ];
        let (out, outcome) = mgr.trim_to_budget(msgs, "anthropic/claude-sonnet-4-6", 1);
        assert_eq!(outcome.evicted, 0, "must not evict the question");
        assert_eq!(outcome.truncated, 1, "must truncate the in-turn result");
        assert_eq!(out.len(), 4);
        assert_eq!(out[1]["content"], "Where is trim_to_budget defined?");
        assert!(out[3]["content"].as_str().unwrap().contains("grep hit"));
        assert!(out[3]["content"].as_str().unwrap().contains("truncated"));
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
        assert_eq!(
            water_fill_cap(&[1_000_000, 1_000_000, 1_000_000], 90_000),
            30_000
        );
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
