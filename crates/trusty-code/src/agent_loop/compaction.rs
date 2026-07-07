//! Non-destructive context compaction trigger + summary generation (#2070).
//!
//! Why: vision spec §5.4 ("Non-Destructive Compaction + Last-Message Replay")
//! calls for a token-efficiency middleware in the agent-loop message buffer:
//! once the running transcript grows past a threshold, older turns should be
//! collapsed into a short summary rather than sent verbatim on every
//! subsequent request — while never discarding the original content. This
//! module holds the two pure, transcript-shape-agnostic pieces of that
//! mechanism — the trigger decision and the summary text — so
//! `super::transcript::Transcript` (which owns the actual entries and their
//! `compacted` flags) can stay focused on state management. Splitting the
//! policy (when / what to say) from the mechanism (marking entries, replaying
//! the last user turn) keeps both halves independently testable.
//! What: [`CompactionConfig`] (the tunable threshold + active-zone size),
//! [`estimate_tokens`] (a cheap chars/4 heuristic — no tokenizer dependency),
//! [`should_compact`] (threshold check), and [`summarize_span`] (a
//! deterministic one-line summary of a compacted message span, built from
//! role counts and any tool names invoked — never an LLM call, so compaction
//! never blocks the loop on network I/O).
//! Test: `compaction::tests::*`.

use crate::llm::ChatMessage;

/// Tuning knobs for non-destructive context compaction (#2070, §5.4).
///
/// Why: A caller (production: `AgentLoop`; tests: unit tests exercising
/// `Transcript` directly) needs to control both "how big is too big" and
/// "how much of the recent conversation counts as the active work zone" —
/// bundling them avoids two separate parameters threading through every call
/// site.
/// What: `token_threshold` is compared against the transcript's
/// [`estimate_tokens`] total; `keep_last_messages` is the number of the most
/// recent messages (the active work zone, §5.4 item 4) that are never
/// eligible for compaction regardless of size.
/// Test: `compaction::tests::default_is_sane`.
#[derive(Debug, Clone, Copy)]
pub struct CompactionConfig {
    /// Estimated-token threshold that triggers compaction once exceeded.
    pub token_threshold: usize,
    /// Number of most-recent messages kept verbatim (the active work zone).
    pub keep_last_messages: usize,
}

impl Default for CompactionConfig {
    /// §5.4 gives no exact numbers; these defaults keep several full
    /// tool-call round-trips (a handful of assistant+tool pairs) uncompacted
    /// while triggering well before a typical model's context window is at
    /// risk.
    fn default() -> Self {
        Self {
            token_threshold: 6_000,
            keep_last_messages: 6,
        }
    }
}

/// Estimate the token count of a text span using a cheap chars/4 heuristic.
///
/// Why: The agent loop has no tokenizer dependency today and adding one only
/// to gate a compaction trigger would be disproportionate; the standard
/// chars-divided-by-four rule of thumb is accurate enough to decide "is this
/// transcript getting large", which is all the trigger needs.
/// What: Returns `text.chars().count() / 4`, floor-divided.
/// Test: `compaction::tests::estimate_tokens_uses_chars_over_four`.
pub fn estimate_tokens(text: &str) -> usize {
    text.chars().count() / 4
}

/// Sum the estimated token count across a span of messages.
///
/// Why: The trigger decision operates on the whole transcript's estimated
/// size, not any single message.
/// What: Sums `estimate_tokens` over each message's content plus role/name
/// overhead is intentionally ignored — the heuristic only needs to track
/// growth trend, not byte-exact accounting.
/// Test: `compaction::tests::estimate_total_tokens_sums_content`.
pub fn estimate_total_tokens(messages: &[ChatMessage]) -> usize {
    messages
        .iter()
        .map(|m| estimate_tokens(m.content.as_deref().unwrap_or_default()))
        .sum()
}

/// Whether the transcript's estimated size warrants compaction.
///
/// Why: Centralises the threshold comparison so `Transcript::maybe_compact`
/// reads as policy application, not arithmetic.
/// What: `true` when `estimate_total_tokens(messages) > cfg.token_threshold`.
/// Test: `compaction::tests::should_compact_respects_threshold`.
pub fn should_compact(messages: &[ChatMessage], cfg: &CompactionConfig) -> bool {
    estimate_total_tokens(messages) > cfg.token_threshold
}

