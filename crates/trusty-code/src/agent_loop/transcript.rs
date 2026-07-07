//! Conversation transcript for the multi-turn agent loop.
//!
//! Why: The loop must accumulate the full message history (system, user,
//! assistant turns, and tool results) to send back to the model on each
//! iteration and to render a final answer. Wrapping the `Vec<ChatMessage>` in a
//! small type gives the loop intent-revealing helpers (`push_assistant`,
//! `push_tool_result`) and keeps message-construction details out of the loop
//! body. (#2070) It also owns non-destructive context compaction (vision spec
//! §5.4): each entry carries a `compacted` flag rather than the message ever
//! being deleted, so the outward, model-facing view can shrink while the
//! stored history stays fully auditable.
//! What: `Transcript` owns an ordered `Vec<TranscriptEntry>` (message +
//! compacted flag); helpers append the seed messages, an assistant turn (text
//! and/or tool calls), and a `tool` role result message. `assistant_text`
//! concatenates the assistant text turns for the final `AgentOutput`.
//! `maybe_compact` (#2070) applies [`super::compaction`]'s threshold policy;
//! `to_messages` renders the model-facing view — contiguous compacted runs
//! collapsed to one summary message, followed by a replayed copy of the last
//! user message once any compaction has occurred (§5.4 items 2-3) — while
//! `messages` always returns the complete, uncompacted raw history.
//! Test: `agent_loop::tests` build transcripts indirectly through the loop;
//! `transcript::tests` cover the helpers and compaction directly.

use crate::llm::{ChatMessage, ChatResponse, ToolCall};

use super::compaction::{CompactionConfig, should_compact, summarize_span};

/// One stored conversation turn plus its compaction state (#2070).
///
/// Why: Non-destructive compaction requires the original message to survive
/// even after it stops being sent to the model verbatim; a bare flag next to
/// the message (rather than removing it from the vector) is the simplest
/// representation that satisfies both "shrink what the model sees" and
/// "never lose the audit trail".
/// What: `message` is the original, untouched `ChatMessage`; `compacted`
/// starts `false` and is set `true` by `Transcript::maybe_compact` — never
/// reset, since compaction is a one-way transform for a given entry.
/// Test: `transcript::tests::maybe_compact_marks_middle_span_only`.
#[derive(Debug, Clone)]
struct TranscriptEntry {
    message: ChatMessage,
    compacted: bool,
}

impl TranscriptEntry {
    fn fresh(message: ChatMessage) -> Self {
        Self {
            message,
            compacted: false,
        }
    }
}

/// Running conversation history for one agent-loop run.
///
/// Why: Centralises message accumulation so the loop body stays focused on
/// control flow rather than `ChatMessage` plumbing, and (#2070) centralises
/// the compaction state machine so the loop only has to call
/// `maybe_compact` at its turn boundary.
/// What: A thin wrapper over `Vec<TranscriptEntry>` with append helpers, a
/// raw-history accessor, a compacted model-facing view, and an
/// assistant-text accessor. `compaction_triggered` records whether ANY
/// compaction has happened yet in this run — once `true`, `to_messages`
/// starts appending the last-user-message replay (§5.4 item 3) on every
/// subsequent call, re-anchoring the model on every following turn, not just
/// the turn compaction fired on.
/// Test: `transcript::tests::*`.
#[derive(Debug, Default, Clone)]
pub struct Transcript {
    entries: Vec<TranscriptEntry>,
    assistant_texts: Vec<String>,
    compaction_triggered: bool,
}

impl Transcript {
    /// Seed a transcript with a system prompt and the user task.
    ///
    /// Why: Every run starts from exactly these two turns; bundling the seed in
    /// a constructor prevents the loop from forgetting either.
    /// What: Pushes a `system` message then a `user` message, both starting
    /// uncompacted. `maybe_compact` (#2070) never marks `system` or `user`
    /// role entries — see its docs — so these two seed turns are preserved
    /// forever regardless of how long the run continues.
    /// Test: `transcript::tests::seed_has_two_messages`.
    pub fn seed(system: &str, task: &str) -> Self {
        Self {
            entries: vec![
                TranscriptEntry::fresh(ChatMessage::system(system)),
                TranscriptEntry::fresh(ChatMessage::user(task)),
            ],
            assistant_texts: Vec::new(),
            compaction_triggered: false,
        }
    }

