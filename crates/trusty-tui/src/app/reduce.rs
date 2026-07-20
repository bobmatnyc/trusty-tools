//! [`apply`] — the `ReplApp` half of [`crate::run::event_loop`]'s
//! `apply: FnMut(&mut M, ReplEvent)` contract.
//!
//! Why: DOC-50 §5 Slice 5 ("Event dispatch and line editing") is scoped as
//! its own future slice, but [`crate::run::event_loop`] (already on main
//! since Slice 2) needs *some* `apply` closure to be renderable at all — a
//! product cannot demonstrate the Slice 4 widgets against real key input
//! without one. This module provides the generic core: character
//! insertion/deletion, cursor movement, history recall, and scroll —
//! the same primitives tagent's `keys.rs`/`events.rs` already proved out,
//! minus the tagent-specific extras (pickers, slash-completion, cost/token
//! tracking) that stay behind per the generalization mandate. Full
//! line-editing polish (Ctrl-w word-delete, kill-ring, etc.) remains Slice
//! 5's to design.
//!
//! What: `apply` cannot reach the event channel or the engine — it only has
//! `&mut ReplApp` — so anything that would normally need to call
//! `TuiEngine::handle_input`/`cancel_session` instead stages a signal on
//! `ReplApp` ([`ReplApp::pending_submit`], [`ReplApp::pending_cancel`]) for
//! the caller to drain after `apply` returns, mirroring tagent's own
//! `pending_picker_selection`/`pending_cancel` precedent
//! (`crates/trusty-agents/src/repl/tui/types.rs`).
//!
//! # Spec References
//! - [`SPEC-TTUI-05~draft`](../../../docs/specs/DOC-50-tcode-tui-claude-code-clone.md#SPEC-TTUI-05~draft) — Slice 4 deliverable (§5, Slice 4): wire `ReplApp` to `ReplEvent`.

use super::ReplApp;
use crate::event::{KeyCode, KeyInput, ReplEvent};
use crate::text::{strip_interior_blank_lines, trim_surrounding_blank_lines};

/// How many lines a Page-Up/Page-Down key press scrolls.
const PAGE_SCROLL: isize = 10;

/// Apply one [`ReplEvent`] to `app`, mutating it in place.
///
/// Why: a free function (rather than an `ReplApp` method) so it matches
/// `crate::run::event_loop`'s `apply: impl FnMut(&mut M, ReplEvent)` shape
/// exactly — a caller passes `trusty_tui::app::apply` directly as that
/// argument.
/// What: dispatches on every [`ReplEvent`] variant (see the module doc
/// comment for what's out of scope). `Key` events are further dispatched by
/// [`apply_key`].
/// Test: [`tests`] below cover every variant this function actually changes
/// state for.
pub fn apply(app: &mut ReplApp, ev: ReplEvent) {
    match ev {
        ReplEvent::Key(key) => apply_key(app, key),
        ReplEvent::Resize(_, _) => {
            // No-op: ratatui reads the real terminal size on every `draw`;
            // nothing in `ReplApp` caches a stale width/height to refresh.
        }
        ReplEvent::Scroll(delta) => app.scroll(delta),
        ReplEvent::Submit(line) => app.submit_line(line),
        ReplEvent::Cancel => app.pending_cancel = true,
        ReplEvent::AssistantOutput {
            chunk,
            done,
            is_error,
        } => apply_assistant_output(app, chunk, done, is_error),
        ReplEvent::ToolInvocation {
            id: _,
            tool_name,
            args,
            result,
        } => match result {
            None => app.push_status(format!("[TOOL] {tool_name}: {args}")),
            Some(r) => app.push_status(format!("[RESULT] {r}")),
        },
        ReplEvent::StatusMessage(msg) => app.push_status(msg),
        ReplEvent::ClearScrollback => app.clear_scrollback(),
        ReplEvent::StatuslineUpdate(segments) => app.statusline = segments,
        ReplEvent::WorkstreamUpdated(ws) => app.active_workstream = Some(ws),
        ReplEvent::WorkstreamActivationChanged { .. } => {
            // Intentional no-op: per `TuiEngine::subscribe_workstream_events`,
            // the engine follows this with a `WorkstreamUpdated` carrying the
            // full summary once it re-fetches — that's what actually changes
            // the displayed workstream. This event alone names an id with no
            // display name to show yet.
        }
        ReplEvent::ConnectionLost { reason } => {
            app.push_status(format!("Connection lost: {reason}"))
        }
    }
}

