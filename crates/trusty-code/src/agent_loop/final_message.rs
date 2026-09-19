//! The "a turn must never end in silence" rule (#8238), split out of
//! `agent_loop/mod.rs` for the 500-SLOC cap — a child module of `agent_loop`
//! (declared via `#[path = ...] mod final_message;`).
//!
//! Why: `AgentLoop::run_inner` read "this turn requested no tools" as "the
//! model is done", so a terminal turn carrying no text ended the run with
//! nothing said at all — no report, no question, no refusal (#8238's P8/P9
//! parity transcripts). The client's only source of assistant text is
//! `ToolEventSink::agent_message`, so an empty turn means the user sees
//! literally nothing after the last tool result.
//! What: [`nudge_once`] recognises that turn and appends
//! [`FINAL_MESSAGE_NUDGE`] instead of letting the loop accept the silence.
//! Test: `agent_loop::tests::sink_events::silent_terminal_turn_is_nudged_into_a_real_final_message`.

use super::Transcript;
use crate::llm::ChatResponse;

/// The user-role prompt `AgentLoop::run_inner` appends when a terminating turn
/// says nothing (#8238).
///
/// Why: the closing message belongs to the model — a daemon-authored
/// placeholder ("the task ended") would be a new thing for the user to decode,
/// not an answer. So the loop asks for the real one.
/// What: names the three acceptable closings and forbids a tool call, so the
/// answer terminates the loop on the very next turn.
/// Test: `agent_loop::tests::sink_events::silent_terminal_turn_is_nudged_into_a_real_final_message`.
pub(super) const FINAL_MESSAGE_NUDGE: &str = "Your last turn produced no text \
and no tool calls, so the user saw nothing at all. Reply now with the closing \
message for this request: report what you did and what remains, ask the \
question you need answered, or state plainly that you are declining and why. \
Do not call a tool in this turn.";

/// Ask once for a closing message when `response` would end the run in
/// silence (#8238).
///
/// Why: "no tool calls" alone already terminated the loop; the missing half of
/// the test is whether the turn actually SAID anything. The caller has already
/// established there are no tool calls, so only the text is in question here.
/// The one-shot bound matters as much as the nudge: an unbounded "ask again"
/// would let a model that answers every nudge with silence burn the whole turn
/// budget, which is a longer stall than the bug it replaces.
/// What: returns `true` — meaning "take another turn" — only when the turn is
/// blank AND `already_nudged` is still `false`, appending the nudge as a user
/// turn and latching `already_nudged` on the way. Returns `false` otherwise,
/// leaving the caller's terminate-now path untouched.
/// Test: `agent_loop::tests::sink_events::silent_terminal_turn_is_nudged_into_a_real_final_message`,
/// `agent_loop::tests::sink_events::a_second_silent_turn_ends_the_run_instead_of_nudging_again`,
/// `agent_loop::tests::sink_events::a_terminal_turn_with_text_is_never_nudged`.
pub(super) fn nudge_once(
    response: &ChatResponse,
    transcript: &mut Transcript,
    already_nudged: &mut bool,
) -> bool {
    // A `None` text is as silent as an empty one — `unwrap_or_default` folds
    // the two shapes providers use for "this turn said nothing" into one.
    if *already_nudged || !response.first_text().unwrap_or_default().trim().is_empty() {
        return false;
    }
    *already_nudged = true;
    transcript.push_user(FINAL_MESSAGE_NUDGE);
    true
}
