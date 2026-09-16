//! Tests for `super::apply` (`crate::app::reduce`), split into its own file
//! to satisfy the 500-SLOC production-file cap (`scripts/check_line_cap.sh`)
//! — mirrors the precedent set by Slice 3 (splitting a widget module's
//! implementation from its test module across two files). Reachable via
//! `crate::app::reduce::tests` exactly as an inline `mod tests { ... }`
//! would have been; only the file boundary changed.

use super::*;
use crate::app::ReplApp;
use crate::event::{KeyModifiers, WorkstreamSummary};
use crate::model::StatuslineSegment;
use crate::run::TuiModel;

fn key(code: KeyCode) -> ReplEvent {
    ReplEvent::Key(KeyInput {
        code,
        modifiers: KeyModifiers::default(),
    })
}

fn ctrl_key(c: char) -> ReplEvent {
    ReplEvent::Key(KeyInput {
        code: KeyCode::Char(c),
        modifiers: KeyModifiers {
            ctrl: true,
            alt: false,
            shift: false,
        },
    })
}

#[test]
fn apply_char_inserts_at_cursor() {
    let mut app = ReplApp::new("demo", "u");
    apply(&mut app, key(KeyCode::Char('h')));
    apply(&mut app, key(KeyCode::Char('i')));
    assert_eq!(app.input_buf, "hi");
    assert_eq!(app.cursor_pos, 2);
}

#[test]
fn apply_backspace_removes_last_char() {
    let mut app = ReplApp::new("demo", "u");
    app.insert_char('h');
    app.insert_char('i');
    apply(&mut app, key(KeyCode::Backspace));
    assert_eq!(app.input_buf, "h");
}

#[test]
fn apply_enter_submits_and_echoes() {
    let mut app = ReplApp::new("demo", "u");
    for c in "hello".chars() {
        apply(&mut app, key(KeyCode::Char(c)));
    }
    apply(&mut app, key(KeyCode::Enter));
    assert!(app.input_buf.is_empty());
    assert_eq!(app.pending_submit.take(), Some("hello".to_string()));
    assert_eq!(app.chat.len(), 1);
    assert_eq!(app.chat[0].text, "hello");
    assert_eq!(app.history, vec!["hello".to_string()]);
}

#[test]
fn apply_enter_on_empty_input_is_noop() {
    let mut app = ReplApp::new("demo", "u");
    apply(&mut app, key(KeyCode::Enter));
    assert!(app.pending_submit.is_none());
    assert!(app.chat.is_empty());
}

#[test]
fn apply_submit_event_mirrors_enter() {
    let mut app = ReplApp::new("demo", "u");
    apply(&mut app, ReplEvent::Submit("/model opus-4".to_string()));
    assert_eq!(app.pending_submit.take(), Some("/model opus-4".to_string()));
    assert_eq!(app.chat[0].text, "/model opus-4");
}

/// DOC-50 §5 Slice 5's "blocks user input until cancel completes", made
/// literal: while a turn is in flight (`busy == true`), Enter must not start
/// a second turn — and must leave the typed text sitting in `input_buf`
/// rather than consuming it and dropping it via `submit_line`'s own guard.
/// This is the direct fix for the double-submit corruption a code-review
/// pass caught on PR #3477 (task B's chunks splicing into task A's orphaned
/// `streaming_idx` entry).
#[test]
fn apply_enter_is_noop_while_busy_and_preserves_buffer() {
    let mut app = ReplApp::new("demo", "u");
    app.busy = true;
    for c in "explain Y".chars() {
        apply(&mut app, key(KeyCode::Char(c)));
    }
    let chat_len_before = app.chat.len();
    apply(&mut app, key(KeyCode::Enter));
    assert_eq!(
        app.input_buf, "explain Y",
        "typed text must survive a blocked Enter, not be discarded"
    );
    assert!(app.pending_submit.is_none(), "must not stage a second turn");
    assert_eq!(
        app.chat.len(),
        chat_len_before,
        "must not echo a second user line while busy"
    );
}

/// Same guard, exercised via the synthesized `ReplEvent::Submit` path
/// (e.g. a future picker confirmation) rather than the Enter key — proves
/// the guard lives in `submit_line` itself, not just the Enter call site.
#[test]
fn apply_submit_event_is_noop_while_busy() {
    let mut app = ReplApp::new("demo", "u");
    app.busy = true;
    apply(&mut app, ReplEvent::Submit("/model opus-4".to_string()));
    assert!(app.pending_submit.is_none());
    assert!(app.chat.is_empty(), "must not echo while busy");
}

/// Direct port of tagent's `repl_app_up_arrow_recalls_last_prompt`
/// (`crates/trusty-agents/src/repl/tui/tests_input.rs`).
#[test]
fn apply_up_recalls_last_prompt_when_idle() {
    let mut app = ReplApp::new("demo", "u");
    app.last_prompt = "hello world".to_string();
    app.busy = false;
    apply(&mut app, key(KeyCode::Up));
    assert_eq!(app.input_buf, "hello world");
    assert_eq!(app.cursor_pos, "hello world".len());
    assert!(!app.pending_cancel, "idle Up must NOT signal cancel");
}

/// Direct port of tagent's `repl_app_up_arrow_when_busy_signals_cancel`.
#[test]
fn apply_up_signals_cancel_and_recalls_when_busy() {
    let mut app = ReplApp::new("demo", "u");
    app.last_prompt = "long task".to_string();
    app.busy = true;
    apply(&mut app, key(KeyCode::Up));
    assert!(app.pending_cancel, "busy Up must signal cancel");
    assert_eq!(app.input_buf, "long task", "must restore last_prompt");
}

