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
/// assistant-text accessor. `last_compaction_len` records the entry count at
/// the moment compaction most recently fired (`None` if it has never fired
/// in this run) — `to_messages` compares it against the CURRENT entry count
/// to append the last-user-message replay (§5.4 item 3) only on the turn
/// compaction actually fired on, not on every subsequent call.
/// Test: `transcript::tests::*`.
#[derive(Debug, Default, Clone)]
pub struct Transcript {
    entries: Vec<TranscriptEntry>,
    assistant_texts: Vec<String>,
    last_compaction_len: Option<usize>,
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
            last_compaction_len: None,
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
    /// `last_compaction_len` equals the CURRENT entry count (i.e. this call
    /// is happening on the very turn `maybe_compact` last fired on — the
    /// loop calls `maybe_compact` then `to_messages` back-to-back before any
    /// further entries are appended), the LAST `user`-role entry's message is
    /// cloned and appended again at the very end — §5.4 item 3's "re-anchor
    /// the model" replay. On every later call the entry count has grown past
    /// `last_compaction_len`, so the replay does not repeat until compaction
    /// fires again. A transcript with no `user` entry (never happens via
    /// `seed`, but guarded rather than assumed) simply skips the replay.
    /// Test: `transcript::tests::to_messages_collapses_compacted_span`,
    /// `transcript::tests::to_messages_replays_last_user_message_after_compaction`,
    /// `transcript::tests::to_messages_does_not_replay_on_every_subsequent_turn`,
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

        if self.last_compaction_len == Some(self.entries.len())
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
    /// "preserve... user requests forever" half). Records `last_compaction_len
    /// = Some(self.entries.len())` and returns `true` only if at least one
    /// entry was newly marked (an already-fully-compacted or
    /// entirely-preserved prefix is a no-op, not a spurious trigger) — this
    /// marker is what lets `to_messages` replay the last user message
    /// exactly once, on this turn, rather than on every subsequent call.
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
            self.last_compaction_len = Some(self.entries.len());
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

    /// Append a plain `user`-role turn WITHOUT re-seeding a system message
    /// (#2344).
    ///
    /// Why: `session::SessionRegistry::begin_pm_transcript` uses this to
    /// hand a SUBSEQUENT `task.run` on the same session its new request as a
    /// fresh user turn on top of the growing, already-seeded conversation —
    /// `Transcript::seed` is only ever called ONCE per session, on its
    /// first run.
    /// What: Pushes an uncompacted, unpinned `user`-role `ChatMessage`.
    /// Test: `transcript_tests::push_user_appends_without_reseeding_system`.
    pub fn push_user(&mut self, task: &str) {
        self.entries
            .push(TranscriptEntry::fresh(ChatMessage::user(task)));
    }

    /// Number of assistant text turns recorded so far (#2344).
    ///
    /// Why: `AgentLoop::run_with_transcript` needs a marker of "how much
    /// assistant text existed before this run" so it can later scope the
    /// run's OWN reported output to just its new turns via
    /// [`Self::assistant_text_since`], even though the underlying
    /// conversation is persistent and keeps growing across runs.
    /// What: The current length of the internal `assistant_texts`
    /// accumulator.
    /// Test: `transcript_tests::assistant_text_since_scopes_to_new_turns_only`.
    pub fn assistant_text_mark(&self) -> usize {
        self.assistant_texts.len()
    }

    /// Join assistant text turns recorded SINCE `mark` (#2344).
    ///
    /// Why: Pairs with [`Self::assistant_text_mark`] — see its docs.
    /// What: Joins `self.assistant_texts[mark..]` the same way
    /// [`Self::assistant_text`] joins the whole vec. An out-of-range `mark`
    /// (should never happen — only ever produced by `assistant_text_mark` on
    /// the same instance) degrades to an empty string rather than panicking.
    /// Test: `transcript_tests::assistant_text_since_scopes_to_new_turns_only`,
    /// `transcript_tests::assistant_text_since_out_of_range_mark_is_empty_not_panicking`.
    pub fn assistant_text_since(&self, mark: usize) -> String {
        self.assistant_texts.get(mark..).unwrap_or(&[]).join("\n\n")
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

#[cfg(test)]
#[path = "transcript_tests.rs"]
mod tests;