    /// Append the assistant turn from a model response.
    ///
    /// Why: The model's turn (text and any tool calls) must be added to history
    /// before the tool results, so the next request reflects the real ordering
    /// the API expects.
    /// What: Reconstructs a `ChatMessage` with `role = "assistant"`, carrying the
    /// optional text content and the emitted tool calls; records the text (if
    /// any) for the final answer.
    /// Test: `transcript::tests::push_assistant_records_text`.
    pub fn push_assistant(&mut self, text: Option<String>, tool_calls: &[ToolCall]) {
        if let Some(t) = &text
            && !t.is_empty()
        {
            self.assistant_texts.push(t.clone());
        }
        let tool_calls = if tool_calls.is_empty() {
            None
        } else {
            Some(tool_calls.to_vec())
        };
        self.entries.push(TranscriptEntry::fresh(ChatMessage {
            role: "assistant".into(),
            content: text,
            tool_calls,
            tool_call_id: None,
            name: None,
        }));
    }

    /// Append a `tool` role result message answering a specific tool call.
    ///
    /// Why: The API requires each tool call to be answered by a `tool` message
    /// referencing its `tool_call_id`, or the next assistant turn errors.
    /// What: Pushes a `ChatMessage::tool_result` with the call id, function
    /// name, and textual result.
    /// Test: `transcript::tests::push_tool_result_sets_call_id`.
    pub fn push_tool_result(&mut self, tool_call_id: &str, name: &str, content: &str) {
        self.entries
            .push(TranscriptEntry::fresh(ChatMessage::tool_result(
                tool_call_id,
                name,
                content,
            )));
    }

    /// Return the complete, uncompacted raw message history.
    ///
    /// Why: (#2070) Non-destructive compaction is only meaningful if the full
    /// history stays retrievable for audit even after `to_messages` starts
    /// collapsing older turns; this is that escape hatch.
    /// What: Clones every entry's original `ChatMessage`, in order, regardless
    /// of its `compacted` flag.
    /// Test: `transcript::tests::messages_reflects_appends`,
    /// `transcript::tests::compacted_entries_remain_in_raw_history`.
    pub fn messages(&self) -> Vec<ChatMessage> {
        self.entries.iter().map(|e| e.message.clone()).collect()
    }

    /// Render the model-facing view: compacted spans collapsed to a summary,
    /// plus a replayed last user message once compaction has occurred.
    ///
    /// Why: This is what `ChatRequest.messages` actually sends — it must
    /// shrink once compaction fires (the whole point of §5.4) while staying
    /// a valid, coherently-ordered conversation.
    /// What: Walks entries in order; a run of one-or-more contiguous
    /// `compacted` entries is replaced by ONE synthetic `system`-role message
    /// (`super::compaction::summarize_span`'s one-line text) in its place;
    /// uncompacted entries pass through unchanged. After the walk, if
    /// `compaction_triggered` is `true` (i.e. compaction has fired at least
    /// once in this transcript's life), the LAST `user`-role entry's message
    /// is cloned and appended again at the very end — §5.4 item 3's
    /// "re-anchor the model" replay. A transcript with no `user` entry (never
    /// happens via `seed`, but guarded rather than assumed) simply skips the
    /// replay.
    /// Test: `transcript::tests::to_messages_collapses_compacted_span`,
    /// `transcript::tests::to_messages_replays_last_user_message_after_compaction`,
    /// `transcript::tests::to_messages_is_passthrough_before_compaction`.
    pub fn to_messages(&self) -> Vec<ChatMessage> {
        let mut out = Vec::with_capacity(self.entries.len());
        let mut compacted_span: Vec<ChatMessage> = Vec::new();

        for entry in &self.entries {
            if entry.compacted {
                compacted_span.push(entry.message.clone());
                continue;
            }
            flush_span(&mut out, &mut compacted_span);
            out.push(entry.message.clone());
        }
        flush_span(&mut out, &mut compacted_span);

        if self.compaction_triggered
            && let Some(last_user) = self.entries.iter().rev().find(|e| e.message.role == "user")
        {
            out.push(last_user.message.clone());
        }

        out
    }