/// tagent's cancel signal fires unconditionally on `thinking`, ahead of
/// (and independent of) the `last_prompt` recall — pins that ordering
/// isn't accidentally coupled to `last_prompt` being non-empty.
#[test]
fn apply_up_signals_cancel_even_with_no_last_prompt() {
    let mut app = ReplApp::new("demo", "u");
    app.busy = true;
    assert!(app.last_prompt.is_empty());
    apply(&mut app, key(KeyCode::Up));
    assert!(app.pending_cancel);
    assert!(app.input_buf.is_empty());
}

/// Direct port of tagent's `repl_app_up_arrow_noop_when_no_last_prompt`.
#[test]
fn apply_up_is_noop_when_idle_and_no_last_prompt() {
    let mut app = ReplApp::new("demo", "u");
    app.insert_char('a');
    app.insert_char('b');
    app.last_prompt.clear();
    app.busy = false;
    apply(&mut app, key(KeyCode::Up));
    assert_eq!(app.input_buf, "ab");
    assert!(!app.pending_cancel);
}

/// Direct port of tagent's real `KeyCode::Down` arm — a functional
/// no-op today (nothing sets `history_idx`), kept for fidelity per
/// `crate::app`'s disclosure list.
#[test]
fn apply_down_calls_history_next_and_is_currently_inert() {
    let mut app = ReplApp::new("demo", "u");
    app.insert_char('x');
    apply(&mut app, key(KeyCode::Down));
    assert_eq!(app.input_buf, "x", "Down must not clobber input_buf");
    assert!(app.history_idx.is_none());
}

/// Direct port of tagent's `ctrl_e_pastes_last_bash_block_when_input_empty`.
#[test]
fn apply_ctrl_e_pastes_last_bash_block_when_input_empty() {
    let mut app = ReplApp::new("demo", "u");
    app.push_assistant("```bash\necho hi\n```", false);
    apply(&mut app, ctrl_key('e'));
    assert_eq!(app.input_buf, "echo hi");
    assert_eq!(app.cursor_pos, "echo hi".len());
}

/// Direct port of tagent's
/// `ctrl_e_falls_back_to_end_of_line_when_input_nonempty`.
#[test]
fn apply_ctrl_e_falls_back_to_end_of_line_when_input_nonempty() {
    let mut app = ReplApp::new("demo", "u");
    app.push_assistant("```bash\necho hi\n```", false);
    app.set_input("typed text".into());
    app.cursor_pos = 0;
    apply(&mut app, ctrl_key('e'));
    assert_eq!(app.input_buf, "typed text");
    assert_eq!(app.cursor_pos, "typed text".len());
}

/// Direct port of tagent's `ctrl_e_no_op_when_no_block_and_input_empty`.
#[test]
fn apply_ctrl_e_noop_when_no_block_and_input_empty() {
    let mut app = ReplApp::new("demo", "u");
    apply(&mut app, ctrl_key('e'));
    assert_eq!(app.input_buf, "");
    assert_eq!(app.cursor_pos, 0);
}

/// Direct port of tagent's `ctrl_e_noop_when_python_block_but_no_shell_block`.
#[test]
fn apply_ctrl_e_noop_when_only_non_shell_block_present() {
    let mut app = ReplApp::new("demo", "u");
    app.push_assistant("Result:\n```python\nprint('hi')\n```", false);
    assert_eq!(app.last_bash_block, None);
    apply(&mut app, ctrl_key('e'));
    assert_eq!(app.input_buf, "");
    assert_eq!(app.cursor_pos, 0);
}

/// Multi-line block: only the first non-blank line pastes (single-line
/// input constraint) — pins the exact behavior a naive "paste the whole
/// block" port would get wrong.
#[test]
fn apply_ctrl_e_pastes_only_first_line_of_multiline_block() {
    let mut app = ReplApp::new("demo", "u");
    app.push_assistant("```bash\ngit add -A\ngit commit -m \"msg\"\n```", false);
    apply(&mut app, ctrl_key('e'));
    assert_eq!(app.input_buf, "git add -A");
}

/// Pins the pre-first-token latency window fix: `busy` must already be
/// `true` immediately after Submit, before any `AssistantOutput` chunk
/// has arrived — this is what makes the input composer's busy indicator
/// light up right away instead of staying on the idle placeholder while
/// waiting for the first token.
#[test]
fn apply_enter_sets_busy_before_any_assistant_output_arrives() {
    let mut app = ReplApp::new("demo", "u");
    for c in "hello".chars() {
        apply(&mut app, key(KeyCode::Char(c)));
    }
    assert!(!app.busy, "must not be busy before Submit");
    apply(&mut app, key(KeyCode::Enter));
    assert!(
        app.busy,
        "must be busy immediately at Submit, not only once streaming starts"
    );
}

#[test]
fn apply_ctrl_a_e_u_move_and_clear() {
    let mut app = ReplApp::new("demo", "u");
    app.set_input("hello".to_string());
    apply(&mut app, ctrl_key('a'));
    assert_eq!(app.cursor_pos, 0);
    apply(&mut app, ctrl_key('e'));
    assert_eq!(app.cursor_pos, 5);
    apply(&mut app, ctrl_key('u'));
    assert_eq!(app.input_buf, "");
    assert_eq!(app.cursor_pos, 0);
}

#[test]
fn apply_ctrl_c_signals_pending_cancel() {
    let mut app = ReplApp::new("demo", "u");
    apply(&mut app, ctrl_key('c'));
    assert!(app.pending_cancel);
}

/// Direct port of tagent's real `KeyCode::Char('d')` arm: Ctrl-D only quits
/// on an EMPTY input buffer (the readline EOF convention).
#[test]
fn apply_ctrl_d_signals_quit_when_input_empty() {
    let mut app = ReplApp::new("demo", "u");
    assert!(!app.should_quit());
    apply(&mut app, ctrl_key('d'));
    assert!(app.should_quit());
}

