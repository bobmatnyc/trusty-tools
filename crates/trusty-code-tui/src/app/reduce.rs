//! [`apply`] — the `ReplApp` half of [`crate::run::event_loop`]'s
//! `apply: FnMut(&mut M, ReplEvent)` contract.
//!
//! Why: Slice 4 (already on main) needed *some* `apply` closure so the
//! widgets could be demonstrated against real key input before DOC-50 §5
//! Slice 5 ("Event dispatch and line editing") landed as its own slice — it
//! shipped the generic core: character insertion/deletion, cursor movement,
//! history recall, and scroll. Slice 5 (this revision) completes the keymap
//! to full parity with tagent's actual `crates/trusty-agents/src/repl/tui/keys.rs`
//! (not its doc comments, which in at least one case — Up-arrow —
//! misattribute production behavior to a dead-code helper) and wires
//! [`ReplApp::pending_submit`]/[`ReplApp::pending_cancel`] through to
//! `TuiEngine::handle_input`/`cancel_session` in [`crate::run::run`]. See
//! [`crate::app`]'s module doc comment for the full disclosed-differences
//! list. Tagent-specific extras (pickers, slash-completion, cost/token
//! tracking) stay behind per the generalization mandate; line-editing
//! features tagent's own `keys.rs` never had either (Ctrl-w word-delete, a
//! kill-ring, word-motion) are deliberately NOT invented here — full parity
//! means matching tagent, not exceeding it.
//!
//! What: `apply` cannot reach the event channel or the engine — it only has
//! `&mut ReplApp` — so anything that would normally need to call
//! `TuiEngine::handle_input`/`cancel_session` instead stages a signal on
//! `ReplApp` ([`ReplApp::pending_submit`], [`ReplApp::pending_cancel`]) for
//! the caller to drain after `apply` returns, mirroring tagent's own
//! `pending_picker_selection`/`pending_cancel` precedent
//! (`crates/trusty-agents/src/repl/tui/types.rs`). [`crate::run::run`]'s
//! dispatch step is the actual drain point — see that module for the
//! `engine.handle_input`/`engine.cancel_session` wiring and the
//! `ReplEvent::Quit` round-trip an engine's `Ok(false)` needs to reach
//! `ReplApp::quit`, since a spawned task can't reach `&mut ReplApp` directly.
//!
//! # Spec References
//! - [`SPEC-TTUI-05~draft`](docs/specs/DOC-50-tcode-tui-claude-code-clone.md#SPEC-TTUI-05~draft) — Slice 5 deliverable (§5, Slice 5): line-editor keymap + Ctrl-C daemon cancel.

use super::{ChatLine, ChatRole, Delegation, ReplApp};
use crate::event::{DelegationOutcome, KeyCode, KeyInput, ReplEvent};
use crate::text::{strip_interior_blank_lines, trim_surrounding_blank_lines};

/// How many lines a Page-Up/Page-Down key press scrolls.
const PAGE_SCROLL: isize = 10;