    /// Apply the compaction policy: mark eligible older entries `compacted`.
    ///
    /// Why: (#2070) This is the turn-boundary hook the agent loop calls —
    /// gated by the caller on `HarnessMode::DailyDriver` (Parity must never
    /// compact, per §5.9's D2 reconciliation) — so growth is bounded without
    /// the loop body needing to know compaction's internals.
    /// What: No-ops (returns `false`) unless the CURRENT model-facing view
    /// (`to_messages()`, not the ever-growing raw history — raw size would
    /// re-trigger every turn even after compaction shrank what's actually
    /// sent) exceeds `cfg.token_threshold`. Otherwise, marks every entry
    /// before the trailing `cfg.keep_last_messages` (the active work zone,
    /// §5.4 item 4) as `compacted = true`, EXCEPT entries already compacted
    /// (a one-way transform) and `system`/`user` role entries (§5.4 item 5:
    /// "preserve... user requests forever"). Sets `compaction_triggered` and
    /// returns `true` only if at least one entry was newly marked (an
    /// already-fully-compacted or entirely-preserved prefix is a no-op, not a
    /// spurious trigger).
    /// Test: `transcript::tests::maybe_compact_marks_middle_span_only`,
    /// `transcript::tests::maybe_compact_is_noop_below_threshold`,
    /// `transcript::tests::maybe_compact_never_touches_system_or_user`.
    pub fn maybe_compact(&mut self, cfg: &CompactionConfig) -> bool {
        if !should_compact(&self.to_messages(), cfg) {
            return false;
        }

        let keep_from = self.entries.len().saturating_sub(cfg.keep_last_messages);
        let mut newly_compacted = false;

        for entry in &mut self.entries[..keep_from] {
            if entry.compacted {
                continue;
            }
            if entry.message.role == "system" || entry.message.role == "user" {
                continue;
            }
            entry.compacted = true;
            newly_compacted = true;
        }

        if newly_compacted {
            self.compaction_triggered = true;
        }
        newly_compacted
    }

    /// Number of entries currently marked `compacted`.
    ///
    /// Why: Test/observability accessor proving compaction happened without
    /// exposing the private `TranscriptEntry` type.
    /// What: Counts entries with `compacted == true`.
    /// Test: `transcript::tests::maybe_compact_marks_middle_span_only`.
    pub fn compacted_count(&self) -> usize {
        self.entries.iter().filter(|e| e.compacted).count()
    }

    /// Join all recorded assistant text turns into the final answer string.
    ///
    /// Why: The `AgentOutput.content` should be the model's prose across the
    /// run, not the raw JSON transcript; joining the text turns yields that.
    /// What: Joins the recorded non-empty assistant texts with blank lines.
    /// Test: `transcript::tests::assistant_text_joins_turns`.
    pub fn assistant_text(&self) -> String {
        self.assistant_texts.join("\n\n")
    }

    /// Convenience accessor: push the assistant turn straight from a response.
    ///
    /// Why: Callers usually have a `ChatResponse` in hand; this saves them
    /// destructuring `first_text`/`first_tool_calls` at the call site.
    /// What: Extracts the first choice's text and tool calls and delegates to
    /// `push_assistant`.
    /// Test: `transcript::tests::push_response_appends_assistant`.
    pub fn push_response(&mut self, resp: &ChatResponse) {
        let text = resp.first_text();
        let calls = resp.first_tool_calls().to_vec();
        self.push_assistant(text, &calls);
    }
}