/// Direct port of tagent's real `KeyCode::Char('d')` arm: with text still in
/// the buffer, Ctrl-D is a no-op (tagent has no forward-delete fallback) —
/// pins the gap this slice's audit found and fixed (an earlier revision
/// quit unconditionally, losing unsaved input on a stray Ctrl-D).
#[test]
fn apply_ctrl_d_is_noop_when_input_nonempty() {
    let mut app = ReplApp::new("demo", "u");
    app.set_input("still typing".to_string());
    apply(&mut app, ctrl_key('d'));
    assert!(!app.should_quit());
    assert_eq!(app.input_buf, "still typing");
}

/// `ReplEvent::Quit` (synthesized by `crate::run::run`'s dispatch step when
/// `TuiEngine::handle_input` returns `Ok(false)`) must set `ReplApp::quit`
/// exactly like Ctrl-D does — the two are independent triggers for the same
/// state.
#[test]
fn apply_quit_event_signals_quit() {
    let mut app = ReplApp::new("demo", "u");
    assert!(!app.should_quit());
    apply(&mut app, ReplEvent::Quit);
    assert!(app.should_quit());
}

/// `ReplEvent::TurnFinished` (the `dispatch_pending` stuck-`busy` safety net)
/// must clear `busy`/`streaming_idx` and touch NOTHING else — in particular
/// it must never push a chat entry, which is exactly why it exists instead
/// of reusing an empty `AssistantOutput { done: true, .. }` — PROVIDED its
/// `generation` matches `app.current_generation` (the live turn).
#[test]
fn apply_turn_finished_clears_busy_and_streaming_idx_without_touching_chat() {
    let mut app = ReplApp::new("demo", "u");
    app.busy = true;
    app.streaming_idx = Some(0);
    app.current_generation = 1;
    app.push_status("unrelated"); // pre-existing chat content must survive
    let chat_before = app.chat.len();

    apply(&mut app, ReplEvent::TurnFinished { generation: 1 });

    assert!(!app.busy);
    assert!(app.streaming_idx.is_none());
    assert_eq!(
        app.chat.len(),
        chat_before,
        "must not push/alter any chat entry"
    );
}

/// The TOCTOU-race fix (PR #3477, final re-review round): a `TurnFinished`
/// whose `generation` is STALE relative to `app.current_generation` (a
/// newer turn is now live — e.g. the old turn was cancelled and the user
/// already resubmitted) must be a complete no-op. Without this guard, a
/// terminal signal that raced a cancel+resubmit could clear `busy`/
/// `streaming_idx` out from under a genuinely in-flight NEWER turn,
/// reintroducing the streaming-splice corruption class this whole PR
/// closes. This is a deterministic reducer-level test — the point of the
/// by-construction fix is that this comparison is the reducer's own logic,
/// not a live cross-thread race that would need reproducing.
#[test]
fn apply_turn_finished_with_stale_generation_is_ignored() {
    let mut app = ReplApp::new("demo", "u");
    app.busy = true;
    app.streaming_idx = Some(0);
    app.current_generation = 2; // a newer turn is now live

    apply(&mut app, ReplEvent::TurnFinished { generation: 1 }); // stale

    assert!(
        app.busy,
        "a stale TurnFinished must not clear busy for a newer in-flight turn"
    );
    assert_eq!(
        app.streaming_idx,
        Some(0),
        "a stale TurnFinished must not reset streaming_idx either"
    );
}

#[test]
fn apply_page_up_and_down_scroll_by_page() {
    let mut app = ReplApp::new("demo", "u");
    app.last_max_scroll
        .store(100, std::sync::atomic::Ordering::Relaxed);
    apply(&mut app, key(KeyCode::PageUp));
    assert_eq!(app.scroll_offset, PAGE_SCROLL as usize);
    apply(&mut app, key(KeyCode::PageDown));
    assert_eq!(app.scroll_offset, 0);
}

#[test]
fn apply_scroll_event_delegates_to_scroll() {
    let mut app = ReplApp::new("demo", "u");
    app.last_max_scroll
        .store(10, std::sync::atomic::Ordering::Relaxed);
    apply(&mut app, ReplEvent::Scroll(-5));
    assert_eq!(app.scroll_offset, 5);
}

#[test]
fn apply_cancel_event_signals_pending_cancel() {
    let mut app = ReplApp::new("demo", "u");
    apply(&mut app, ReplEvent::Cancel);
    assert!(app.pending_cancel);
}

#[test]
fn apply_assistant_output_streams_into_one_entry() {
    let mut app = ReplApp::new("demo", "u");
    apply(
        &mut app,
        ReplEvent::AssistantOutput {
            chunk: "Hel".into(),
            done: false,
            is_error: false,
        },
    );
    assert!(app.busy);
    assert_eq!(app.chat.len(), 1);
    apply(
        &mut app,
        ReplEvent::AssistantOutput {
            chunk: "lo".into(),
            done: true,
            is_error: false,
        },
    );
    assert!(!app.busy);
    assert_eq!(app.chat.len(), 1, "must accumulate into one entry");
    assert_eq!(app.chat[0].text, "Hello");
    assert!(app.streaming_idx.is_none());
}

#[test]
fn apply_assistant_output_finalizes_as_error_role() {
    use crate::app::ChatRole;
    let mut app = ReplApp::new("demo", "u");
    apply(
        &mut app,
        ReplEvent::AssistantOutput {
            chunk: "boom".into(),
            done: true,
            is_error: true,
        },
    );
    assert_eq!(app.chat[0].role, ChatRole::Error);
}

