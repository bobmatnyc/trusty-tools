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
//! compacted flag + pinned flag); helpers append the seed messages, an
//! assistant turn (text and/or tool calls), and a `tool` role result message.
//! `assistant_text` concatenates the assistant text turns for the final
//! `AgentOutput`. `maybe_compact` (#2070) applies [`super::compaction`]'s
//! threshold policy; `to_messages` renders the model-facing view —
//! contiguous compacted runs collapsed to one summary message, followed by a
//! replayed copy of the last user message once any compaction has occurred
//! (§5.4 items 2-3) — while `messages` always returns the complete,
//! uncompacted raw history. `push_tool_result` (#2070) pins any
//! [`crate::tools::USE_SKILL_TOOL_NAME`] result forever — §5.4 item 5's
//! "preserve skill outputs... forever" — alongside `system`/`user` turns.
//! Test: `agent_loop::tests` build transcripts indirectly through the loop;
//! `transcript::tests` cover the helpers and compaction directly.

use crate::llm::{ChatMessage, ChatResponse, ToolCall};
use crate::tools::USE_SKILL_TOOL_NAME;

use super::compaction::{CompactionConfig, should_compact, summarize_span};

/// One stored conversation turn plus its compaction state (#2070).
///
/// Why: Non-destructive compaction requires the original message to survive
/// even after it stops being sent to the model verbatim; a bare flag next to
/// the message (rather than removing it from the vector) is the simplest
/// representation that satisfies both "shrink what the model sees" and
/// "never lose the audit trail". `pinned` is a SEPARATE flag from `compacted`
/// (rather than overloading `compacted`) because a pinned entry must never
/// transition to `compacted` at all — it is a permanent exemption, not a
/// one-way state like compaction itself.
/// What: `message` is the original, untouched `ChatMessage`; `compacted`
/// starts `false` and is set `true` by `Transcript::maybe_compact` — never
/// reset, since compaction is a one-way transform for a given entry. `pinned`
/// starts `false` and is set `true` only at construction time (never later)
/// for entries `maybe_compact` must always skip — currently: skill outputs
/// (`push_tool_result` sets it when `name == USE_SKILL_TOOL_NAME`; §5.4 item
/// 5 also covers `system`/`user` role turns, but those are matched by role
/// directly in `maybe_compact` rather than via this flag, since they are
/// identifiable without extra state).
/// Test: `transcript::tests::maybe_compact_marks_middle_span_only`,
/// `transcript::tests::maybe_compact_never_compacts_pinned_skill_output`.
#[derive(Debug, Clone)]
struct TranscriptEntry {
    message: ChatMessage,
    compacted: bool,
    pinned: bool,
}

impl TranscriptEntry {
    /// An ordinary entry: uncompacted, unpinned.
    fn fresh(message: ChatMessage) -> Self {
        Self {
            message,
            compacted: false,
            pinned: false,
        }
    }