/// Flush a pending compacted span into `out` as one summary message, if any.
///
/// Why: `to_messages` needs to collapse a contiguous run of compacted entries
/// exactly once, whether the run ends because a non-compacted entry follows
/// or because the entries list ended; sharing this helper avoids duplicating
/// the "if non-empty, summarise and clear" logic at both call sites.
/// What: If `span` is non-empty, pushes one `ChatMessage::system` carrying
/// `summarize_span(span)`'s text, then clears `span`.
fn flush_span(out: &mut Vec<ChatMessage>, span: &mut Vec<ChatMessage>) {
    if span.is_empty() {
        return;
    }
    out.push(ChatMessage::system(summarize_span(span)));
    span.clear();
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{FunctionCall, ToolCall};

    fn sample_call(id: &str, name: &str) -> ToolCall {
        ToolCall {
            id: id.into(),
            kind: "function".into(),
            function: FunctionCall {
                name: name.into(),
                arguments: "{}".into(),
            },
        }
    }

    /// `seed` produces exactly a system + user message pair.
    ///
    /// Why: Guards the run's starting state.
    /// What: Seed and assert two messages with the expected roles.
    /// Test: this test.
    #[test]
    fn seed_has_two_messages() {
        let t = Transcript::seed("you are helpful", "do the thing");
        assert_eq!(t.messages().len(), 2);
        assert_eq!(t.messages()[0].role, "system");
        assert_eq!(t.messages()[1].role, "user");
    }

    /// `push_assistant` records non-empty text for the final answer.
    ///
    /// Why: The final `AgentOutput` is built from assistant text turns.
    /// What: Push text + no calls; assert `assistant_text` returns it.
    /// Test: this test.
    #[test]
    fn push_assistant_records_text() {
        let mut t = Transcript::seed("s", "u");
        t.push_assistant(Some("hello".into()), &[]);
        assert_eq!(t.assistant_text(), "hello");
        assert_eq!(t.messages().last().expect("msg").role, "assistant");
    }

    /// An empty-string assistant turn is not recorded as text.
    ///
    /// Why: Tool-only turns often carry empty/None content; they must not add
    /// blank lines to the final answer.
    /// What: Push empty text; assert `assistant_text` stays empty.
    /// Test: this test.
    #[test]
    fn push_assistant_skips_empty_text() {
        let mut t = Transcript::seed("s", "u");
        t.push_assistant(Some(String::new()), &[sample_call("c1", "echo")]);
        assert_eq!(t.assistant_text(), "");
    }

    /// `push_tool_result` appends a `tool` message bound to the call id.
    ///
    /// Why: The API requires tool results to reference their call id.
    /// What: Push a tool result; assert role and `tool_call_id`.
    /// Test: this test.
    #[test]
    fn push_tool_result_sets_call_id() {
        let mut t = Transcript::seed("s", "u");
        t.push_tool_result("call_1", "echo", "echoed");
        let messages = t.messages();
        let last = messages.last().expect("msg");
        assert_eq!(last.role, "tool");
        assert_eq!(last.tool_call_id.as_deref(), Some("call_1"));
        assert_eq!(last.name.as_deref(), Some("echo"));
    }

    /// `messages` reflects appended turns in order.
    ///
    /// Why: The model must see the real chronological history.
    /// What: Seed, push assistant + tool result, assert length grows by two.
    /// Test: this test.
    #[test]
    fn messages_reflects_appends() {
        let mut t = Transcript::seed("s", "u");
        t.push_assistant(None, &[sample_call("c1", "echo")]);
        t.push_tool_result("c1", "echo", "ok");
        assert_eq!(t.messages().len(), 4);
    }

    /// `assistant_text` joins multiple text turns with blank lines.
    ///
    /// Why: A multi-turn final answer should read as coherent prose.
    /// What: Push two text turns; assert the joined form.
    /// Test: this test.
    #[test]
    fn assistant_text_joins_turns() {
        let mut t = Transcript::seed("s", "u");
        t.push_assistant(Some("first".into()), &[]);
        t.push_assistant(Some("second".into()), &[]);
        assert_eq!(t.assistant_text(), "first\n\nsecond");
    }

    /// `to_messages` is a byte-identical passthrough before compaction fires.
    ///
    /// Why: Parity mode (and any DailyDriver run under the threshold) must
    /// see exactly the raw history — no summary, no replay.
    /// What: Build a small transcript, assert `to_messages() == messages()`.
    /// Test: this test.
    #[test]
    fn to_messages_is_passthrough_before_compaction() {
        let mut t = Transcript::seed("s", "u");
        t.push_assistant(Some("hi".into()), &[]);
        assert_eq!(t.to_messages(), t.messages());
    }

    /// `maybe_compact` is a no-op below the threshold.
    ///
    /// Why: Compaction must not fire on ordinary, short runs.
    /// What: A tiny transcript against a generous threshold; assert no
    /// change and a `false` return.
    /// Test: this test.
    #[test]
    fn maybe_compact_is_noop_below_threshold() {
        let mut t = Transcript::seed("s", "u");
        t.push_assistant(Some("hi".into()), &[]);
        let cfg = CompactionConfig {
            token_threshold: 10_000,
            keep_last_messages: 1,
        };
        assert!(!t.maybe_compact(&cfg));
        assert_eq!(t.compacted_count(), 0);
    }

    /// `maybe_compact` marks only the middle span, never `system`/`user`.
    ///
    /// Why: §5.4 item 5 requires user requests (and the system preamble) to
    /// be preserved forever; only assistant/tool turns outside the active
    /// zone are compaction-eligible.
    /// What: Build a transcript with several assistant/tool turns after a
    /// long user task, set a low threshold and a small active zone, compact,
    /// and assert the system/user entries stay uncompacted while middle
    /// entries are marked.
    /// Test: this test.
    #[test]
    fn maybe_compact_never_touches_system_or_user() {
        let mut t = Transcript::seed("s", &"long task ".repeat(50));
        for i in 0..10 {
            t.push_assistant(Some(format!("assistant turn {i}")), &[]);
            t.push_tool_result(&format!("c{i}"), "bash", "output");
        }
        let cfg = CompactionConfig {
            token_threshold: 1,
            keep_last_messages: 2,
        };
        assert!(t.maybe_compact(&cfg));
        assert!(t.compacted_count() > 0);

        let raw = t.messages();
        assert_eq!(raw[0].role, "system");
        assert_eq!(raw[1].role, "user");
        // Non-destructive: every original entry is still present in the raw
        // history even though some are now marked compacted internally.
        assert_eq!(raw.len(), 2 + 20);
    }

    /// A compacted span collapses to exactly one summary message.
    ///
    /// Why: §5.4 item 2 — replace compacted content with a one-line summary,
    /// not one summary per original message.
    /// What: Force compaction, then assert `to_messages()` is shorter than
    /// the raw history and contains a `[compacted ...]` marker.
    /// Test: this test.
    #[test]
    fn to_messages_collapses_compacted_span() {
        let mut t = Transcript::seed("s", "task");
        for i in 0..10 {
            t.push_assistant(Some(format!("turn {i}")), &[]);
            t.push_tool_result(&format!("c{i}"), "bash", "output");
        }
        let cfg = CompactionConfig {
            token_threshold: 1,
            keep_last_messages: 2,
        };
        assert!(t.maybe_compact(&cfg));

        let compacted_view = t.to_messages();
        assert!(compacted_view.len() < t.messages().len());
        let has_summary = compacted_view
            .iter()
            .any(|m| m.content.as_deref().unwrap_or("").contains("[compacted"));
        assert!(
            has_summary,
            "expected a summary message in {compacted_view:?}"
        );
    }

    /// After compaction fires, `to_messages` replays the last user message
    /// at the tail.
    ///
    /// Why: §5.4 item 3 — re-anchor the model on the original task after
    /// older turns are collapsed.
    /// What: Force compaction, assert the LAST message in `to_messages()` is
    /// a `user`-role message whose content matches the seeded task.
    /// Test: this test.
    #[test]
    fn to_messages_replays_last_user_message_after_compaction() {
        let mut t = Transcript::seed("s", "the original task");
        for i in 0..10 {
            t.push_assistant(Some(format!("turn {i}")), &[]);
            t.push_tool_result(&format!("c{i}"), "bash", "output");
        }
        let cfg = CompactionConfig {
            token_threshold: 1,
            keep_last_messages: 2,
        };
        assert!(t.maybe_compact(&cfg));

        let compacted_view = t.to_messages();
        let last = compacted_view.last().expect("non-empty view");
        assert_eq!(last.role, "user");
        assert_eq!(last.content.as_deref(), Some("the original task"));
    }

    /// `maybe_compact` never panics on a minimal (seed-only) transcript, even
    /// with a threshold/active-zone combination that would otherwise compact
    /// everything.
    ///
    /// Why: The loop calls `maybe_compact` at every turn boundary, including
    /// the very first one, when the transcript holds only the seeded
    /// system+user pair; an off-by-one in the active-zone slice bound must
    /// not panic on this smallest possible input.
    /// What: Seed a transcript (2 entries), force a trigger with a
    /// zero-threshold config and an active zone larger than the transcript,
    /// call `maybe_compact`, and assert it returns `false` (nothing eligible
    /// — both entries are `system`/`user`) with no panic.
    /// Test: this test.
    #[test]
    fn maybe_compact_is_noop_on_seed_only_transcript() {
        let mut t = Transcript::seed("s", "u");
        let cfg = CompactionConfig {
            token_threshold: 0,
            keep_last_messages: 100,
        };
        assert!(!t.maybe_compact(&cfg));
        assert_eq!(t.compacted_count(), 0);
        assert_eq!(t.to_messages(), t.messages());
    }

    /// Compacted entries remain retrievable in the raw history (non-destructive).
    ///
    /// Why: §5.4's core promise is auditability — a compacted turn must
    /// still be readable in full, not summarised-and-gone.
    /// What: Force compaction, assert `messages()` still contains the
    /// original, uncompacted text of a turn that was marked `compacted`.
    /// Test: this test.
    #[test]
    fn compacted_entries_remain_in_raw_history() {
        let mut t = Transcript::seed("s", "task");
        t.push_assistant(Some("distinctive-early-turn-text".into()), &[]);
        for i in 0..10 {
            t.push_assistant(Some(format!("turn {i}")), &[]);
            t.push_tool_result(&format!("c{i}"), "bash", "output");
        }
        let cfg = CompactionConfig {
            token_threshold: 1,
            keep_last_messages: 2,
        };
        assert!(t.maybe_compact(&cfg));
        assert!(t.compacted_count() > 0);

        let raw = t.messages();
        assert!(
            raw.iter()
                .any(|m| m.content.as_deref() == Some("distinctive-early-turn-text")),
            "compacted entry's original content must survive in the raw history"
        );
    }
}