/// #4596: a call's start opens one card and its completion (same `id`, `Null`
/// args, as trusty-code sends it) fills that card instead of pushing a second
/// entry.
#[test]
fn tool_invocation_result_merges_into_its_call_card() {
    use crate::app::{ChatRole, ToolCard};
    let mut app = ReplApp::new("demo", "u");
    apply(
        &mut app,
        ReplEvent::ToolInvocation {
            id: "call-1".into(),
            agent_id: String::new(),
            tool_name: "git.checkout".into(),
            args: serde_json::json!("main"),
            result: None,
        },
    );
    assert_eq!(app.chat.len(), 1);
    assert_eq!(app.chat[0].role, ChatRole::Status);
    assert_eq!(app.tool_cards.get("call-1"), Some(&0));
    apply(
        &mut app,
        ReplEvent::ToolInvocation {
            id: "call-1".into(),
            agent_id: String::new(),
            tool_name: "git.checkout".into(),
            args: serde_json::Value::Null,
            result: Some("switched to main".into()),
        },
    );
    assert_eq!(app.chat.len(), 1, "one card, not two entries");
    assert_eq!(
        app.chat[0].tool,
        Some(ToolCard {
            id: "call-1".into(),
            tool_name: "git.checkout".into(),
            args: serde_json::json!("main"),
            result: Some("switched to main".into()),
        })
    );
    assert!(
        app.tool_cards.is_empty(),
        "a completed call releases its key"
    );
}

/// A completion for a call this client never saw start (a TUI attached
/// mid-call) still shows, as its own completed card.
#[test]
fn tool_invocation_result_without_a_start_opens_its_own_card() {
    let mut app = ReplApp::new("demo", "u");
    apply(
        &mut app,
        ReplEvent::ToolInvocation {
            id: "call-9".into(),
            agent_id: String::new(),
            tool_name: "bash".into(),
            args: serde_json::Value::Null,
            result: Some("done".into()),
        },
    );
    let card = app.chat[0].tool.as_ref().expect("a card");
    assert_eq!(card.result.as_deref(), Some("done"));
    assert!(app.tool_cards.is_empty());
}

#[test]
fn apply_status_message_and_clear_scrollback() {
    let mut app = ReplApp::new("demo", "u");
    apply(&mut app, ReplEvent::StatusMessage("hi".into()));
    assert_eq!(app.chat.len(), 1);
    apply(&mut app, ReplEvent::ClearScrollback);
    assert!(app.chat.is_empty());
}

#[test]
fn apply_connection_lost_pushes_status() {
    let mut app = ReplApp::new("demo", "u");
    apply(
        &mut app,
        ReplEvent::ConnectionLost {
            reason: "timeout".into(),
        },
    );
    assert_eq!(app.chat[0].text, "Connection lost: timeout");
}

#[test]
fn apply_statusline_update_replaces_segments() {
    let mut app = ReplApp::new("demo", "u");
    apply(
        &mut app,
        ReplEvent::StatuslineUpdate(vec![StatuslineSegment::SessionId("s1".into())]),
    );
    assert_eq!(app.statusline.len(), 1);
}

#[test]
fn apply_workstream_updated_sets_active_workstream() {
    let mut app = ReplApp::new("demo", "u");
    apply(
        &mut app,
        ReplEvent::WorkstreamUpdated(WorkstreamSummary {
            id: "a1".into(),
            name: "Token rotation".into(),
        }),
    );
    assert_eq!(app.active_workstream.unwrap().name, "Token rotation");
}

#[test]
fn apply_workstream_activation_changed_some_is_a_deliberate_noop() {
    // `Some(new_id)` alone names an id with no display name — the engine's
    // subsequent `WorkstreamUpdated` (asserted separately above) is what
    // actually sets the displayed workstream in this case, not this event.
    let mut app = ReplApp::new("demo", "u");
    apply(
        &mut app,
        ReplEvent::WorkstreamActivationChanged {
            new_active_id: Some("a1".into()),
            prior_id: None,
        },
    );
    assert!(app.active_workstream.is_none());
    assert!(app.chat.is_empty());
}

/// Regression test (DOC-50 §5.3, Slice 6): a `WorkstreamActivationChanged`
/// with `new_active_id: None` (the workstream was deactivated with no
/// replacement, DOC-48 §4.2/§4.3) must clear `app.active_workstream`
/// immediately. `ReplEvent::WorkstreamUpdated`'s payload is a concrete
/// `WorkstreamSummary`, so it can never represent "no active workstream" —
/// this event, and only this event, is what clears the indicator. Before
/// this fix, `WorkstreamActivationChanged` was treated as an unconditional
/// no-op for EVERY `new_active_id` value, so a deactivation left the status
/// line showing the stale, no-longer-active workstream forever.
#[test]
fn apply_workstream_activation_changed_none_clears_active_workstream() {
    let mut app = ReplApp::new("demo", "u");
    app.active_workstream = Some(WorkstreamSummary {
        id: "a1".into(),
        name: "Token rotation".into(),
    });

    apply(
        &mut app,
        ReplEvent::WorkstreamActivationChanged {
            new_active_id: None,
            prior_id: Some("a1".into()),
        },
    );

    assert!(
        app.active_workstream.is_none(),
        "deactivation must clear active_workstream, not leave the stale summary in place"
    );
}

#[test]
fn apply_resize_is_a_noop() {
    let mut app = ReplApp::new("demo", "u");
    let before = app.clone();
    apply(&mut app, ReplEvent::Resize(80, 24));
    assert_eq!(app.chat.len(), before.chat.len());
    assert_eq!(app.input_buf, before.input_buf);
}

/// Behavior-preserving port of tagent's
/// `push_assistant_trims_surrounding_blanks`
/// (`crates/trusty-agents/src/repl/tui/tests_state.rs`).
#[test]
fn push_assistant_trims_surrounding_blanks() {
    let mut app = ReplApp::new("demo", "u");
    app.push_assistant("\n\n2 + 2 = 4.\n\n   No tools needed.\n\n\n", false);
    assert_eq!(app.chat.len(), 1);
    assert_eq!(app.chat[0].text, "2 + 2 = 4.\n   No tools needed.");
}

/// Direct port of tagent's `repl_app_last_bash_block_updates_on_push`.
#[test]
fn push_assistant_updates_last_bash_block() {
    let mut app = ReplApp::new("demo", "u");
    assert_eq!(app.last_bash_block, None);
    app.push_assistant("Try `ls`:\n```bash\nls -la\n```", false);
    assert_eq!(app.last_bash_block, Some("ls -la".into()));
    app.push_assistant("Here:\n```sh\npwd\n```", false);
    assert_eq!(app.last_bash_block, Some("pwd".into()));
}

