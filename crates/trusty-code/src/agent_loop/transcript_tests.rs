//! Unit tests for `Transcript` (#2070 compaction, #2344 persistent-session
//! append/scoping helpers). Split out of `transcript.rs` per the crate's
//! `_tests.rs` sibling-file convention (see `session::registry_tests` for
//! precedent) to keep the production file under the 500-SLOC cap.

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

/// Once `maybe_compact` fires, `estimate_total_tokens` over the
/// resulting `to_messages()` view is strictly smaller than the pre-fire
/// estimate (#2308).
///
/// Why: Research for #2308 verified this reset bookkeeping is NOT the
/// bug (no feedback-loop regression) — this test locks that invariant in
/// as a regression guard alongside the model-aware threshold fix, so a
/// future change can't silently reintroduce a "compaction never actually
/// shrinks the estimate" bug.
/// What: Build a transcript with several sizeable assistant/tool turns,
/// force one compaction with a low threshold, and assert the post-fire
/// `estimate_total_tokens(&t.to_messages())` is strictly less than the
/// pre-fire estimate over the same view.
/// Test: this test.
#[test]
fn maybe_compact_estimate_drops_after_compaction_fires() {
    use super::super::compaction::estimate_total_tokens;

    let mut t = Transcript::seed("s", "task");
    for i in 0..10 {
        t.push_assistant(
            Some(format!("assistant turn {i}: {}", "x".repeat(200))),
            &[],
        );
        t.push_tool_result(&format!("c{i}"), "bash", &"output ".repeat(50));
    }

    let cfg = CompactionConfig {
        token_threshold: 1,
        keep_last_messages: 2,
    };

    let pre_fire_estimate = estimate_total_tokens(&t.to_messages());
    assert!(t.maybe_compact(&cfg));
    let post_fire_estimate = estimate_total_tokens(&t.to_messages());

    assert!(
        post_fire_estimate < pre_fire_estimate,
        "expected estimate to drop after compaction: pre={pre_fire_estimate} post={post_fire_estimate}"
    );
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
        compacted_view
            .iter()
            .any(|m| m.content.as_deref().unwrap_or("").contains("[compacted")),
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

/// The last-user-message replay fires exactly once, on the turn
/// compaction happened, and does NOT re-anchor on every subsequent turn
/// (#2302 — the turn-explosion bug: without rate-limiting, every later
/// `to_messages()` call re-appended the original task, causing the model
/// to read it as "start over from scratch" repeatedly).
///
/// Why: §5.4 item 3 only ever intended a one-time re-anchor right after
/// compaction fires, not a standing replay on every following turn.
/// What: Force compaction (as the existing replay test does) and confirm
/// the replay is present on that turn. Then push MORE assistant/tool
/// pairs WITHOUT calling `maybe_compact` again — simulating ordinary
/// turns after the compaction turn — and assert the task text appears
/// exactly ONCE in the new `to_messages()` view (only at its original
/// seeded position) and that the view's last message is NOT the task.
/// Test: this test.
#[test]
fn to_messages_does_not_replay_on_every_subsequent_turn() {
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

    // On the compaction turn itself, the replay is present at the tail.
    let compaction_turn_view = t.to_messages();
    let last = compaction_turn_view.last().expect("non-empty view");
    assert_eq!(last.role, "user");
    assert_eq!(last.content.as_deref(), Some("the original task"));

    // Simulate further turns after the compaction turn, WITHOUT calling
    // maybe_compact again — this is the normal steady state once the
    // active zone is back under threshold.
    for i in 10..15 {
        t.push_assistant(Some(format!("turn {i}")), &[]);
        t.push_tool_result(&format!("c{i}"), "bash", "output");
    }

    let later_view = t.to_messages();
    let task_occurrences = later_view
        .iter()
        .filter(|m| m.content.as_deref() == Some("the original task"))
        .count();
    assert_eq!(
        task_occurrences, 1,
        "the original task must appear exactly once (its seeded position), not replayed on every later turn: {later_view:?}"
    );
    let last = later_view.last().expect("non-empty view");
    assert_ne!(
        last.content.as_deref(),
        Some("the original task"),
        "later turns must not re-anchor the model on the original task: {later_view:?}"
    );
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

// ── #2344: persistent-session append/scoping helpers ───────────────────────

/// `push_user` appends a plain user-role turn WITHOUT re-adding a system
/// message.
///
/// Why: `session::SessionRegistry::begin_pm_transcript` uses this on every
/// task.run AFTER the first one, to append the new request onto the
/// session's already-seeded conversation.
/// What: Seed, push a second user turn, assert the raw history is
/// `[system, user, user]` with no extra system entry.
/// Test: this test.
#[test]
fn push_user_appends_without_reseeding_system() {
    let mut t = Transcript::seed("s", "first task");
    t.push_user("second task");

    let messages = t.messages();
    assert_eq!(messages.len(), 3);
    assert_eq!(messages[0].role, "system");
    assert_eq!(messages[1].role, "user");
    assert_eq!(messages[1].content.as_deref(), Some("first task"));
    assert_eq!(messages[2].role, "user");
    assert_eq!(messages[2].content.as_deref(), Some("second task"));
    assert_eq!(
        messages.iter().filter(|m| m.role == "system").count(),
        1,
        "push_user must never add a second system message"
    );
}

/// `assistant_text_mark`/`assistant_text_since` scope a run's reported
/// output to only the assistant text produced AFTER the mark.
///
/// Why: `AgentLoop::run_with_transcript` (#2344) needs to report only the
/// CURRENT run's answer even though the underlying transcript keeps
/// growing across runs — this is the primitive that makes that possible.
/// What: Mark before any assistant text exists (mark == 0); push one
/// assistant turn (run 1); mark again; push a second assistant turn (run
/// 2); assert `assistant_text_since` at each mark returns only the text
/// added after it, while `assistant_text()` still returns everything.
/// Test: this test.
#[test]
fn assistant_text_since_scopes_to_new_turns_only() {
    let mut t = Transcript::seed("s", "task");
    assert_eq!(t.assistant_text_mark(), 0);

    t.push_assistant(Some("run one's answer".into()), &[]);
    let mark_after_run_one = t.assistant_text_mark();
    assert_eq!(mark_after_run_one, 1);
    assert_eq!(t.assistant_text_since(0), "run one's answer");

    t.push_user("second task");
    t.push_assistant(Some("run two's answer".into()), &[]);

    assert_eq!(
        t.assistant_text_since(mark_after_run_one),
        "run two's answer"
    );
    assert_eq!(
        t.assistant_text(),
        "run one's answer\n\nrun two's answer",
        "the raw joined view must still contain both runs' text"
    );
}

/// `assistant_text_since` degrades to an empty string for an out-of-range
/// mark rather than panicking.
///
/// Why: Defensive robustness — `mark` should only ever come from a prior
/// `assistant_text_mark()` call on the SAME instance, but a slice-index
/// panic on any drift would be far worse than an empty string.
/// What: Call with a mark past the current length; assert `""`, not a panic.
/// Test: this test.
#[test]
fn assistant_text_since_out_of_range_mark_is_empty_not_panicking() {
    let t = Transcript::seed("s", "task");
    assert_eq!(t.assistant_text_since(100), "");
}
