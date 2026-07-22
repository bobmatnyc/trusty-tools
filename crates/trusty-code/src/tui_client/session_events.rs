//! Translating one `GET /sessions/{id}/events` [`SessionEventEnvelope`] into
//! [`ReplEvent`]s, and classifying which HTTP statuses are worth an SSE
//! reconnect (issue #3415).
//!
//! Why: split out of `engine.rs` (issue #610's 500-SLOC production-file cap)
//! — this is the one piece of `EngineState::pump_session_events`
//! (`engine_state.rs`) that is pure and directly unit-testable without an
//! HTTP round trip, so it earns its own file rather than staying inline.
//! What: [`forward_session_event`] (the `Event` -> `ReplEvent` mapping) and
//! [`is_retryable_status`] (502/503 -> reconnect, everything else -> fail).
//! Test: `engine_tests::*` (in the sibling `engine_tests.rs`, included from
//! `engine.rs`).

use serde_json::{Value, json};
use tokio::sync::mpsc::UnboundedSender;
use trusty_tui::ReplEvent;

use crate::events::{Event, SessionEventEnvelope};

/// `true` for HTTP statuses worth retrying (daemon restarting) rather than
/// failing immediately.
pub(super) fn is_retryable_status(status: reqwest::StatusCode) -> bool {
    status == reqwest::StatusCode::BAD_GATEWAY || status == reqwest::StatusCode::SERVICE_UNAVAILABLE
}

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
/// (`done: false`); `ToolStarted` -> `ToolInvocation{result: None}`;
/// `ToolFinished`/`ToolError` -> `ToolInvocation{result: Some(..)}`, keyed
/// by the SAME `call_id` so the (future) tool-card renderer can pair them;
/// `SessionDone` -> a final `AssistantOutput{done: true}` (`is_error` iff
/// `status == "failed"`); `SessionCancelled` -> a status message. Every
/// other event kind (progress/telemetry/agent-lifecycle events this MVP
/// doesn't yet render) is silently ignored — forward-compatible with new
/// `Event` variants (no `match` arm needed per new kind, thanks to the
/// catch-all).
/// Test: `engine_tests::forward_message_emits_assistant_output_chunk_not_done`,
/// `engine_tests::forward_tool_started_emits_tool_invocation_with_call_id`,
/// `engine_tests::forward_tool_finished_carries_result_and_shares_call_id`,
/// `engine_tests::forward_session_done_is_terminal`,
/// `engine_tests::forward_session_done_failed_marks_is_error`,
/// `engine_tests::forward_session_cancelled_is_terminal`,
/// `engine_tests::forward_unrelated_event_is_ignored`.
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
        Event::ToolStarted {
            tool,
            call_id,
            args_preview,
            ..
        } => {
            let _ = tx.send(ReplEvent::ToolInvocation {
                id: call_id,
                tool_name: tool,
                args: json!(args_preview),
                result: None,
            });
            false
        }
        Event::ToolFinished {
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
                tool_name: tool,
                args: Value::Null,
                result: Some(result),
            });
            false
        }
        Event::ToolError {
            tool,
            call_id,
            error,
            ..
        } => {
            let _ = tx.send(ReplEvent::ToolInvocation {
                id: call_id,
                tool_name: tool,
                args: Value::Null,
                result: Some(format!("ERROR: {error}")),
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