/// Direct port of tagent's `repl_app_last_bash_block_skips_errors`.
#[test]
fn push_assistant_skips_error_entries_for_bash_block() {
    let mut app = ReplApp::new("demo", "u");
    app.push_assistant("```bash\nls\n```", false);
    app.push_assistant("```bash\nrm -rf /\n```", true);
    assert_eq!(app.last_bash_block, Some("ls".into()));
}

/// A streamed (not single-push) assistant response must refresh
/// `last_bash_block` on finalize exactly like `push_assistant` does —
/// otherwise Ctrl-E's paste buffer goes stale for every streaming
/// engine (the MVP-required "streaming input/output" case).
#[test]
fn apply_assistant_output_refreshes_last_bash_block_on_finalize() {
    let mut app = ReplApp::new("demo", "u");
    apply(
        &mut app,
        ReplEvent::AssistantOutput {
            chunk: "```bash\n".into(),
            done: false,
            is_error: false,
        },
    );
    assert_eq!(
        app.last_bash_block, None,
        "must not update mid-stream, only on finalize"
    );
    apply(
        &mut app,
        ReplEvent::AssistantOutput {
            chunk: "echo hi\n```".into(),
            done: true,
            is_error: false,
        },
    );
    assert_eq!(app.last_bash_block, Some("echo hi".into()));
}

// ── #7940: delegation rendering and per-agent stream keying ───────────────

/// Open a delegation block for `agent_id`/`agent` and return the app.
fn app_with_delegation(agent_id: &str, agent: &str) -> ReplApp {
    let mut app = ReplApp::new("demo", "u");
    apply(
        &mut app,
        ReplEvent::DelegationStarted {
            agent_id: agent_id.into(),
            agent: agent.into(),
            task: "do the thing".into(),
        },
    );
    app
}

/// One chunk of attributed output.
fn agent_output(agent_id: &str, turn_id: &str, chunk: &str, done: bool) -> ReplEvent {
    ReplEvent::AgentOutput {
        agent_id: agent_id.into(),
        turn_id: turn_id.into(),
        chunk: chunk.into(),
        done,
    }
}

/// THE regression this slice exists to close: a primary agent and a
/// delegated sub-agent streaming inside one human turn must land in two
/// separate bubbles, not one interleaved bubble. Driven with the two streams
/// alternating chunk-by-chunk, which is what the unkeyed
/// `ReplApp::streaming_idx` path produced `"pm-1sub-1 pm-2 sub-2"` from.
#[test]
fn agent_output_keys_concurrent_streams_separately() {
    let mut app = ReplApp::new("demo", "u");
    apply(&mut app, agent_output("pm-1", "turn-a", "pm-1", false));
    apply(&mut app, agent_output("eng-1", "turn-b", "sub-1", false));
    apply(&mut app, agent_output("pm-1", "turn-a", " pm-2", false));
    apply(&mut app, agent_output("eng-1", "turn-b", " sub-2", false));

    assert_eq!(app.chat.len(), 2, "one bubble per (agent_id, turn_id)");
    assert_eq!(app.chat[0].text, "pm-1 pm-2");
    assert_eq!(app.chat[1].text, "sub-1 sub-2");
}

/// Two agents sharing ONE `turn_id` must still get their own bubbles.
///
/// Why: this is the case that actually pins the `agent_id` half of the key.
/// `agent_output_keys_concurrent_streams_separately` above varies BOTH halves,
/// so it passes even if the key collapses to `turn_id` alone — a code-critic
/// pass on this branch proved that by mutation. The delta contract
/// (`trusty_code::events::Event::AgentMessageDelta`) requires consumers to
/// group by the PAIR precisely because a producer can get session-global
/// `turn_id` uniqueness wrong; this test is the defense against that, and it
/// is red against a `turn_id`-only key.
#[test]
fn agent_output_keys_shared_turn_id_by_agent_id() {
    let mut app = ReplApp::new("demo", "u");
    apply(&mut app, agent_output("pm-1", "turn-shared", "pm-1", false));
    apply(
        &mut app,
        agent_output("eng-1", "turn-shared", "sub-1", false),
    );
    apply(
        &mut app,
        agent_output("pm-1", "turn-shared", " pm-2", false),
    );
    apply(
        &mut app,
        agent_output("eng-1", "turn-shared", " sub-2", false),
    );

    assert_eq!(
        app.chat.len(),
        2,
        "two agents sharing a turn_id must not collapse into one bubble: {:?}",
        app.chat.iter().map(|c| &c.text).collect::<Vec<_>>()
    );
    assert_eq!(app.chat[0].text, "pm-1 pm-2");
    assert_eq!(app.chat[1].text, "sub-1 sub-2");
}

/// Two turns from the SAME agent must also get their own bubbles — the key
/// is the pair, not the agent id alone.
#[test]
fn agent_output_keys_distinct_turns_of_one_agent_separately() {
    let mut app = ReplApp::new("demo", "u");
    apply(&mut app, agent_output("pm-1", "turn-a", "first", false));
    apply(&mut app, agent_output("pm-1", "turn-b", "second", false));
    assert_eq!(app.chat.len(), 2);
    assert_eq!(app.chat[0].text, "first");
    assert_eq!(app.chat[1].text, "second");
}

