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

use super::{ChatLine, ChatRole, Delegation, ReplApp, ToolCard};
use crate::event::{DelegationOutcome, KeyCode, KeyInput, ReplEvent};
use crate::model::{PendingPermission, PermissionAnswer};
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
            id,
            agent_id,
            tool_name,
            args,
            result,
        } => {
            let card = ToolCard {
                id,
                tool_name,
                args,
                result,
            };
            apply_tool_invocation(app, card, &agent_id);
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
        ReplEvent::PermissionRequested {
            request_id,
            agent,
            agent_id,
            tool,
            subject,
            rule,
        } => apply_permission_requested(
            app,
            PendingPermission {
                request_id,
                agent,
                agent_id,
                tool,
                subject,
                rule,
            },
        ),
        ReplEvent::PermissionResolved {
            request_id,
            agent,
            decision,
            source,
            ..
        } => apply_permission_resolved(app, &request_id, &agent, &decision, &source),
        ReplEvent::PermissionAnswerFailed { pending, error } => {
            apply_permission_answer_failed(app, pending, &error)
        }
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
                tool: None,
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
    app.chat.push(ChatLine {
        role,
        text,
        tool: None,
    });
    app.scroll_offset = 0;
}

/// File one half of a tool call — its start or its completion — into that
/// call's card (#4596).
///
/// Why: a call and its result arrive as two `ToolInvocation` events sharing
/// `id`. Pushing each as its own line split one call across two scrollback
/// entries.
/// What: a completion whose `id` names an open card fills that card's
/// `result` in place and releases the key; a repeated start for an open card
/// is a no-op. Anything else opens a new card, keyed by `id` while it runs.
/// Placement is decided once, when the card opens: [`ChatRole::Delegated`]
/// inside an open delegation block (#7940), [`ChatRole::Status`] otherwise.
/// An empty `id` never correlates. The completion's own `args` are dropped,
/// since producers send `Null` there.
/// Test: [`tests::tool_invocation_result_merges_into_its_call_card`],
/// [`tests::tool_invocation_result_without_a_start_opens_its_own_card`],
/// [`tests::tool_invocation_attributed_to_a_delegation_is_delegated_role`].
fn apply_tool_invocation(app: &mut ReplApp, card: ToolCard, agent_id: &str) {
    app.scroll_offset = 0;
    if !card.id.is_empty()
        && let Some(&idx) = app.tool_cards.get(&card.id)
        && let Some(open) = app.chat.get_mut(idx).and_then(|e| e.tool.as_mut())
        && open.id == card.id
    {
        if card.result.is_some() {
            open.result = card.result;
            app.tool_cards.remove(&card.id);
        }
        return;
    }
    if card.id.is_empty() || card.result.is_some() {
        app.tool_cards.remove(&card.id);
    } else {
        app.tool_cards.insert(card.id.clone(), app.chat.len());
    }
    let role = if is_delegated(app, agent_id) {
        ChatRole::Delegated
    } else {
        ChatRole::Status
    };
    app.chat.push(ChatLine {
        role,
        text: String::new(),
        tool: Some(card),
    });
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
                tool: None,
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

/// Open the modal permission prompt and record the request in the
/// scrollback (#3422).
///
/// Why: the request has to survive in the transcript even after the prompt
/// closes — an operator reading back needs to see what was asked, not only
/// what was answered. The prompt itself is separate state because it is
/// re-rendered every frame and addressed by `request_id`.
/// What: pushes one [`ChatRole::Status`] line naming the tool, the
/// already-redacted subject and the matched rule, then stores the request.
/// A second request arriving while one is open REPLACES it: the backend
/// suspends one call per agent loop, so an overlap means the first is
/// already resolved (its `PermissionResolved` may simply not have landed
/// yet) and stacking prompts would block on a request nobody can answer.
/// Test: [`tests::permission_requested_opens_a_prompt_and_records_it`],
/// [`tests::permission_requested_twice_keeps_only_the_newest_prompt`].
fn apply_permission_requested(app: &mut ReplApp, pending: PendingPermission) {
    let mut text = format!("permission: {} wants {}", pending.agent, pending.tool);
    if !pending.subject.trim().is_empty() {
        text.push_str(&format!(" — {}", pending.subject));
    }
    text.push_str(&format!(" (rule: {})", pending.rule));
    app.push_status(text);
    // #3422: a fresh request carries no failed answer — never inherit the
    // retry line from the prompt this one replaces.
    app.permission_error = None;
    app.pending_permission = Some(pending);
}

/// Record a permission decision and close the prompt it answers (#3422).
///
/// Why: the backend is the only authority on how a request resolved — it may
/// have timed out, been answered by another client, or been covered by a
/// grant this client never saw. Recording ITS word (rather than the local key
/// press) is what keeps the scrollback honest.
/// What: pushes one [`ChatRole::Status`] line carrying the producer's own
/// `decision`/`source` words verbatim, and clears
/// [`ReplApp::pending_permission`] only when the ids match — a resolution for
/// some other request must not unblock the prompt the user is looking at.
/// Test: [`tests::permission_resolved_clears_the_prompt_and_records_the_decision`],
/// [`tests::permission_resolved_for_another_request_leaves_the_prompt_pending`].
fn apply_permission_resolved(
    app: &mut ReplApp,
    request_id: &str,
    agent: &str,
    decision: &str,
    source: &str,
) {
    app.push_status(format!("permission: {agent} — {decision} (by {source})"));
    if app
        .pending_permission
        .as_ref()
        .is_some_and(|p| p.request_id == request_id)
    {
        app.pending_permission = None;
        app.permission_error = None;
    }
}

/// Reopen the prompt whose answer never reached the backend (#3422).
///
/// Why: [`ReplApp::answer_permission`] clears the prompt the instant a key is
/// pressed, ahead of the RPC, so a slow backend can never freeze the
/// keyboard. A failed relay makes that optimism wrong: the call is still
/// suspended on the backend, and with the modal gone the operator has no way
/// to answer it again. This restores the question.
/// What: records the engine's own error text in the scrollback and, unless
/// some OTHER prompt is already open, puts the request back with
/// [`ReplApp::permission_error`] set so the prompt draws its retry line. A
/// newer prompt wins: the backend suspends one call per agent loop, so an
/// open prompt means this request is already moot.
/// Test: [`tests::permission_answer_failed_reopens_the_prompt_with_a_retry_line`],
/// [`tests::permission_answer_failed_never_clobbers_a_newer_prompt`],
/// `crate::run::tests::dispatch_pending_permission_answer_failure_reopens_the_prompt`.
fn apply_permission_answer_failed(app: &mut ReplApp, pending: PendingPermission, error: &str) {
    app.push_status(format!(
        "permission answer failed: {error} — {} is still waiting; answer again",
        pending.tool
    ));
    if app.pending_permission.is_some() {
        return;
    }
    app.permission_error = Some(format!("answer failed ({error}) — retry"));
    app.pending_permission = Some(pending);
}

/// The answer bound to `key` while a permission prompt is open (#3422), or
/// `None` for a key that is not one of the three bindings.
///
/// Why: kept pure so the whole keymap is testable without driving a frame,
/// and so "which keys answer" is one readable list rather than arms scattered
/// through [`apply_key`]. The bindings extend #3422's original y/n proposal
/// with the third answer #7948's wire protocol added — a prompt offering only
/// allow-once and deny would nag on every repeat of the same call.
/// What: `y`/`Y` allow once, `a`/`A` allow for the session, `n`/`N`/Esc deny
/// — exactly the keys [`crate::widgets::permission_prompt`]'s footer
/// advertises, and nothing else. Enter is deliberately UNBOUND; see the
/// inline note on the match below. `AllowForSession` carries `pattern: None`
/// deliberately: choosing a grant width is policy, and policy is the
/// backend's (ADR-0063).
/// Test: [`tests::permission_key_y_answers_allow_once`],
/// [`tests::permission_key_a_answers_allow_for_session`],
/// [`tests::permission_key_n_answers_deny`],
/// [`tests::permission_key_escape_answers_deny`],
/// [`tests::permission_unbound_key_leaves_the_prompt_pending`],
/// [`tests::enter_while_a_permission_prompt_is_pending_does_not_submit`].
fn permission_answer_for_key(key: KeyInput) -> Option<PermissionAnswer> {
    if key.modifiers.ctrl || key.modifiers.alt {
        return None;
    }
    // #3422: no `KeyCode::Enter` arm. A prior revision mapped Enter to
    // `AllowOnce`, a grant the footer never advertises — an operator pressing
    // Enter to send the line they were typing would silently approve the
    // suspended call. An unadvertised key must never answer; Enter falls
    // through to `_ => None` with every other key.
    match key.code {
        KeyCode::Char('y') | KeyCode::Char('Y') => Some(PermissionAnswer::AllowOnce),
        KeyCode::Char('a') | KeyCode::Char('A') => {
            Some(PermissionAnswer::AllowForSession { pattern: None })
        }
        KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => Some(PermissionAnswer::Deny),
        _ => None,
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
    // #3422: the prompt is modal. While a tool call is suspended on the
    // backend, every key either answers it or does nothing — routing a
    // keystroke to the line editor would let the user compose a turn the
    // backend cannot start, and Enter would look like a submit that silently
    // vanished. Deny is always one key away (`n` or Esc), so this can never
    // trap the keyboard.
    if app.pending_permission.is_some() {
        if let Some(answer) = permission_answer_for_key(key) {
            app.answer_permission(answer);
        }
        return;
    }
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
        // #8181: live since `apply_up` walks `history` rather than recalling
        // the single-level `last_prompt`. Down steps forward through the
        // same list and restores the in-progress draft at the newest end.
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

/// Up-arrow: walk one step back through [`ReplApp::history`], and — while
/// [`ReplApp::busy`] — ALSO signal [`ReplApp::pending_cancel`].
///
/// Why: #8181. This used to recall [`ReplApp::last_prompt`], a single
/// snapshot overwritten on every submit, so a second press repeated the
/// first and `KeyCode::Down` (which calls [`ReplApp::history_next`]) never
/// had an index to walk forward from. Walking [`ReplApp::history`] is the
/// readline behavior the owner asked for and is what makes the already-wired
/// Down arm live. The busy-cancel half is unchanged tagent parity
/// (`crates/trusty-agents/src/repl/tui/keys.rs`): it fires independent of
/// whether anything is recallable, and the composer's `↑ to cancel` hint
/// depends on it.
/// What: the input is a single line — [`crate::widgets::input_composer`]
/// renders `input_buf` on one row and no binding inserts a newline — so
/// readline's "history only when the cursor is on the first/last line" rule
/// is satisfied unconditionally here and Up is always history.
/// [`ReplApp::history_prev`] saves the in-progress draft on the first step
/// (so Down can restore it), clamps at the oldest entry, and no-ops on an
/// empty history.
/// Test: [`tests::apply_up_walks_back_through_submitted_prompts`],
/// [`tests::apply_down_walks_forward_and_restores_the_draft`],
/// [`tests::apply_up_clamps_at_the_oldest_entry`],
/// [`tests::apply_down_is_noop_when_not_navigating_history`],
/// [`tests::apply_up_signals_cancel_and_recalls_when_busy`],
/// [`tests::apply_up_signals_cancel_even_with_no_history`],
/// [`tests::apply_up_is_noop_when_idle_and_history_is_empty`].
fn apply_up(app: &mut ReplApp) {
    if app.busy {
        app.pending_cancel = true;
    }
    // #8181: was a single-level `last_prompt` recall; `history_prev` walks
    // the whole session's submissions and is what gives Down an index.
    app.history_prev();
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
