//! Part of the `tui` module (split from the original monolithic `tui.rs`
//! to satisfy the 500-line file cap — see #357). Cross-submodule items and
//! external imports resolve through the flat re-exports in `mod.rs`.

use super::*;

/// Handle one key press. Returns `Some(line)` if the user submitted.
pub(crate) fn handle_key(app: &mut ReplApp, key: KeyEvent) -> Option<String> {
    // Picker modal: when an overlay is open, capture all keys here so
    // arrow / Enter / Esc don't leak through to the input editor.
    if app.picker.is_some() {
        return handle_picker_key(app, key);
    }
    // Inline choice picker: when the LLM offered a list and we surfaced it,
    // arrow keys navigate the choices, Enter commits the selection into the
    // input buffer (or clears for free-type on the "Other…" row), Esc
    // dismisses. Other keys fall through so the user can keep typing.
    if !app.choices.is_empty() {
        match key.code {
            KeyCode::Up => {
                app.choice_cursor = app.choice_cursor.saturating_sub(1);
                return None;
            }
            KeyCode::Down => {
                if app.choice_cursor + 1 < app.choices.len() {
                    app.choice_cursor += 1;
                }
                return None;
            }
            KeyCode::Enter => {
                let idx = app.choice_cursor;
                let last = app.choices.len().saturating_sub(1);
                let is_other = idx == last
                    && app
                        .choices
                        .get(idx)
                        .map(|s| s.starts_with("Other"))
                        .unwrap_or(false);
                if is_other {
                    // Free-type path: leave input empty for the user.
                    app.choices.clear();
                    app.choice_cursor = 0;
                    app.choices_context = None;
                    return None;
                }
                let pick = app.choices[idx].clone();
                let ctx = app.choices_context.take();
                app.choices.clear();
                app.choice_cursor = 0;
                match ctx.as_deref() {
                    Some("switch") => {
                        // Direct dispatch — synthesize `/switch <name>`
                        // so the user doesn't need a second Enter.
                        app.pending_submit = Some(format!("/switch {}", pick));
                    }
                    _ => {
                        // Default: insert selection into input buffer for
                        // the user to edit/submit themselves.
                        app.set_input(pick);
                    }
                }
                return None;
            }
            KeyCode::Esc => {
                app.choices.clear();
                app.choice_cursor = 0;
                app.choices_context = None;
                return None;
            }
            _ => { /* fall through to normal input editing */ }
        }
    }
    // Ctrl combos.
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        match key.code {
            KeyCode::Char('c') => {
                // Cancel current input but stay in REPL.
                app.input_buf.clear();
                app.cursor_pos = 0;
                return None;
            }
            KeyCode::Char('d') => {
                if app.input_buf.is_empty() {
                    app.quit = true;
                }
                return None;
            }
            KeyCode::Char('a') => {
                app.cursor_pos = 0;
                return None;
            }
            KeyCode::Char('e') => {
                // #321: When the input is empty, paste the most recent
                // bash/sh fenced block from chat into the buffer so the
                // user can edit-and-run a suggested shell command without
                // mouse selection. Falls through to "cursor to end" when
                // input is non-empty (preserves the readline End-of-line
                // muscle memory).
                if app.input_buf.is_empty()
                    && let Some(block) = &app.last_bash_block
                {
                    // #323: Only paste the first non-empty line — the
                    // REPL input is single-line. Multi-line blocks are
                    // common (sequential commands like `git add -A` then
                    // `git commit -m "msg"`); pasting the whole block
                    // would silently truncate at the first `\n` on submit.
                    let first_line = block
                        .lines()
                        .find(|l| !l.trim().is_empty())
                        .unwrap_or("")
                        .to_string();
                    if !first_line.is_empty() {
                        app.input_buf = first_line;
                        app.cursor_pos = app.input_buf.len();
                        return None;
                    }
                }
                app.cursor_pos = app.input_buf.len();
                return None;
            }
            KeyCode::Char('u') => {
                app.input_buf.clear();
                app.cursor_pos = 0;
                return None;
            }
            _ => {}
        }
    }

    match key.code {
        KeyCode::Enter => app.take_input(),
        KeyCode::Tab => {
            // Slash-command autocomplete: when the inline picker is showing
            // matches, Tab completes the highlighted command into the input
            // buffer with a trailing space (so the user can immediately type
            // arguments). Falls through to no-op when no choices are active.
            if !app.choices.is_empty()
                && app.choices_context.is_none()
                && app.input_buf.starts_with('/')
            {
                let selected = app.choices[app.choice_cursor].clone();
                app.input_buf = format!("{selected} ");
                app.cursor_pos = app.input_buf.len();
                app.choices.clear();
                app.choice_cursor = 0;
            }
            None
        }
        KeyCode::Char(c) => {
            app.insert_char(c);
            update_slash_completions(app);
            None
        }
        KeyCode::Backspace => {
            app.backspace();
            update_slash_completions(app);
            None
        }
        KeyCode::Left => {
            app.cursor_left();
            None
        }
        KeyCode::Right => {
            app.cursor_right();
            None
        }
        KeyCode::Home => {
            app.cursor_pos = 0;
            None
        }
        KeyCode::End => {
            app.cursor_pos = app.input_buf.len();
            None
        }
        KeyCode::Up => {
            // New semantics (#XXX): Up-arrow recalls the last submitted
            // prompt into the input buffer. While the LLM is busy, it ALSO
            // signals the event loop to cancel the in-flight task via
            // `pending_cancel` — the user can then edit and resubmit.
            if app.thinking {
                app.pending_cancel = true;
            }
            if !app.last_prompt.is_empty() {
                let lp = app.last_prompt.clone();
                app.set_input(lp);
            }
            None
        }
        KeyCode::Down => {
            app.history_next();
            None
        }
        KeyCode::PageUp => {
            app.scroll(-10);
            None
        }
        KeyCode::PageDown => {
            app.scroll(10);
            None
        }
        _ => None,
    }
}

/// Handle a key while a picker overlay is open.
///
/// Why: Centralizes the modal-state key routing so `handle_key`'s normal path
/// can stay focused on the input editor. Up/Down navigate (with wrap-around),
/// Enter confirms (stashing the choice in `pending_picker_selection` for the
/// event loop to translate into a Submit), Esc cancels.
/// What: Mutates `app.picker` directly. Returns `None` always — picker keys
/// never produce a submitted line directly; the event loop synthesizes a
/// `Submit("/model …")` after observing `pending_picker_selection`.
/// Test: `repl_app_picker_navigation_wraps`, `repl_app_picker_enter_sets_pending_selection`,
/// `repl_app_picker_esc_dismisses`.
pub(crate) fn handle_picker_key(app: &mut ReplApp, key: KeyEvent) -> Option<String> {
    let picker = app.picker.as_mut().expect("picker present");
    match key.code {
        KeyCode::Up => {
            if picker.items.is_empty() {
                return None;
            }
            if picker.selected == 0 {
                picker.selected = picker.items.len() - 1;
            } else {
                picker.selected -= 1;
            }
        }
        KeyCode::Down => {
            if picker.items.is_empty() {
                return None;
            }
            picker.selected = (picker.selected + 1) % picker.items.len();
        }
        KeyCode::Enter if !picker.items.is_empty() => {
            let selected = picker.items[picker.selected].clone();
            let kind = picker.kind.clone();
            app.picker = None;
            app.pending_picker_selection = Some((kind, selected));
        }
        KeyCode::Esc => {
            app.picker = None;
        }
        _ => {}
    }
    None
}