/// `done: true` closes the bubble, drops the key, and — unlike
/// `AssistantOutput` — never clears `busy`: one agent's turn ending is not
/// the human turn ending.
#[test]
fn agent_output_finalizes_and_drops_its_key() {
    let mut app = ReplApp::new("demo", "u");
    app.busy = true;
    apply(&mut app, agent_output("pm-1", "turn-a", "hello", false));
    apply(&mut app, agent_output("pm-1", "turn-a", "", true));
    assert!(app.agent_streams.is_empty(), "the key must be released");
    assert!(
        app.busy,
        "an agent turn ending is not the human turn ending"
    );
    // A later delta for the same key opens a NEW bubble rather than
    // reopening the finalized one.
    apply(&mut app, agent_output("pm-1", "turn-a", "again", false));
    assert_eq!(app.chat.len(), 2);
}

/// A terminal chunk for a stream this client never saw must not push a
/// permanently blank bubble.
#[test]
fn agent_output_terminal_chunk_for_unknown_stream_is_dropped() {
    let mut app = ReplApp::new("demo", "u");
    apply(&mut app, agent_output("pm-1", "turn-a", "", true));
    assert!(app.chat.is_empty());
}

/// Output attributed to an OPEN delegation renders inside that block;
/// output from an unknown id is ordinary assistant output.
#[test]
fn agent_output_inside_a_delegation_is_delegated_role() {
    let mut app = app_with_delegation("eng-1", "engineer");
    apply(&mut app, agent_output("eng-1", "turn-b", "working", false));
    apply(&mut app, agent_output("pm-1", "turn-a", "thinking", false));
    assert_eq!(app.chat[1].role, ChatRole::Delegated);
    assert_eq!(app.chat[2].role, ChatRole::Assistant);
}

/// An empty `agent_id` carries no attribution and must never be filed under
/// whichever delegation happens to be open.
#[test]
fn agent_output_with_no_attribution_is_not_delegated() {
    let mut app = app_with_delegation("", "engineer");
    apply(&mut app, agent_output("", "turn-a", "unattributed", false));
    assert_eq!(app.chat[1].role, ChatRole::Assistant);
}

#[test]
fn delegation_started_pushes_header_and_sets_active_agent() {
    let app = app_with_delegation("eng-1", "engineer");
    assert_eq!(app.chat[0].role, ChatRole::Delegation);
    assert_eq!(app.chat[0].text, "▶ engineer — do the thing");
    assert_eq!(app.active_agent(), Some("engineer"));
}

/// A producer that announces the delegation before the sub-agent exists (no
/// `agent_id`) and then reports the real spawn must render ONE block, with
/// the real id adopted so later attributed events file under it.
#[test]
fn delegation_started_with_id_adopts_an_announced_block() {
    let mut app = app_with_delegation("", "engineer");
    apply(
        &mut app,
        ReplEvent::DelegationStarted {
            agent_id: "eng-1".into(),
            agent: "engineer".into(),
            task: "do the thing".into(),
        },
    );
    assert_eq!(app.chat.len(), 1, "one block, not two");
    assert_eq!(app.delegations.len(), 1);
    assert_eq!(app.delegations[0].agent_id, "eng-1");
    apply(&mut app, agent_output("eng-1", "turn-b", "working", false));
    assert_eq!(app.chat[1].role, ChatRole::Delegated);
}

/// Two concurrent delegations to the SAME agent name but distinct ids are
/// two genuine delegations and get two blocks.
#[test]
fn delegation_started_twice_for_distinct_ids_opens_two_blocks() {
    let mut app = app_with_delegation("eng-1", "engineer");
    apply(
        &mut app,
        ReplEvent::DelegationStarted {
            agent_id: "eng-2".into(),
            agent: "engineer".into(),
            task: "the other thing".into(),
        },
    );
    assert_eq!(app.delegations.len(), 2);
    assert_eq!(app.chat.len(), 2);
}

#[test]
fn delegation_finished_pushes_footer_and_clears_active_agent() {
    let mut app = app_with_delegation("eng-1", "engineer");
    apply(
        &mut app,
        ReplEvent::DelegationFinished {
            agent_id: "eng-1".into(),
            agent: "engineer".into(),
            outcome: DelegationOutcome::Finished("success".into()),
        },
    );
    assert_eq!(app.chat[1].role, ChatRole::Delegation);
    assert_eq!(app.chat[1].text, "└ engineer — success");
    assert_eq!(app.active_agent(), None);
}

#[test]
fn delegation_finished_failed_footer_carries_the_error() {
    let mut app = app_with_delegation("eng-1", "engineer");
    apply(
        &mut app,
        ReplEvent::DelegationFinished {
            agent_id: "eng-1".into(),
            agent: "engineer".into(),
            outcome: DelegationOutcome::Failed("turn cap exceeded".into()),
        },
    );
    assert_eq!(app.chat[1].text, "└ engineer — failed: turn cap exceeded");
}

/// A failed loop aborts mid-turn and never sends its terminal chunk;
/// closing the block must release the key so the map stays bounded.
#[test]
fn delegation_finished_drops_an_unterminated_stream_key() {
    let mut app = app_with_delegation("eng-1", "engineer");
    apply(
        &mut app,
        agent_output("eng-1", "turn-b", "half a th", false),
    );
    assert_eq!(app.agent_streams.len(), 1);
    apply(
        &mut app,
        ReplEvent::DelegationFinished {
            agent_id: "eng-1".into(),
            agent: "engineer".into(),
            outcome: DelegationOutcome::Failed("boom".into()),
        },
    );
    assert!(app.agent_streams.is_empty());
}

/// A delegated sub-agent's tool cards belong inside its block; the primary
/// agent's stay top-level (#7940, #4596).
#[test]
fn tool_invocation_attributed_to_a_delegation_is_delegated_role() {
    let mut app = app_with_delegation("eng-1", "engineer");
    apply(
        &mut app,
        ReplEvent::ToolInvocation {
            id: "c1".into(),
            agent_id: "eng-1".into(),
            tool_name: "bash".into(),
            args: serde_json::json!("cargo test"),
            result: None,
        },
    );
    apply(
        &mut app,
        ReplEvent::ToolInvocation {
            id: "c2".into(),
            agent_id: "pm-1".into(),
            tool_name: "delegate_to_agent".into(),
            args: serde_json::json!("{}"),
            result: None,
        },
    );
    let name = |i: usize| app.chat[i].tool.as_ref().map(|c| c.tool_name.as_str());
    assert_eq!(app.chat[1].role, ChatRole::Delegated);
    assert_eq!(name(1), Some("bash"));
    assert_eq!(app.chat[2].role, ChatRole::Status);
    assert_eq!(name(2), Some("delegate_to_agent"));
}

