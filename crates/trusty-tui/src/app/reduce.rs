//! [`apply`] — the `ReplApp` half of [`crate::run::event_loop`]'s
//! `apply: FnMut(&mut M, ReplEvent)` contract.
//!
//! Why: DOC-50 §5 Slice 5 ("Event dispatch and line editing") is scoped as
//! its own future slice, but [`crate::run::event_loop`] (already on main
//! since Slice 2) needs *some* `apply` closure to be renderable at all — a
//! product cannot demonstrate the Slice 4 widgets against real key input
//! without one. This module provides the generic core: character
//! insertion/deletion, cursor movement, history recall, and scroll. Where a
//! binding has a REAL tagent precedent (Up/Down, Ctrl-E), this module ports
//! that exact production behavior — verified against tagent's actual
//! `crates/trusty-agents/src/repl/tui/keys.rs`, not its doc comments, which
//! in at least one case (Up-arrow) misattribute production behavior to a
//! dead-code helper. See [`crate::app`]'s module doc comment for the full
//! disclosed-differences list. Tagent-specific extras (pickers,
//! slash-completion, cost/token tracking) stay behind per the
//! generalization mandate; full line-editing polish (Ctrl-w word-delete,
//! kill-ring, etc.) remains Slice 5's to design.
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
/// next chunk was about to fill back in). Also refreshes
/// [`ReplApp::last_bash_block`] on finalize, same as `push_assistant`
/// (Ctrl-E's paste buffer must not go stale just because a response arrived
/// via streaming instead of a single push).
/// Test: [`tests::apply_assistant_output_streams_into_one_entry`],
/// [`tests::apply_assistant_output_finalizes_as_error_role`],
/// [`tests::apply_assistant_output_refreshes_last_bash_block_on_finalize`].
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
        app.update_last_bash_block();
    }
}

/// Dispatch one translated key press to the appropriate `ReplApp` mutator.
///
/// Why: split out of [`apply`] so the (long, mechanical) key-by-key match
/// reads as its own unit, matching tagent's `keys.rs::handle_key` precedent
/// in spirit (though deliberately smaller — see the module doc comment for
/// what's deferred to Slice 5).
/// What: printable chars insert; Backspace/Left/Right/Home/End edit/move;
/// PageUp/PageDown scroll a page; Enter submits; Ctrl-a/u/c/d match the
/// readline bindings DOC-50 §5 Slice 5 specifies. Up, Down, and Ctrl-E are
/// direct ports of tagent's real `keys.rs` bindings rather than a Slice-5
/// invention — see [`apply_up`] and [`apply_ctrl_e`] for why they're pulled
/// into their own functions. Any other key (Tab, Esc, Delete,
/// `KeyCode::Other`) is a no-op — those are slash-completion/picker-
/// navigation concerns this slice doesn't own.
/// Test: [`tests`] below, one per binding.
fn apply_key(app: &mut ReplApp, key: KeyInput) {
    let ctrl = key.modifiers.ctrl;
    match key.code {
        KeyCode::Char(c) if ctrl => match c {
            'a' => app.cursor_pos = 0,
            'e' => apply_ctrl_e(app),
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
        KeyCode::Up => apply_up(app),
        // Direct port of tagent's real `KeyCode::Down` arm
        // (`crates/trusty-agents/src/repl/tui/keys.rs`), which calls
        // `history_next()` even though nothing (including `apply_up` below)
        // ever sets `history_idx` — a functional no-op today in tagent too.
        // Kept for fidelity per `crate::app`'s disclosure list, not because
        // it currently does anything observable.
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

/// Up-arrow: recall [`ReplApp::last_prompt`], and — while
/// [`ReplApp::busy`] — ALSO signal [`ReplApp::pending_cancel`].
///
/// Why: direct port of tagent's real `KeyCode::Up` arm
/// (`crates/trusty-agents/src/repl/tui/keys.rs`): a busy in-flight request
/// gets cancelled so the user can edit and resubmit, and the cancel signal
/// fires independent of whether `last_prompt` happens to be set (matching
/// tagent's unconditional `if app.thinking { app.pending_cancel = true; }`
/// ahead of the recall). This is NOT the multi-level `history_prev` browser
/// — see `crate::app`'s module doc comment for why that helper stays
/// unwired, exactly as it is in tagent.
/// What: no-ops the recall half when `last_prompt` is empty (nothing to
/// recall); the cancel signal is unconditional on `busy`.
/// Test: [`tests::apply_up_recalls_last_prompt_when_idle`],
/// [`tests::apply_up_signals_cancel_and_recalls_when_busy`],
/// [`tests::apply_up_signals_cancel_even_with_no_last_prompt`],
/// [`tests::apply_up_is_noop_when_idle_and_no_last_prompt`].
fn apply_up(app: &mut ReplApp) {
    if app.busy {
        app.pending_cancel = true;
    }
    if !app.last_prompt.is_empty() {
        let lp = app.last_prompt.clone();
        app.set_input(lp);
    }
}

/// Ctrl-E: with an empty input buffer and a cached
/// [`ReplApp::last_bash_block`], paste that block's first non-blank line;
/// otherwise (non-empty buffer, or no cached block) move the cursor to the
/// end of the line.
///
/// Why: direct port of tagent's real `KeyCode::Char('e')` arm
/// (`crates/trusty-agents/src/repl/tui/keys.rs`) — only the first line
/// pastes because the REPL input is single-line; pasting a multi-line block
/// verbatim would silently truncate at the first `\n` on submit.
/// Test: [`tests::apply_ctrl_e_pastes_last_bash_block_when_input_empty`],
/// [`tests::apply_ctrl_e_falls_back_to_end_of_line_when_input_nonempty`],
/// [`tests::apply_ctrl_e_noop_when_no_block_and_input_empty`].
fn apply_ctrl_e(app: &mut ReplApp) {
    if app.input_buf.is_empty()
        && let Some(block) = &app.last_bash_block
    {
        let first_line = block
            .lines()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("")
            .to_string();
        if !first_line.is_empty() {
            app.input_buf = first_line;
            app.cursor_pos = app.input_buf.len();
            return;
        }
    }
    app.cursor_pos = app.input_buf.len();
}

#[cfg(test)]
mod tests;
