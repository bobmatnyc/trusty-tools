//! Translating one `session.events` [`SessionEventEnvelope`] into
//! [`ReplEvent`]s (issue #3415).
//!
//! Why: split out of `engine.rs` (issue #610's 500-SLOC production-file cap)
//! — this is the one piece of `EngineState::pump_session_events`
//! (`engine_state.rs`) that is pure and directly unit-testable without an
//! HTTP round trip, so it earns its own file rather than staying inline.
//! What: [`forward_session_event`] (the `Event` -> `ReplEvent` mapping).
//! `is_retryable_status` went with the HTTP transport in #6637 — a framed
//! JSON-RPC stream has no status code to classify, and every failure it can
//! report is worth the same bounded reconnect.
//! Test: `engine_tests::*` (in the sibling `engine_tests.rs`, included from
//! `engine.rs`).

use serde_json::{Value, json};
use tokio::sync::mpsc::UnboundedSender;
use trusty_code_tui::{DelegationOutcome, ReplEvent};

use crate::events::{Event, SessionEventEnvelope};

/// Build the terminal, VISIBLE-in-the-TUI event `pump_session_events` sends
/// when it gives up reconnecting (`SESSION_STREAM_MAX_RECONNECTS` exhausted)
/// without ever observing a `SessionDone`/`SessionCancelled` terminal event
/// (epic #3411's deferred Slice 3 review item, closed out by Slice 6).
///
/// Why: every `ReplEvent::ConnectionLost` sent DURING a retry already renders
/// visibly (`ReplApp::apply` pushes it to the status area), but
/// `ConnectionLost` alone never clears `ReplApp::busy`/`streaming_idx` — only
/// `AssistantOutput{done: true}` does. Without an event like this one, the
/// LAST reconnect attempt failing left the input composer stuck on
/// "streaming" forever with no error rendered in the chat itself: a silent
/// stall, exactly the failure mode the review flagged as one unit tests on
/// either side of the engine/TUI boundary would both pass while the
/// integrated behavior is wrong.
/// What: an `AssistantOutput{done: true, is_error: true}` chunk carrying
/// `reason` — reusing the SAME "terminal event" vocabulary
/// `forward_session_event` uses for a failed `SessionDone`, so `ReplApp`
/// needs no new rendering path to display it.
/// Test: `engine_tests::terminal_stream_failure_event_is_a_done_error_output`.
pub(super) fn terminal_stream_failure_event(reason: String) -> ReplEvent {
    ReplEvent::AssistantOutput {
        chunk: format!("connection lost: {reason}"),
        done: true,
        is_error: true,
    }
}