/// Accumulate one streamed assistant-output chunk into the in-progress chat
/// entry, finalizing (trim + role) it on `done`.
///
/// Why: split out of [`apply`] so the streaming state machine — "append to
/// the open entry, or open a new one" — reads as its own unit.
/// What: mirrors [`ReplApp::push_assistant`]'s trim/collapse pass, but only
/// on the *finished* text (trimming mid-stream would strip a blank line the
/// next chunk was about to fill back in).
/// Test: [`tests::apply_assistant_output_streams_into_one_entry`],
/// [`tests::apply_assistant_output_finalizes_as_error_role`].
fn apply_assistant_output(app: &mut ReplApp, chunk: String, done: bool, is_error: bool) {
    use crate::app::ChatRole;

    match app.streaming_idx {
        Some(idx) => {
            if let Some(entry) = app.chat.get_mut(idx) {
                entry.text.push_str(&chunk);
            }
        }
        None => {
            app.chat.push(super::ChatLine {
                role: ChatRole::Assistant,
                text: chunk,
            });
            app.streaming_idx = Some(app.chat.len() - 1);
        }
    }
    app.busy = !done;
    app.scroll_offset = 0;

    if done
        && let Some(idx) = app.streaming_idx.take()
        && let Some(entry) = app.chat.get_mut(idx)
    {
        let trimmed = trim_surrounding_blank_lines(&entry.text);
        entry.text = strip_interior_blank_lines(&trimmed);
        if is_error {
            entry.role = ChatRole::Error;
        }
    }
}

/// Dispatch one translated key press to the appropriate `ReplApp` mutator.
///
/// Why: split out of [`apply`] so the (long, mechanical) key-by-key match
/// reads as its own unit, matching tagent's `keys.rs::handle_key` precedent
/// in spirit (though deliberately smaller — see the module doc comment for
/// what's deferred to Slice 5).
/// What: printable chars insert; Backspace/Left/Right/Home/End edit/move;
/// Up/Down recall history; PageUp/PageDown scroll a page; Enter submits;
/// Ctrl-a/e/u/c/d match the readline bindings DOC-50 §5 Slice 5 specifies.
/// Any other key (Tab, Esc, Delete, `KeyCode::Other`) is a no-op — those are
/// slash-completion/picker-navigation concerns this slice doesn't own.
/// Test: [`tests`] below, one per binding.
fn apply_key(app: &mut ReplApp, key: KeyInput) {
    let ctrl = key.modifiers.ctrl;
    match key.code {
        KeyCode::Char(c) if ctrl => match c {
            'a' => app.cursor_pos = 0,
            'e' => app.cursor_pos = app.input_buf.len(),
            'u' => {
                app.input_buf.clear();
                app.cursor_pos = 0;
            }
            'c' => app.pending_cancel = true,
            'd' => app.quit = true,
            _ => {}
        },
        KeyCode::Char(c) => app.insert_char(c),
        KeyCode::Backspace => app.backspace(),
        KeyCode::Left => app.cursor_left(),
        KeyCode::Right => app.cursor_right(),
        KeyCode::Home => app.cursor_pos = 0,
        KeyCode::End => app.cursor_pos = app.input_buf.len(),
        KeyCode::Up => app.history_prev(),
        KeyCode::Down => app.history_next(),
        KeyCode::PageUp => app.scroll(-PAGE_SCROLL),
        KeyCode::PageDown => app.scroll(PAGE_SCROLL),
        KeyCode::Enter => {
            if let Some(line) = app.take_input() {
                app.submit_line(line);
            }
        }
        KeyCode::Delete | KeyCode::Tab | KeyCode::Esc | KeyCode::Other => {}
    }
}

#[cfg(test)]
mod tests {
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

    #[test]
    fn apply_up_down_recall_history() {
        let mut app = ReplApp::new("demo", "u");
        app.history = vec!["one".into(), "two".into(), "three".into()];
        app.insert_char('x');
        apply(&mut app, key(KeyCode::Up));
        assert_eq!(app.input_buf, "three");
        apply(&mut app, key(KeyCode::Up));
        assert_eq!(app.input_buf, "two");
        apply(&mut app, key(KeyCode::Down));
        assert_eq!(app.input_buf, "three");
        apply(&mut app, key(KeyCode::Down));
        assert_eq!(app.input_buf, "x");
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
                new_active_id: "a1".into(),
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
}
