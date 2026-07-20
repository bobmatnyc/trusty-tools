//! Part of the `tui` module (split from the original monolithic `tui.rs`
//! to satisfy the 500-line file cap — see #357). Cross-submodule items and
//! external imports resolve through the flat re-exports in `mod.rs`.

use super::*;

/// Whether an Up/Down keypress should navigate `app.choices` right now,
/// rather than being reinterpreted as shell-style history recall (#3346).
///
/// Why: `handle_key` used to let Up/Down always navigate `app.choices`
/// whenever it was non-empty, even before the input-editing match arms ever
/// saw the key. That's still correct for two picker kinds the user is
/// *actively driving with arrow keys on purpose* — a live slash-command
/// completion list (rebuilt from `input_buf` on every keystroke) and the
/// tagged `/switch` persona picker (`choices_context == Some("switch")`),
/// which has no other input mechanism than Up/Down + Enter. Those must
/// always navigate, on the very first press, no exceptions — dismissing
/// `/switch` on its first Down (e.g. after the user paused a couple of
/// seconds reading the persona names before pressing a key) would make the
/// picker unusable, which an earlier wall-clock-based version of this fix
/// actually did (caught by `inline_choices_switch_context_dispatches_submit`
/// during review).
///
/// The #3325/#3346 residual case is narrower: an *untagged* `detect_choices`
/// LLM list (`choices_context.is_none()`, not a live slash completion) that
/// was offered alongside an assistant reply and then left untouched — the
/// user's attention has moved on to composing a new message, and their first
/// Up/Down is far more likely to be history recall than picker browsing.
/// Untagged lists ARE still arrow-navigable by design (`draw_inline_choice_picker`
/// highlights `choice_cursor`, and Enter inserts the highlighted item into
/// `input_buf` for editing) — so this predicate only reinterprets the very
/// first Up/Down on such a list, and only while the buffer is empty; once
/// the user has genuinely started navigating it (`choices_navigated`), every
/// further press keeps navigating.
/// What: True (navigate) when `app.choices` is a live slash-command
/// completion (`is_live_slash_completion`), OR `choices_context.is_some()`
/// (covers `/switch` and any future tagged picker), OR `choices_navigated`
/// is already `true`, OR `input_buf` is non-empty. False only for a
/// never-navigated, untagged, non-live picker with an empty buffer — the
/// #3346 history-recall case. Deliberately time-independent: no wall clock,
/// no tick counter.
/// Test: `repl_app_first_up_over_stale_untagged_picker_recalls_history`,
/// `repl_app_first_down_over_stale_untagged_picker_recalls_history`,
/// `repl_app_switch_picker_up_down_always_navigates`,
/// `repl_app_live_slash_picker_up_down_still_navigates`,
/// `repl_app_untagged_picker_navigates_once_started`.
fn should_navigate_picker(app: &ReplApp) -> bool {
    is_live_slash_completion(app)
        || app.choices_context.is_some()
        || app.choices_navigated
        || !app.input_buf.is_empty()
}