/// A delegated call whose completion arrives after its block closed still
/// fills the card it opened inside that block (#4596, #7940).
#[test]
fn tool_invocation_completion_after_delegation_closes_fills_the_delegated_card() {
    let mut app = app_with_delegation("eng-1", "engineer");
    apply(
        &mut app,
        ReplEvent::ToolInvocation {
            id: "c1".into(),
            agent_id: "eng-1".into(),
            tool_name: "bash".into(),
            args: serde_json::json!("cargo test"),
            result: None,
        },
    );
    apply(
        &mut app,
        ReplEvent::DelegationFinished {
            agent_id: "eng-1".into(),
            agent: "engineer".into(),
            outcome: DelegationOutcome::Finished("success".into()),
        },
    );
    apply(
        &mut app,
        ReplEvent::ToolInvocation {
            id: "c1".into(),
            agent_id: "eng-1".into(),
            tool_name: "bash".into(),
            args: serde_json::Value::Null,
            result: Some("ok".into()),
        },
    );
    let cards: Vec<usize> = (0..app.chat.len())
        .filter(|&i| app.chat[i].tool.as_ref().is_some_and(|c| c.id == "c1"))
        .collect();
    assert_eq!(cards, vec![1], "exactly one card for c1: {:?}", app.chat);
    assert_eq!(app.chat[1].role, ChatRole::Delegated);
    let card = app.chat[1].tool.as_ref().expect("a card");
    assert_eq!(card.result.as_deref(), Some("ok"));
    assert_eq!(card.args, serde_json::json!("cargo test"));
    assert!(app.tool_cards.is_empty());
}

/// A repeated start for a call whose card is still open changes nothing —
/// no second entry, no re-keying, and the first start's args survive.
#[test]
fn tool_invocation_repeated_start_for_an_open_card_is_a_noop() {
    let mut app = ReplApp::new("demo", "u");
    apply(
        &mut app,
        ReplEvent::ToolInvocation {
            id: "c1".into(),
            agent_id: String::new(),
            tool_name: "bash".into(),
            args: serde_json::json!("cargo test"),
            result: None,
        },
    );
    let chat_len = app.chat.len();
    let tool_cards = app.tool_cards.clone();
    let args = app.chat[0].tool.as_ref().expect("a card").args.clone();
    apply(
        &mut app,
        ReplEvent::ToolInvocation {
            id: "c1".into(),
            agent_id: String::new(),
            tool_name: "bash".into(),
            args: serde_json::json!("cargo build"),
            result: None,
        },
    );
    assert_eq!(app.chat.len(), chat_len);
    assert_eq!(app.tool_cards, tool_cards);
    let card = app.chat[0].tool.as_ref().expect("a card");
    assert_eq!(card.args, args);
    assert_eq!(card.result, None);
}

/// `/clear` must drop the keyed-stream indices too — they index INTO `chat`,
/// so leaving them behind would point at rows that no longer exist.
#[test]
fn clear_scrollback_drops_delegation_state() {
    let mut app = app_with_delegation("eng-1", "engineer");
    apply(&mut app, agent_output("eng-1", "turn-b", "working", false));
    apply(
        &mut app,
        ReplEvent::ToolInvocation {
            id: "c1".into(),
            agent_id: "eng-1".into(),
            tool_name: "bash".into(),
            args: serde_json::json!("cargo test"),
            result: None,
        },
    );
    apply(&mut app, ReplEvent::ClearScrollback);
    assert!(app.chat.is_empty());
    assert!(app.agent_streams.is_empty());
    assert!(app.tool_cards.is_empty());
    assert_eq!(app.active_agent(), None);
}

// ── Permission prompt (#3422) ─────────────────────────────────────────────

fn permission_requested(request_id: &str) -> ReplEvent {
    ReplEvent::PermissionRequested {
        request_id: request_id.into(),
        agent: "python-engineer".into(),
        agent_id: "spawn-1".into(),
        tool: "bash".into(),
        subject: "rm -rf build".into(),
        rule: "bash[rm *]".into(),
    }
}

fn permission_resolved(request_id: &str, decision: &str, source: &str) -> ReplEvent {
    ReplEvent::PermissionResolved {
        request_id: request_id.into(),
        agent: "python-engineer".into(),
        agent_id: "spawn-1".into(),
        decision: decision.into(),
        source: source.into(),
    }
}

fn app_with_prompt() -> ReplApp {
    let mut app = ReplApp::new("demo", "u");
    apply(&mut app, permission_requested("req-1"));
    app
}

/// The request opens the prompt AND lands in the scrollback — the prompt is
/// transient, the transcript entry is not.
#[test]
fn permission_requested_opens_a_prompt_and_records_it() {
    let app = app_with_prompt();
    let pending = app.pending_permission.as_ref().expect("prompt is open");
    assert_eq!(pending.request_id, "req-1");
    assert_eq!(pending.tool, "bash");
    assert_eq!(pending.subject, "rm -rf build");
    assert_eq!(pending.rule, "bash[rm *]");
    assert_eq!(app.chat.len(), 1);
    assert_eq!(app.chat[0].role, ChatRole::Status);
    assert!(app.chat[0].text.contains("bash"), "{}", app.chat[0].text);
    assert!(
        app.chat[0].text.contains("rm -rf build"),
        "{}",
        app.chat[0].text
    );
    assert!(
        app.chat[0].text.contains("bash[rm *]"),
        "{}",
        app.chat[0].text
    );
}