/// Apply one [`ReplEvent`] to `app`, mutating it in place.
///
/// Why: a free function (rather than an `ReplApp` method) so it matches
/// `crate::run::event_loop`'s `apply: impl FnMut(&mut M, ReplEvent)` shape
/// exactly — a caller passes `trusty_code_tui::app::apply` directly as that
/// argument.
/// What: dispatches on every [`ReplEvent`] variant (see the module doc
/// comment for what's out of scope). `Key` events are further dispatched by
/// [`apply_key`].
/// Test: `tests` below cover every variant this function actually changes
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
        ReplEvent::Quit => app.quit = true,
        // `crate::run::dispatch_pending`'s completion safety net (see
        // `ReplEvent::TurnFinished`'s doc comment for the stuck-`busy`
        // deadlock this closes). Applied ONLY if `generation` still matches
        // `app.current_generation` — a stale signal from a turn that was
        // since cancelled/superseded is a no-op, by construction (this
        // compare runs serially in the reducer, never as a spawned task's
        // load-then-branch against the shared counter — see that variant's
        // doc comment for the TOCTOU race this closes). When applied, it
        // touches ONLY these two fields, never `chat`, so it can never push
        // a stray blank entry.
        ReplEvent::TurnFinished { generation } => {
            if generation == app.current_generation {
                app.busy = false;
                app.streaming_idx = None;
            }
        }
        ReplEvent::AssistantOutput {
            chunk,
            done,
            is_error,
        } => apply_assistant_output(app, chunk, done, is_error),
        ReplEvent::ToolInvocation {
            id: _,
            agent_id,
            tool_name,
            args,
            result,
        } => {
            let text = match result {
                None => format!("[TOOL] {tool_name}: {args}"),
                Some(r) => format!("[RESULT] {r}"),
            };
            // #7940: a delegated sub-agent's tool calls belong inside its own
            // block, not interleaved with the primary agent's at top level.
            if is_delegated(app, &agent_id) {
                push_delegated(app, ChatRole::Delegated, text);
            } else {
                app.push_status(text);
            }
        }
        ReplEvent::AgentOutput {
            agent_id,
            turn_id,
            chunk,
            done,
        } => apply_agent_output(app, agent_id, turn_id, chunk, done),
        ReplEvent::DelegationStarted {
            agent_id,
            agent,
            task,
        } => apply_delegation_started(app, agent_id, agent, task),
        ReplEvent::DelegationFinished {
            agent_id,
            agent,
            outcome,
        } => apply_delegation_finished(app, agent_id, agent, outcome),
        ReplEvent::StatusMessage(msg) => app.push_status(msg),
        ReplEvent::ClearScrollback => app.clear_scrollback(),
        ReplEvent::StatuslineUpdate(segments) => app.statusline = segments,
        ReplEvent::WorkstreamUpdated(ws) => app.active_workstream = Some(ws),
        ReplEvent::WorkstreamActivationChanged { new_active_id, .. } => {
            if new_active_id.is_none() {
                // The workstream was deactivated with no replacement active
                // (DOC-48 §4.2/§4.3, a real daemon-published state — see this
                // variant's own doc comment). `WorkstreamUpdated`'s payload
                // is a concrete `WorkstreamSummary`, so it structurally
                // CANNOT represent "no active workstream" — there is no
                // follow-up `WorkstreamUpdated` to wait for here, unlike the
                // `Some(new_id)` case below. This is the only place the
                // indicator gets cleared; skipping it (as a prior revision
                // did, treating this whole variant as an unconditional
                // no-op) left the status line showing a stale workstream
                // name forever after a deactivation.
                app.active_workstream = None;
            }
            // A `Some(new_id)` activation: per
            // `TuiEngine::subscribe_workstream_events`'s contract, the
            // engine follows this with a `WorkstreamUpdated` carrying the
            // full re-fetched summary — that event, not this one, is what
            // sets the displayed name (this event alone names an id with no
            // display name to show yet).
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
    match app.streaming_idx {
        Some(idx) => {
            if let Some(entry) = app.chat.get_mut(idx) {
                entry.text.push_str(&chunk);
            }
        }
        None => {
            app.chat.push(ChatLine {
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

/// Whether `agent_id` names a delegation whose block is currently open
/// (#7940).
///
/// Why: an empty `agent_id` means "no attribution", which must never match
/// an open block — otherwise an unattributed event would be filed under
/// whichever delegation happened to be running.
fn is_delegated(app: &ReplApp, agent_id: &str) -> bool {
    !agent_id.is_empty() && app.delegations.iter().any(|d| d.agent_id == agent_id)
}

/// Push one line into the scrollback with `role`, pinning the view to the
/// newest content (#7940).
fn push_delegated(app: &mut ReplApp, role: ChatRole, text: String) {
    app.chat.push(ChatLine { role, text });
    app.scroll_offset = 0;
}

/// Accumulate one attributed output chunk into the bubble for its
/// `(agent_id, turn_id)` stream (#7940).
///
/// Why: [`apply_assistant_output`] accumulates into ONE unkeyed slot
/// (`ReplApp::streaming_idx`), so a primary agent and a delegated sub-agent
/// streaming inside the same human turn appended into a single chat entry —
/// their words physically interleaved. Keying the in-progress entry by the
/// producer's own `(agent_id, turn_id)` pair makes that impossible by
/// construction rather than by luck of arrival order.
/// What: find-or-open the entry for the key, append, and on `done` trim it
/// and drop the key. The entry's role is decided ONCE, when it is opened:
/// `Delegated` when the id names an open delegation block, `Assistant`
/// otherwise. `busy` is deliberately untouched — one agent's turn ending is
/// not the human turn ending, which only `AssistantOutput { done: true }`
/// reports.
/// Test: [`tests::agent_output_keys_concurrent_streams_separately`],
/// [`tests::agent_output_finalizes_and_drops_its_key`],
/// [`tests::agent_output_inside_a_delegation_is_delegated_role`].
fn apply_agent_output(
    app: &mut ReplApp,
    agent_id: String,
    turn_id: String,
    chunk: String,
    done: bool,
) {
    let key = (agent_id, turn_id);
    let idx = match app.agent_streams.get(&key) {
        Some(&idx) => idx,
        None => {
            // A terminal delta carries no text of its own (see
            // `ReplEvent::AgentOutput`), so opening a bubble for one whose
            // stream we never saw would push a permanently blank entry.
            if done && chunk.is_empty() {
                return;
            }
            let role = if is_delegated(app, &key.0) {
                ChatRole::Delegated
            } else {
                ChatRole::Assistant
            };
            app.chat.push(ChatLine {
                role,
                text: String::new(),
            });
            let idx = app.chat.len() - 1;
            app.agent_streams.insert(key.clone(), idx);
            idx
        }
    };
    if let Some(entry) = app.chat.get_mut(idx) {
        entry.text.push_str(&chunk);
    }
    app.scroll_offset = 0;

    if done {
        app.agent_streams.remove(&key);
        if let Some(entry) = app.chat.get_mut(idx) {
            let trimmed = trim_surrounding_blank_lines(&entry.text);
            entry.text = strip_interior_blank_lines(&trimmed);
        }
        app.update_last_bash_block();
    }
}

/// Open a delegated sub-agent's scrollback block (#7940).
///
/// Why: this is what makes delegation visible at all — without a header the
/// sub-agent's output and tool calls arrive indistinguishable from the
/// primary agent's.
/// What: pushes a [`ChatRole::Delegation`] header and registers the
/// delegation so later attributed events file under it. A second
/// `DelegationStarted` for the same agent ADOPTS the open block instead of
/// opening a duplicate — a producer that announces its intent to delegate
/// before the sub-agent exists (no `agent_id` yet) and then reports the real
/// spawn emits two events for one delegation.
/// Test: [`tests::delegation_started_pushes_header_and_sets_active_agent`],
/// [`tests::delegation_started_with_id_adopts_an_announced_block`],
/// [`tests::delegation_started_twice_for_distinct_ids_opens_two_blocks`].
fn apply_delegation_started(app: &mut ReplApp, agent_id: String, agent: String, task: String) {
    if let Some(open) = app
        .delegations
        .iter_mut()
        .find(|d| d.agent == agent && (d.agent_id.is_empty() || d.agent_id == agent_id))
    {
        if open.agent_id.is_empty() {
            open.agent_id = agent_id;
        }
        return;
    }
    app.delegations.push(Delegation {
        agent_id,
        agent: agent.clone(),
    });
    let text = if task.trim().is_empty() {
        format!("▶ {agent}")
    } else {
        format!("▶ {agent} — {task}")
    };
    push_delegated(app, ChatRole::Delegation, text);
}

/// Close a delegated sub-agent's scrollback block with its outcome (#7940).
///
/// Why: a block with no footer reads as still-running forever, and "finished"
/// vs. "blew up" is the single fact an operator watching a delegation most
/// needs.
/// What: pushes a [`ChatRole::Delegation`] footer naming the outcome, drops
/// the delegation (so [`ReplApp::active_agent`] reverts), and drops any
/// still-open stream key for that agent — a failed loop aborts mid-turn and
/// never sends its terminal chunk, which would otherwise leak a map entry
/// for the life of the session. The footer renders even for an id this
/// client never saw start (a TUI that attached mid-delegation), rather than
/// dropping the only report of how the run ended.
/// Test: [`tests::delegation_finished_pushes_footer_and_clears_active_agent`],
/// [`tests::delegation_finished_failed_footer_carries_the_error`],
/// [`tests::delegation_finished_drops_an_unterminated_stream_key`].
fn apply_delegation_finished(
    app: &mut ReplApp,
    agent_id: String,
    agent: String,
    outcome: DelegationOutcome,
) {
    app.delegations
        .retain(|d| !(d.agent_id == agent_id && d.agent == agent));
    app.agent_streams.retain(|(aid, _), _| aid != &agent_id);
    let text = match outcome {
        DelegationOutcome::Finished(status) if status.trim().is_empty() => format!("└ {agent}"),
        DelegationOutcome::Finished(status) => format!("└ {agent} — {status}"),
        DelegationOutcome::Failed(error) => format!("└ {agent} — failed: {error}"),
    };
    push_delegated(app, ChatRole::Delegation, text);
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
            // Direct port of tagent's real `KeyCode::Char('d')` arm
            // (`crates/trusty-agents/src/repl/tui/keys.rs`): Ctrl-D only
            // quits on an EMPTY input buffer (the readline EOF convention);
            // with text still in the buffer it's a no-op in tagent too (no
            // forward-delete fallback), so this stays a plain guard rather
            // than growing new behavior tagent doesn't have.
            'd' if app.input_buf.is_empty() => app.quit = true,
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
        // Gated on `!app.busy` BEFORE `take_input()` runs (not just inside
        // `submit_line`, which also guards): while a turn is in flight, the
        // typed line stays in `input_buf` untouched rather than being taken
        // out and then silently dropped by `submit_line`'s own guard — DOC-50
        // §5 Slice 5's "blocks user input until cancel completes", made
        // literal so a second turn genuinely cannot start (see
        // `ReplApp::submit_line`'s doc comment for the corruption bug this
        // closes).
        KeyCode::Enter => {
            if !app.busy
                && let Some(line) = app.take_input()
            {
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