/// Handle one key press. Returns `Some(line)` if the user submitted.
pub(crate) fn handle_key(app: &mut ReplApp, key: KeyEvent) -> Option<String> {
    // Picker modal (`/model`, `/provider`): while an overlay is open, Up /
    // Down / Enter / Esc drive `handle_picker_key` exclusively so navigation
    // doesn't leak into the input editor underneath. Any OTHER key (#3403 —
    // sibling of #3325/#3346, which fixed the same swallow class for
    // `app.choices`) used to hit `handle_picker_key`'s `_ => {}` catch-all
    // and vanish with zero effect: the user would type a few characters
    // while `/model` was open, see nothing happen, and have no idea their
    // keystrokes were being eaten. Type-to-dismiss: a non-navigation key
    // closes the picker and falls through to the normal input path below,
    // so a printable character lands in `input_buf` exactly like it would
    // with no picker open. Mirrors the #3325 resolution for `app.choices`.
    if app.picker.is_some() {
        match key.code {
            KeyCode::Up | KeyCode::Down | KeyCode::Enter | KeyCode::Esc => {
                return handle_picker_key(app, key);
            }
            _ => {
                app.picker = None;
                // Fall through — the rest of `handle_key` treats this as a
                // normal keystroke (char insert, backspace, cursor move, …).
            }
        }
    }
    // Inline choice picker: when the LLM offered a list and we surfaced it,
    // arrow keys navigate the choices, Enter commits the selection into the
    // input buffer (or clears for free-type on the "Other…" row), Esc
    // dismisses. Other keys fall through so the user can keep typing — and
    // (#3325) dismiss a *stale picker* first, since `app.choices` otherwise
    // stays populated indefinitely (it's set once per assistant turn by
    // `push_assistant` -> `detect_choices`, or once by `/switch`, not
    // per-keystroke) and would keep intercepting the *next* Enter as
    // "confirm choice" (or, for `/switch`, silently dispatch a persona
    // switch) instead of "submit the line I just typed", swallowing the
    // whole typed message. This applies to BOTH the untagged
    // (`choices_context == None`) LLM list AND the tagged `/switch` picker
    // (`choices_context == Some("switch")`) — the only exemption is live
    // slash-command completions, which are also untagged but rebuilt from
    // `input_buf` on every keystroke by `update_slash_completions` below,
    // so they must survive typing rather than being dismissed by it.
    if !app.choices.is_empty() {
        match key.code {
            KeyCode::Up => {
                if should_navigate_picker(app) {
                    app.choice_cursor = app.choice_cursor.saturating_sub(1);
                    app.choices_navigated = true;
                    return None;
                }
                // Untagged, never-navigated, empty input buffer: this is
                // shell-style history recall, not picker navigation (#3346).
                // Dismiss and fall through to the normal Up-arrow handling
                // below instead of returning early. `/switch` and live
                // slash completions never reach here — `should_navigate_picker`
                // already returned `true` for both.
                app.dismiss_choices();
            }
            KeyCode::Down => {
                if should_navigate_picker(app) {
                    if app.choice_cursor + 1 < app.choices.len() {
                        app.choice_cursor += 1;
                    }
                    app.choices_navigated = true;
                    return None;
                }
                // Same disambiguation as Up above, mirrored for `history_next`.
                app.dismiss_choices();
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
                    app.dismiss_choices();
                    return None;
                }
                let pick = app.choices[idx].clone();
                let ctx = app.choices_context.clone();
                app.dismiss_choices();
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
                app.dismiss_choices();
                return None;
            }
            _ => {
                // Any other key means the user is editing/typing instead of
                // navigating the picker. Dismiss a stale picker — the
                // untagged LLM-offered list OR the tagged `/switch` list —
                // so the normal input-editing match below runs unobstructed
                // and a subsequent Enter submits the line instead of
                // confirming a choice (#3325) or firing an unintended
                // persona switch (#3325 follow-up). Leave live slash
                // completions alone: `is_live_slash_completion` only ever
                // matches the untagged, all-`/`-prefixed shape
                // `update_slash_completions` produces, so it never
                // shadows the tagged `/switch` picker.
                if !is_live_slash_completion(app) {
                    app.dismiss_choices();
                }
            }
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
                app.dismiss_choices();
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

/// Handle a key while a picker overlay is open, for the navigation keys
/// `handle_key` routes here (Up/Down/Enter/Esc). Every other key is
/// intercepted one level up in `handle_key`, which dismisses the picker and
/// falls through to normal input editing instead of calling this function
/// (#3403) — so this function only ever sees the four keys it understands.
///
/// Why: Centralizes the modal-state key routing so `handle_key`'s normal path
/// can stay focused on the input editor. Up/Down navigate (with wrap-around),
/// Enter confirms (stashing the choice in `pending_picker_selection` for the
/// event loop to translate into a Submit), Esc cancels.
/// What: Mutates `app.picker` directly. Returns `None` always — picker keys
/// never produce a submitted line directly; the event loop synthesizes a
/// `Submit("/model …")` after observing `pending_picker_selection`.
/// Test: `repl_app_picker_navigation_wraps`, `repl_app_picker_enter_sets_pending_selection`,
/// `repl_app_picker_esc_dismisses`, `repl_app_picker_printable_key_dismisses_and_inserts`.
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
