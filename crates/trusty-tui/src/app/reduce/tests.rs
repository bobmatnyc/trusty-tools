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

#[test]
fn apply_ctrl_d_signals_quit() {
    let mut app = ReplApp::new("demo", "u");
    assert!(!app.should_quit());
    apply(&mut app, ctrl_key('d'));
    assert!(app.should_quit());
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

#[test]
fn apply_tool_invocation_renders_start_and_result_as_status() {
    let mut app = ReplApp::new("demo", "u");
    apply(
        &mut app,
        ReplEvent::ToolInvocation {
            id: "call-1".into(),
            tool_name: "git.checkout".into(),
            args: serde_json::json!("main"),
            result: None,
        },
    );
    assert_eq!(app.chat[0].text, "[TOOL] git.checkout: \"main\"");
    apply(
        &mut app,
        ReplEvent::ToolInvocation {
            id: "call-1".into(),
            tool_name: "git.checkout".into(),
            args: serde_json::json!("main"),
            result: Some("switched to main".into()),
        },
    );
    assert_eq!(app.chat[1].text, "[RESULT] switched to main");
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
fn apply_workstream_activation_changed_is_a_deliberate_noop() {
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