/// A second request replaces the first rather than stacking — see
/// `super::apply_permission_requested`'s doc comment.
#[test]
fn permission_requested_twice_keeps_only_the_newest_prompt() {
    let mut app = app_with_prompt();
    apply(&mut app, permission_requested("req-2"));
    assert_eq!(
        app.pending_permission.as_ref().expect("prompt").request_id,
        "req-2"
    );
    assert_eq!(app.chat.len(), 2, "both requests are still recorded");
}

/// Criterion 3: requested -> resolved leaves BOTH entries in the scrollback
/// and no pending prompt.
#[test]
fn permission_resolved_clears_the_prompt_and_records_the_decision() {
    let mut app = app_with_prompt();
    apply(
        &mut app,
        permission_resolved("req-1", "allow_once", "client"),
    );
    assert!(app.pending_permission.is_none());
    assert_eq!(app.chat.len(), 2);
    assert!(
        app.chat[0].text.contains("rm -rf build"),
        "the request: {}",
        app.chat[0].text
    );
    assert!(
        app.chat[1].text.contains("allow_once") && app.chat[1].text.contains("client"),
        "the decision: {}",
        app.chat[1].text
    );
}

/// A resolution for some OTHER request is recorded but must not unblock the
/// prompt the user is looking at — an auto-allowed call elsewhere in the run
/// resolves under a request id this client never saw opened.
#[test]
fn permission_resolved_for_another_request_leaves_the_prompt_pending() {
    let mut app = app_with_prompt();
    apply(
        &mut app,
        permission_resolved("req-other", "allow_once", "remembered"),
    );
    assert_eq!(
        app.pending_permission
            .as_ref()
            .expect("still open")
            .request_id,
        "req-1"
    );
    assert_eq!(app.chat.len(), 2, "the other decision is still recorded");
}

/// Criterion 1: a submitted line is a no-op while a prompt is pending, and
/// the prompt is untouched by the attempt.
#[test]
fn submit_line_is_noop_while_a_permission_prompt_is_pending() {
    let mut app = app_with_prompt();
    let before = app.pending_permission.clone();
    let chat_len = app.chat.len();

    apply(&mut app, ReplEvent::Submit("run the thing".to_string()));

    assert_eq!(app.pending_submit, None, "no turn may be staged");
    assert!(!app.busy, "no turn may start");
    assert_eq!(app.chat.len(), chat_len, "nothing echoed to the scrollback");
    assert_eq!(app.pending_permission, before, "prompt state unchanged");
}

/// Enter is INERT while a prompt is open (#3422): it neither submits the
/// typed line nor answers the prompt. An Enter bound to allow-once would
/// grant a permission the footer never advertises — an operator reaches for
/// Enter to send the line they were typing, not to approve `rm -rf build`.
#[test]
fn enter_while_a_permission_prompt_is_pending_does_not_submit() {
    let mut app = app_with_prompt();
    app.set_input("run the thing".to_string());

    apply(&mut app, key(KeyCode::Enter));

    assert_eq!(app.pending_submit, None, "no turn may be staged");
    assert_eq!(
        app.input_buf, "run the thing",
        "the typed line is preserved"
    );
    assert!(
        app.pending_permission.is_some(),
        "Enter answers nothing — the prompt must still be open"
    );
    assert_eq!(
        app.pending_permission_response, None,
        "Enter must stage no answer at all"
    );
}

/// Criterion 2, key 1 of 3.
#[test]
fn permission_key_y_answers_allow_once() {
    let mut app = app_with_prompt();
    apply(&mut app, key(KeyCode::Char('y')));
    let response = app
        .take_pending_permission_response()
        .expect("an answer is staged");
    assert_eq!(response.request_id, "req-1");
    assert_eq!(response.answer, PermissionAnswer::AllowOnce);
    assert!(
        app.pending_permission.is_none(),
        "the prompt releases input"
    );
}

/// Criterion 2, key 2 of 3. `pattern` is `None` because choosing a grant
/// width is the backend's policy, not the TUI's (ADR-0063).
#[test]
fn permission_key_a_answers_allow_for_session() {
    let mut app = app_with_prompt();
    apply(&mut app, key(KeyCode::Char('a')));
    let response = app
        .take_pending_permission_response()
        .expect("an answer is staged");
    assert_eq!(response.request_id, "req-1");
    assert_eq!(
        response.answer,
        PermissionAnswer::AllowForSession { pattern: None }
    );
}

/// Criterion 2, key 3 of 3.
#[test]
fn permission_key_n_answers_deny() {
    let mut app = app_with_prompt();
    apply(&mut app, key(KeyCode::Char('n')));
    let response = app
        .take_pending_permission_response()
        .expect("an answer is staged");
    assert_eq!(response.request_id, "req-1");
    assert_eq!(response.answer, PermissionAnswer::Deny);
}

/// Escape is the second deny binding — #3422's original proposal named it.
#[test]
fn permission_key_escape_answers_deny() {
    let mut app = app_with_prompt();
    apply(&mut app, key(KeyCode::Esc));
    assert_eq!(
        app.take_pending_permission_response()
            .expect("an answer is staged")
            .answer,
        PermissionAnswer::Deny
    );
}

/// Criterion 2: an unbound key leaves the prompt pending and stages nothing
/// — never a guessed allow or deny.
#[test]
fn permission_unbound_key_leaves_the_prompt_pending() {
    let mut app = app_with_prompt();
    for unbound in [key(KeyCode::Char('q')), key(KeyCode::Tab), ctrl_key('c')] {
        apply(&mut app, unbound);
    }
    assert!(
        app.pending_permission.is_some(),
        "the prompt must still be open"
    );
    assert_eq!(app.pending_permission_response, None);
    assert!(
        !app.pending_cancel,
        "Ctrl-C is swallowed by the modal prompt"
    );
    assert!(app.input_buf.is_empty(), "no key reached the line editor");
}