/// Build a deterministic one-line summary of a span of messages being
/// compacted (§5.4 item 2: "Replace their content with a one-line summary").
///
/// Why: A real LLM-generated summary would require a synchronous network
/// call in the middle of the loop's turn boundary — expensive, and a new
/// failure mode compaction must not introduce. A deterministic summary built
/// from role counts and invoked tool names is cheap, always available, and
/// gives the model (and a human auditor) enough of a pointer to know what was
/// elided.
/// What: Counts assistant/tool/other messages in `span` and collects the
/// distinct tool names named in `tool` role messages (via their `name`
/// field), rendering `"[compacted N earlier messages: A assistant turn(s),
/// T tool call(s) (name1, name2, ...)]"`. Returns a fixed placeholder for an
/// empty span (should not occur in practice, but must not panic).
/// Test: `compaction::tests::summarize_span_counts_roles_and_tools`,
/// `compaction::tests::summarize_span_handles_empty_span`.
pub fn summarize_span(span: &[ChatMessage]) -> String {
    if span.is_empty() {
        return "[compacted 0 earlier messages]".to_string();
    }

    let assistant_count = span.iter().filter(|m| m.role == "assistant").count();
    let mut tool_names: Vec<&str> = span
        .iter()
        .filter(|m| m.role == "tool")
        .filter_map(|m| m.name.as_deref())
        .collect();
    tool_names.sort_unstable();
    tool_names.dedup();

    let tool_count = span.iter().filter(|m| m.role == "tool").count();
    let tools_suffix = if tool_names.is_empty() {
        String::new()
    } else {
        format!(" ({})", tool_names.join(", "))
    };

    format!(
        "[compacted {} earlier message{}: {} assistant turn{}, {} tool call{}{}]",
        span.len(),
        plural_suffix(span.len()),
        assistant_count,
        plural_suffix(assistant_count),
        tool_count,
        plural_suffix(tool_count),
        tools_suffix,
    )
}

/// Return `"s"` unless `n == 1`, for readable pluralisation in the summary.
fn plural_suffix(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn tool_msg(name: &str) -> ChatMessage {
        ChatMessage::tool_result("call_1", name, "result")
    }

    #[test]
    fn default_is_sane() {
        let cfg = CompactionConfig::default();
        assert!(cfg.token_threshold > 0);
        assert!(cfg.keep_last_messages > 0);
    }

    #[test]
    fn estimate_tokens_uses_chars_over_four() {
        assert_eq!(estimate_tokens("abcd"), 1);
        assert_eq!(estimate_tokens("abcdefgh"), 2);
        assert_eq!(estimate_tokens(""), 0);
    }

    #[test]
    fn estimate_total_tokens_sums_content() {
        let messages = vec![
            ChatMessage::user("abcd"),
            ChatMessage::assistant("abcdefgh"),
        ];
        assert_eq!(estimate_total_tokens(&messages), 3);
    }

    #[test]
    fn should_compact_respects_threshold() {
        let cfg = CompactionConfig {
            token_threshold: 2,
            keep_last_messages: 1,
        };
        let small = vec![ChatMessage::user("ab")];
        let large = vec![ChatMessage::user("abcdefghijkl")];
        assert!(!should_compact(&small, &cfg));
        assert!(should_compact(&large, &cfg));
    }

    #[test]
    fn summarize_span_counts_roles_and_tools() {
        let span = vec![
            ChatMessage::assistant("thinking"),
            tool_msg("bash"),
            tool_msg("read_file"),
            tool_msg("bash"),
        ];
        let summary = summarize_span(&span);
        assert!(summary.contains("4 earlier messages"));
        assert!(summary.contains("1 assistant turn"));
        assert!(summary.contains("3 tool calls"));
        assert!(summary.contains("bash"));
        assert!(summary.contains("read_file"));
    }

    #[test]
    fn summarize_span_handles_empty_span() {
        assert_eq!(summarize_span(&[]), "[compacted 0 earlier messages]");
    }
}