/// Translate one [`SessionEventEnvelope`] into zero or more [`ReplEvent`]s
/// on `tx`. Returns `true` iff this event is terminal for the current chat
/// turn (`SessionDone`/`SessionCancelled`) — the caller stops pumping.
///
/// Why: kept as a free function (not a method) so it's directly unit
/// testable against a hand-built envelope, no HTTP/mock server needed.
/// What: `Message`/`AgentMessage`/`PmThinking` -> `AssistantOutput` chunks
/// (`done: false`); `AgentMessageDelta` -> `AgentOutput`, keyed by
/// `(agent_id, turn_id)` (#7940); `ToolStarted` -> `ToolInvocation{result:
/// None}`; `ToolFinished`/`ToolError` -> `ToolInvocation{result: Some(..)}`,
/// keyed by the SAME `call_id` so the (future) tool-card renderer can pair
/// them; `PmDelegating`/`AgentSpawned` -> `DelegationStarted` and
/// `AgentDone`/`AgentFailed` -> `DelegationFinished` (#7940);
/// `PermissionRequested`/`PermissionResolved` -> the matching `ReplEvent`
/// halves of the TUI's permission prompt (#3422), field for field;
/// `SessionDone` -> a final `AssistantOutput{done: true}` (`is_error` iff
/// `status == "failed"`); `SessionCancelled` -> a status message. Every
/// other event kind (progress/telemetry this client doesn't render) is
/// silently ignored — forward-compatible with new `Event` variants (no
/// `match` arm needed per new kind, thanks to the catch-all). #7940 pins
/// what that catch-all may swallow: `engine_tests::
/// every_agent_attributed_event_maps_or_is_explicitly_ignored` fails if a
/// NEW agent-attributed variant lands in it unclassified.
/// Test: `engine_tests::forward_message_emits_assistant_output_chunk_not_done`,
/// `engine_tests::forward_tool_started_emits_tool_invocation_with_call_id`,
/// `engine_tests::forward_tool_finished_carries_result_and_shares_call_id`,
/// `engine_tests::forward_session_done_is_terminal`,
/// `engine_tests::forward_session_done_failed_marks_is_error`,
/// `engine_tests::forward_session_cancelled_is_terminal`,
/// `engine_tests::forward_unrelated_event_is_ignored`,
/// `engine_tests::forward_agent_message_delta_not_done_appends`,
/// `engine_tests::forward_agent_message_delta_done_finalizes`,
/// `engine_tests::forward_agent_message_delta_distinct_agent_ids_not_merged`,
/// `engine_tests::forward_agent_spawned_opens_a_delegation`,
/// `engine_tests::forward_pm_delegating_opens_a_delegation_without_an_id`,
/// `engine_tests::forward_agent_done_closes_the_delegation`,
/// `engine_tests::forward_agent_failed_closes_with_the_error`,
/// `engine_tests::forward_permission_requested_opens_the_prompt`,
/// `engine_tests::forward_permission_resolved_closes_the_prompt`,
/// `engine_tests::every_agent_attributed_event_maps_or_is_explicitly_ignored`.
pub(super) fn forward_session_event(
    envelope: SessionEventEnvelope,
    tx: &UnboundedSender<ReplEvent>,
) -> bool {
    match envelope.event {
        Event::Message { text, .. }
        | Event::AgentMessage { text, .. }
        | Event::PmThinking { text, .. } => {
            let _ = tx.send(ReplEvent::AssistantOutput {
                chunk: text,
                done: false,
                is_error: false,
            });
            false
        }
        // tcode streaming epic #3696 Slice 2 wired this into the unkeyed
        // `AssistantOutput` append machinery; #7940 moves it onto
        // `ReplEvent::AgentOutput`, which carries the `(agent_id, turn_id)`
        // key the Slice 0 contract (`events.rs`, `Event::AgentMessageDelta`)
        // tells consumers to group by. That closes the gap this arm used to
        // document: two agents streaming concurrently inside ONE human turn
        // — routine once the PM delegates — landed in a single chat bubble
        // with their words physically interleaved.
        Event::AgentMessageDelta {
            agent_id,
            turn_id,
            delta,
            done,
            ..
        } => {
            let _ = tx.send(ReplEvent::AgentOutput {
                agent_id,
                turn_id,
                chunk: delta,
                done,
            });
            false
        }
        // #7940: the delegation brackets. `PmDelegating` is the delegating
        // agent's announcement and carries no `agent_id` yet; `AgentSpawned`
        // is the sub-agent's actual spawn and does. Both map to the SAME
        // start event — the reducer adopts an already-announced block rather
        // than opening a second one (see `ReplEvent::DelegationStarted`), so
        // a producer emitting either or both renders one block.
        Event::PmDelegating {
            agent,
            task_preview,
            ..
        } => {
            let _ = tx.send(ReplEvent::DelegationStarted {
                agent_id: String::new(),
                agent,
                task: task_preview,
            });
            false
        }
        Event::AgentSpawned {
            agent,
            agent_id,
            task_preview,
            ..
        } => {
            let _ = tx.send(ReplEvent::DelegationStarted {
                agent_id,
                agent,
                task: task_preview,
            });
            false
        }
        Event::AgentDone {
            agent,
            agent_id,
            status,
            ..
        } => {
            let _ = tx.send(ReplEvent::DelegationFinished {
                agent_id,
                agent,
                outcome: DelegationOutcome::Finished(status),
            });
            false
        }
        Event::AgentFailed {
            agent,
            agent_id,
            error,
            ..
        } => {
            let _ = tx.send(ReplEvent::DelegationFinished {
                agent_id,
                agent,
                outcome: DelegationOutcome::Failed(error),
            });
            false
        }
        Event::ToolStarted {
            agent_id,
            tool,
            call_id,
            args_preview,
            ..
        } => {
            let _ = tx.send(ReplEvent::ToolInvocation {
                id: call_id,
                agent_id,
                tool_name: tool,
                args: json!(args_preview),
                result: None,
            });
            false
        }
        Event::ToolFinished {
            agent_id,
            tool,
            call_id,
            result_preview,
            success,
            ..
        } => {
            let result = if success {
                result_preview
            } else {
                format!("FAILED: {result_preview}")
            };
            let _ = tx.send(ReplEvent::ToolInvocation {
                id: call_id,
                agent_id,
                tool_name: tool,
                args: Value::Null,
                result: Some(result),
            });
            false
        }
        Event::ToolError {
            agent_id,
            tool,
            call_id,
            error,
            ..
        } => {
            let _ = tx.send(ReplEvent::ToolInvocation {
                id: call_id,
                agent_id,
                tool_name: tool,
                args: Value::Null,
                result: Some(format!("ERROR: {error}")),
            });
            false
        }
        // #3422: the two halves of a permission prompt. `PermissionRequested`
        // leaves a tool call suspended on the daemon, so dropping it into the
        // catch-all (as this arm's absence did until now) made the TUI look
        // hung until the daemon's own timeout denied the call.
        Event::PermissionRequested {
            request_id,
            agent,
            agent_id,
            tool,
            subject,
            rule,
            ..
        } => {
            let _ = tx.send(ReplEvent::PermissionRequested {
                request_id,
                agent,
                agent_id,
                tool,
                subject,
                rule,
            });
            false
        }
        Event::PermissionResolved {
            request_id,
            agent,
            agent_id,
            decision,
            source,
            ..
        } => {
            let _ = tx.send(ReplEvent::PermissionResolved {
                request_id,
                agent,
                agent_id,
                decision,
                source,
            });
            false
        }
        Event::SessionDone { status, .. } => {
            let _ = tx.send(ReplEvent::AssistantOutput {
                chunk: String::new(),
                done: true,
                is_error: status == "failed",
            });
            true
        }
        Event::SessionCancelled { .. } => {
            let _ = tx.send(ReplEvent::StatusMessage("cancelled".to_string()));
            true
        }
        _ => false,
    }
}