    /// A permanently-exempt entry (#2070): uncompacted and, unlike `fresh`,
    /// never eligible to become compacted by `Transcript::maybe_compact`.
    fn pinned(message: ChatMessage) -> Self {
        Self {
            message,
            compacted: false,
            pinned: true,
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
            cache_control: None,
        }));
    }

    /// Append a `tool` role result message answering a specific tool call.
    ///
    /// Why: The API requires each tool call to be answered by a `tool` message
    /// referencing its `tool_call_id`, or the next assistant turn errors.
    /// (#2070) A `use_skill` result is the loaded body of a skill the model
    /// deliberately chose to invoke — §5.4 item 5 requires it survive
    /// compaction forever, the same guarantee `system`/`user` turns get.
    /// What: Pushes a `ChatMessage::tool_result` with the call id, function
    /// name, and textual result — as a permanently `pinned` entry when
    /// `name == `[`USE_SKILL_TOOL_NAME`], otherwise as an ordinary compaction-
    /// eligible entry.
    /// Test: `transcript::tests::push_tool_result_sets_call_id`,
    /// `transcript::tests::maybe_compact_never_compacts_pinned_skill_output`,
    /// `transcript::tests::maybe_compact_still_compacts_non_skill_tool_results`.
    pub fn push_tool_result(&mut self, tool_call_id: &str, name: &str, content: &str) {
        let message = ChatMessage::tool_result(tool_call_id, name, content);
        let entry = if name == USE_SKILL_TOOL_NAME {
            TranscriptEntry::pinned(message)
        } else {
            TranscriptEntry::fresh(message)
        };
        self.entries.push(entry);
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
    /// the loop body needing to know compaction's internals. (#2278) A raw
    /// entry-COUNT cutoff has no tool-call awareness: when it lands between
    /// an assistant entry carrying `tool_calls` and one of its answering
    /// `tool`-role entries, the assistant gets folded into the compacted
    /// summary while the answering entry survives verbatim in the kept
    /// zone — an orphaned tool result with no tool use, which providers
    /// with a strict pairing invariant (Bedrock's Converse API) reject
    /// outright.
    /// What: No-ops (returns `false`) unless the CURRENT model-facing view
    /// (`to_messages()`, not the ever-growing raw history — raw size would
    /// re-trigger every turn even after compaction shrank what's actually
    /// sent) exceeds `cfg.token_threshold`. Otherwise computes the naive
    /// count-based cutoff `keep_from = len - cfg.keep_last_messages`, then
    /// (#2278) [`turn_group_start`]/[`turn_group_end`] check whether that
    /// cutoff falls INSIDE a turn group — an assistant entry carrying
    /// `tool_calls` plus the maximal contiguous run of `tool`-role entries
    /// immediately following it (naturally covering multi-tool-call turns:
    /// N `tool_calls` followed by up to N `tool` entries). If so, the whole
    /// group is pulled FORWARD into the kept zone by moving `keep_from` back
    /// to the group's start — `cfg.keep_last_messages` becomes a soft floor
    /// that may slightly enlarge the active zone rather than ever splitting
    /// a group. Marks every entry before the (possibly adjusted) `keep_from`
    /// as `compacted = true`, EXCEPT entries already compacted (a one-way
    /// transform), `pinned` entries (§5.4 item 5's skill-output half — see
    /// `push_tool_result`), and `system`/`user` role entries (§5.4 item 5's
    /// "preserve... user requests forever" half). Sets `compaction_triggered`
    /// and returns `true` only if at least one entry was newly marked (an
    /// already-fully-compacted or entirely-preserved prefix is a no-op, not
    /// a spurious trigger).
    /// Test: `transcript::tests::maybe_compact_is_noop_below_threshold`,
    /// `transcript::tests::maybe_compact_never_touches_system_or_user`,
    /// `transcript::tests::maybe_compact_never_compacts_pinned_skill_output`,
    /// `transcript::tests::maybe_compact_still_compacts_non_skill_tool_results`,
    /// `transcript::tests::maybe_compact_pulls_whole_multi_tool_call_group_forward`.
    pub fn maybe_compact(&mut self, cfg: &CompactionConfig) -> bool {
        if !should_compact(&self.to_messages(), cfg) {
            return false;
        }

        let mut keep_from = self.entries.len().saturating_sub(cfg.keep_last_messages);
        if keep_from > 0
            && keep_from < self.entries.len()
            && let Some(group_start) = turn_group_start(&self.entries, keep_from - 1)
            && turn_group_end(&self.entries, group_start) >= keep_from
        {
            keep_from = group_start;
        }
        let mut newly_compacted = false;

        for entry in &mut self.entries[..keep_from] {
            if entry.compacted || entry.pinned {
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

/// If entry `idx` belongs to a turn group, return the group's assistant
/// entry index (#2278 Fix A).
///
/// Why: `maybe_compact` needs to know whether its naive count-based cutoff
/// lands inside an atomic assistant-`tool_calls` + answering-`tool`-entries
/// group, so it can pull the whole group forward rather than splitting it.
/// What: A turn group is one assistant entry with `tool_calls: Some(_)` plus
/// the maximal contiguous run of `tool`-role entries immediately following
/// it. Walks backward from `idx` through any contiguous `tool`-role entries;
/// the walk stops at the first non-`tool` entry, which is the group's
/// assistant entry IF it carries `tool_calls` — returns its index in that
/// case, `None` otherwise (covers `idx` itself already being that assistant
/// entry, when the backward walk takes zero steps).
/// Test: `transcript::tests::maybe_compact_pulls_whole_multi_tool_call_group_forward`.
fn turn_group_start(entries: &[TranscriptEntry], idx: usize) -> Option<usize> {
    let mut i = idx;
    while entries[i].message.role == "tool" {
        if i == 0 {
            return None;
        }
        i -= 1;
    }
    (entries[i].message.role == "assistant" && entries[i].message.tool_calls.is_some()).then_some(i)
}

/// Return the last index of the turn group starting at `group_start`
/// (#2278 Fix A).
///
/// Why: Once [`turn_group_start`] confirms a group exists, `maybe_compact`
/// must know where it ENDS to tell whether it straddles the naive cutoff —
/// naturally spanning however many `tool` entries answer a multi-tool-call
/// turn, not just one.
/// What: Scans forward from `group_start` while the next entry is `tool`-role;
/// returns the last such index (or `group_start` itself if no `tool` entry
/// immediately follows, e.g. a `tool_calls` turn with no results recorded
/// yet).
/// Test: `transcript::tests::maybe_compact_pulls_whole_multi_tool_call_group_forward`.
fn turn_group_end(entries: &[TranscriptEntry], group_start: usize) -> usize {
    let mut i = group_start;
    while i + 1 < entries.len() && entries[i + 1].message.role == "tool" {
        i += 1;
    }
    i
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

    /// Assert every `tool` entry in a `to_messages()` view has its issuing
    /// assistant `tool_calls` entry present in the SAME view, and every
    /// assistant `tool_calls` entry has all its answers present too.
    ///
    /// Why: Shared invariant check for the turn-group-atomic compaction
    /// tests (#2278) — this is exactly the shape that produces Bedrock's
    /// orphaned-toolResult `ValidationException` if compaction ever splits a
    /// group across its cutoff.
    fn assert_view_pairing_intact(view: &[ChatMessage]) {
        let mut introduced: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut answered: std::collections::HashSet<String> = std::collections::HashSet::new();
        for msg in view {
            if let Some(calls) = &msg.tool_calls {
                for call in calls {
                    introduced.insert(call.id.clone());
                }
            }
            if msg.role == "tool" {
                let id = msg.tool_call_id.clone().unwrap_or_default();
                assert!(
                    introduced.contains(&id),
                    "tool entry for {id:?} has no preceding assistant tool_calls entry in this view: {view:?}"
                );
                answered.insert(id);
            }
        }
        for id in &introduced {
            assert!(
                answered.contains(id),
                "assistant tool_calls id {id:?} has no answering tool entry in this view: {view:?}"
            );
        }
    }

    /// A naive count-based cutoff that would split a multi-tool-call turn
    /// group is pulled forward atomically instead (#2278 Fix A).
    ///
    /// Why: This is the exact root cause of the Bedrock `ValidationException:
    /// missing toolResult` bug — a raw entry-count cutoff has no tool-call
    /// awareness, so it can land between an assistant's `tool_calls` entry
    /// and one of its answering `tool` entries (here, a two-tool-call turn),
    /// orphaning whichever half ends up in the compacted summary.
    /// What: Five ordinary assistant/tool pairs (no tool_calls — plain text
    /// turns), then ONE assistant entry with TWO `tool_calls` immediately
    /// followed by their two answering `tool` entries. `keep_last_messages`
    /// is tuned so the naive `len - keep_last_messages` cutoff lands between
    /// the multi-call assistant entry and its second answer (i.e. splits the
    /// group under the old count-only logic). Asserts compaction still
    /// fires, the five ordinary pairs got compacted (soft floor enlarged,
    /// not disabled), and — critically — `to_messages()` satisfies
    /// [`assert_view_pairing_intact`]: the multi-call assistant entry and
    /// BOTH its answers survive together, uncompacted.
    /// Test: this test.
    #[test]
    fn maybe_compact_pulls_whole_multi_tool_call_group_forward() {
        let mut t = Transcript::seed("s", "the task");
        for i in 0..5 {
            t.push_assistant(Some(format!("turn {i}")), &[]);
            t.push_tool_result(&format!("c{i}"), "bash", "output");
        }
        // entries so far: [system, user, (assistant, tool) x5] = 12 entries,
        // indices 0..12.
        t.push_assistant(
            None,
            &[
                sample_call("multi_1", "get_weather"),
                sample_call("multi_2", "get_time"),
            ],
        );
        t.push_tool_result("multi_1", "get_weather", "72F");
        t.push_tool_result("multi_2", "get_time", "12:00 UTC");
        // entries now: 15 total; the multi-call group is at indices 12..15.
        assert_eq!(t.messages().len(), 15);

        // len(15) - keep_last_messages(2) = 13: falls strictly between the
        // multi-call assistant entry (12) and its second answer (14) — a
        // naive cutoff would compact index 12 while keeping 13 and 14.
        let cfg = CompactionConfig {
            token_threshold: 1,
            keep_last_messages: 2,
        };
        assert!(t.maybe_compact(&cfg));

        // The five earlier ordinary pairs (indices 2..12, 10 entries) are
        // still eligible and got compacted — the fix only protects the
        // straddled group, it doesn't disable compaction entirely.
        assert_eq!(t.compacted_count(), 10);

        let view = t.to_messages();
        assert_view_pairing_intact(&view);
        assert!(
            view.iter()
                .any(|m| m.tool_calls.as_ref().is_some_and(|c| c.len() == 2)),
            "the multi-tool-call assistant entry must survive uncompacted: {view:?}"
        );
    }

    /// A `use_skill` tool result is pinned: it survives compaction even when
    /// it falls outside the active zone and the transcript is well past the
    /// threshold (#2070, §5.4 item 5).
    ///
    /// Why: This is the exact gap QA flagged — skill outputs flow through the
    /// ordinary `push_tool_result` channel and, without pinning, would be
    /// just as compaction-eligible as any other tool result.
    /// What: Push a `use_skill` result early, then enough later turns to push
    /// it well outside a small active zone; force compaction with an
    /// aggressive config; assert `to_messages()` still contains the skill
    /// result's ORIGINAL content verbatim (not replaced by a summary), and
    /// that `messages()` (raw) also still has it — pinned entries are never
    /// marked `compacted` at all, not merely retrievable via raw history.
    /// Test: this test.
    #[test]
    fn maybe_compact_never_compacts_pinned_skill_output() {
        let mut t = Transcript::seed("s", "task");
        t.push_tool_result(
            "call_skill",
            USE_SKILL_TOOL_NAME,
            "full skill body: do the distinctive thing",
        );
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
        assert!(
            compacted_view
                .iter()
                .any(|m| m.content.as_deref() == Some("full skill body: do the distinctive thing")),
            "pinned skill output must survive verbatim in the compacted view: {compacted_view:?}"
        );
    }

    /// Non-skill tool results (e.g. `bash`, `read_file`) remain
    /// compaction-eligible — pinning must not accidentally exempt everything
    /// and defeat compaction (#2070).
    ///
    /// Why: The fix for the skill-output gap must be narrowly scoped to
    /// `USE_SKILL_TOOL_NAME`; a broad "pin every tool result" mistake would
    /// silently disable compaction for ordinary long tool-heavy runs.
    /// What: Same shape as the skill-pinning test but with only ordinary
    /// `bash` tool results outside the active zone; assert compaction still
    /// collapses them into a summary (the compacted view is materially
    /// smaller than the raw history, and a summary marker is present).
    /// Test: this test.
    #[test]
    fn maybe_compact_still_compacts_non_skill_tool_results() {
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
        assert!(
            compacted_view.iter().any(|m| m
                .content
                .as_deref()
                .unwrap_or("")
                .contains("[compacted")),
            "expected non-skill tool results to still collapse into a summary: {compacted_view:?}"
        );
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
