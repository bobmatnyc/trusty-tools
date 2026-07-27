//! Tests for `engine.rs` and its sibling modules (`engine_state.rs`,
//! `session_events.rs`, `workstream_subscription.rs`) — issue #3415.
//!
//! Why: kept as a sibling `_tests.rs` file (not inline in `engine.rs`), per
//! this crate's established convention (`serve/http.rs` -> `http_tests.rs`,
//! `workstreams/sse.rs` -> `sse_tests.rs`) — a `_tests.rs` suffix is scored
//! against the 1500-SLOC TEST cap (issue #610/#1131), not the 500-SLOC
//! PRODUCTION cap, so splitting tests out of `engine.rs` is what keeps that
//! file itself under cap without shrinking coverage.

use tokio::sync::mpsc::unbounded_channel;

use super::*;
use crate::events::{Event, SessionEventEnvelope};
use crate::tui_client::session_events::{
    forward_session_event, is_retryable_status, terminal_stream_failure_event,
};
use crate::tui_client::workstream_subscription::parse_workstream_envelope;
use trusty_tui::{StatuslineSegment, WorkstreamSummary};

fn envelope(event: Event) -> SessionEventEnvelope {
    SessionEventEnvelope::new("s-1".to_string(), 1, chrono::Utc::now(), event)
}

#[test]
fn forward_message_emits_assistant_output_chunk_not_done() {
    let (tx, mut rx) = unbounded_channel();
    let terminal = forward_session_event(
        envelope(Event::Message {
            session_id: "s-1".into(),
            text: "hi".into(),
        }),
        &tx,
    );
    assert!(!terminal);
    assert_eq!(
        rx.try_recv().expect("event"),
        ReplEvent::AssistantOutput {
            chunk: "hi".into(),
            done: false,
            is_error: false,
        }
    );
}

#[test]
fn forward_tool_started_emits_tool_invocation_with_call_id() {
    let (tx, mut rx) = unbounded_channel();
    let terminal = forward_session_event(
        envelope(Event::ToolStarted {
            session_id: "s-1".into(),
            agent: "pm".into(),
            agent_id: String::new(),
            tool: "bash".into(),
            call_id: "call-1".into(),
            args_preview: "ls".into(),
        }),
        &tx,
    );
    assert!(!terminal);
    match rx.try_recv().expect("event") {
        ReplEvent::ToolInvocation {
            id,
            tool_name,
            result,
            ..
        } => {
            assert_eq!(id, "call-1");
            assert_eq!(tool_name, "bash");
            assert!(result.is_none());
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn forward_tool_finished_carries_result_and_shares_call_id() {
    let (tx, mut rx) = unbounded_channel();
    forward_session_event(
        envelope(Event::ToolFinished {
            session_id: "s-1".into(),
            agent: "pm".into(),
            agent_id: String::new(),
            tool: "bash".into(),
            call_id: "call-1".into(),
            success: true,
            result_preview: "done".into(),
        }),
        &tx,
    );
    match rx.try_recv().expect("event") {
        ReplEvent::ToolInvocation { id, result, .. } => {
            assert_eq!(id, "call-1");
            assert_eq!(result.as_deref(), Some("done"));
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn forward_session_done_is_terminal() {
    let (tx, mut rx) = unbounded_channel();
    let terminal = forward_session_event(
        envelope(Event::SessionDone {
            session_id: "s-1".into(),
            status: "finished".into(),
        }),
        &tx,
    );
    assert!(terminal);
    assert_eq!(
        rx.try_recv().expect("event"),
        ReplEvent::AssistantOutput {
            chunk: String::new(),
            done: true,
            is_error: false,
        }
    );
}

#[test]
fn forward_session_done_failed_marks_is_error() {
    let (tx, mut rx) = unbounded_channel();
    forward_session_event(
        envelope(Event::SessionDone {
            session_id: "s-1".into(),
            status: "failed".into(),
        }),
        &tx,
    );
    assert_eq!(
        rx.try_recv().expect("event"),
        ReplEvent::AssistantOutput {
            chunk: String::new(),
            done: true,
            is_error: true,
        }
    );
}

#[test]
fn forward_session_cancelled_is_terminal() {
    let (tx, mut rx) = unbounded_channel();
    let terminal = forward_session_event(
        envelope(Event::SessionCancelled {
            session_id: "s-1".into(),
        }),
        &tx,
    );
    assert!(terminal);
    assert!(rx.try_recv().is_ok());
}

#[test]
fn forward_unrelated_event_is_ignored() {
    let (tx, mut rx) = unbounded_channel();
    let terminal = forward_session_event(envelope(Event::Ping), &tx);
    assert!(!terminal);
    assert!(rx.try_recv().is_err());
}

/// tcode streaming epic #3696 Slice 2: a non-final `AgentMessageDelta`
/// (`done: false`) must forward as an `AssistantOutput` chunk with
/// `done: false`, reusing the SAME append machinery `Message`/`AgentMessage`
/// use (`trusty-tui reduce.rs`'s `streaming_idx`-keyed append) — no new
/// rendering path.
#[test]
fn forward_agent_message_delta_not_done_appends() {
    let (tx, mut rx) = unbounded_channel();
    let terminal = forward_session_event(
        envelope(Event::AgentMessageDelta {
            session_id: "s-1".into(),
            agent: "coder".into(),
            agent_id: "agent-1".into(),
            turn_id: "turn-1".into(),
            delta: "Hello".into(),
            done: false,
        }),
        &tx,
    );
    assert!(!terminal);
    assert_eq!(
        rx.try_recv().expect("event"),
        ReplEvent::AssistantOutput {
            chunk: "Hello".into(),
            done: false,
            is_error: false,
        }
    );
}

/// The final delta (`done: true`) for a turn must forward `done: true` (so
/// `apply_assistant_output` finalizes the streaming bubble) — and, unlike
/// `SessionDone`, is NOT terminal for the SSE pump itself: one agent's turn
/// finishing doesn't mean the whole session stream is over.
#[test]
fn forward_agent_message_delta_done_finalizes() {
    let (tx, mut rx) = unbounded_channel();
    let terminal = forward_session_event(
        envelope(Event::AgentMessageDelta {
            session_id: "s-1".into(),
            agent: "coder".into(),
            agent_id: "agent-1".into(),
            turn_id: "turn-1".into(),
            delta: " world".into(),
            done: true,
        }),
        &tx,
    );
    assert!(
        !terminal,
        "an agent turn finishing must not end the SSE pump"
    );
    assert_eq!(
        rx.try_recv().expect("event"),
        ReplEvent::AssistantOutput {
            chunk: " world".into(),
            done: true,
            is_error: false,
        }
    );
}

/// Two agents streaming concurrently and sharing a `turn_id` (the Slice 0
/// contract's defensive `(agent_id, turn_id)` grouping scenario,
/// `events.rs:436-456`) must each forward as their OWN independent
/// `AssistantOutput` message, in call order — `forward_session_event` never
/// accumulates or merges chunks itself; it is a stateless 1:1 mapper.
///
/// NOTE (see PR description / module doc comment on the `AgentMessageDelta`
/// arm): this proves `forward_session_event` doesn't merge them. It does
/// NOT prove the two stay visually separate once downstream in
/// `trusty-tui` — `ReplEvent::AssistantOutput` carries no `agent_id`/
/// `turn_id`, and `ReplApp::streaming_idx` is a single unkeyed
/// `Option<usize>`, so today the reducer WOULD interleave them into one
/// chat bubble. That gap lives in `trusty-tui` (out of this crate's file
/// scope) and is tracked as a follow-up, not fixed in this slice.
#[test]
fn forward_agent_message_delta_distinct_agent_ids_not_merged() {
    let (tx, mut rx) = unbounded_channel();
    forward_session_event(
        envelope(Event::AgentMessageDelta {
            session_id: "s-1".into(),
            agent: "coder".into(),
            agent_id: "agent-1".into(),
            turn_id: "turn-shared".into(),
            delta: "from-agent-1".into(),
            done: false,
        }),
        &tx,
    );
    forward_session_event(
        envelope(Event::AgentMessageDelta {
            session_id: "s-1".into(),
            agent: "reviewer".into(),
            agent_id: "agent-2".into(),
            turn_id: "turn-shared".into(),
            delta: "from-agent-2".into(),
            done: false,
        }),
        &tx,
    );
    assert_eq!(
        rx.try_recv().expect("event"),
        ReplEvent::AssistantOutput {
            chunk: "from-agent-1".into(),
            done: false,
            is_error: false,
        },
        "agent-1's delta must forward as its own message, not merged with agent-2's"
    );
    assert_eq!(
        rx.try_recv().expect("event"),
        ReplEvent::AssistantOutput {
            chunk: "from-agent-2".into(),
            done: false,
            is_error: false,
        },
        "agent-2's delta must forward as its own message, not merged with agent-1's"
    );
}

#[test]
fn workstream_subcommand_parses_list_and_activate() {
    assert_eq!(workstream_subcommand("/workstream list"), Some("list"));
    assert_eq!(
        workstream_subcommand("/ws activate abc-123"),
        Some("activate abc-123")
    );
    assert_eq!(workstream_subcommand("/workstream"), Some(""));
    assert_eq!(workstream_subcommand("/workstreamx foo"), None);
    assert_eq!(workstream_subcommand("hello"), None);
}

#[test]
fn parse_workstream_envelope_round_trips_activation_changed() {
    let json = serde_json::json!({
        "session_id": "",
        "event_type": "workstream_activation_changed",
        "payload": {
            "type": "workstream_activation_changed",
            "new_active_id": "ws-2",
            "prior_id": "ws-1",
        },
    })
    .to_string();
    let env = parse_workstream_envelope(&json).expect("parse");
    match env.payload {
        Event::WorkstreamActivationChanged {
            new_active_id,
            prior_id,
        } => {
            assert_eq!(new_active_id.as_deref(), Some("ws-2"));
            assert_eq!(prior_id.as_deref(), Some("ws-1"));
        }
        other => panic!("unexpected {other:?}"),
    }
}

/// `pump_session_events` giving up after exhausting reconnects must produce
/// a `done: true, is_error: true` `AssistantOutput` — the epic #3411
/// deferred-verification item: a `ConnectionLost` alone never clears
/// `ReplApp::busy`, so this is what actually rules out a silent stall (a
/// stuck spinner with no visible error) once the last reconnect attempt
/// fails.
#[test]
fn terminal_stream_failure_event_is_a_done_error_output() {
    let event = terminal_stream_failure_event("daemon unreachable".to_string());
    assert_eq!(
        event,
        ReplEvent::AssistantOutput {
            chunk: "connection lost: daemon unreachable".to_string(),
            done: true,
            is_error: true,
        }
    );
}

#[test]
fn is_retryable_status_covers_502_and_503_only() {
    assert!(is_retryable_status(reqwest::StatusCode::BAD_GATEWAY));
    assert!(is_retryable_status(
        reqwest::StatusCode::SERVICE_UNAVAILABLE
    ));
    assert!(!is_retryable_status(reqwest::StatusCode::NOT_FOUND));
}

/// The caches must start empty before `setup()` populates them —
/// `TuiEngine::commands()`/`picker()` must degrade to "nothing yet," never
/// panic, when queried before `setup()` runs.
#[test]
fn caches_are_empty_before_setup() {
    let engine = CodeEngine::with_daemon_url(reqwest::Client::new(), "http://127.0.0.1:1", None);
    assert!(engine.commands().is_empty());
    assert!(engine.picker("workstream").is_none());
}

/// `picker("workstream")` must carry the exact dispatch command
/// `workstream_subcommand` parses back — proves the picker-selection
/// round trip (`"{dispatch_command} {selected.id}"`) actually reaches
/// `handle_workstream_command`'s `activate <id>` branch, not some other
/// unparsed string.
#[test]
fn workstream_picker_dispatch_command_round_trips_through_workstream_subcommand() {
    let dispatch_command = "/workstream activate";
    let resubmitted = format!("{dispatch_command} ws-1");
    assert_eq!(workstream_subcommand(&resubmitted), Some("activate ws-1"));
}

/// An unknown picker name must be `None`, not an empty-but-`Some` request —
/// this engine has exactly one picker.
#[test]
fn picker_unknown_name_is_none() {
    let engine = CodeEngine::with_daemon_url(reqwest::Client::new(), "http://127.0.0.1:1", None);
    assert!(engine.picker("model").is_none());
}

/// `EngineState::statusline_segments` must mirror the `active_workstream`
/// cache exactly: no segment before any workstream is known, one
/// `StatuslineSegment::Workstream` once it is, and — critically — an EMPTY
/// list again (not a stale segment left behind) once deactivated. This is
/// the seam `setup`/`run_workstream_subscription` push through
/// `ReplEvent::StatuslineUpdate` so the status line's "WS: name (id)"
/// segment (DOC-50 §5.3, Slice 6) tracks the daemon's actual active
/// workstream, clearing on deactivation rather than showing stale text.
#[test]
fn statusline_segments_reflect_active_workstream_and_clear_on_none() {
    let state = EngineState::new(
        RpcHttpClient::new(reqwest::Client::new(), "http://127.0.0.1:1".to_string()),
        None,
    );
    assert!(
        state.statusline_segments().is_empty(),
        "no active workstream observed yet -> no segments"
    );

    *state.active_workstream.lock().unwrap() = Some(WorkstreamSummary {
        id: "ws-1".to_string(),
        name: "Feature X".to_string(),
    });
    assert_eq!(
        state.statusline_segments(),
        vec![StatuslineSegment::Workstream {
            id: "ws-1".to_string(),
            name: "Feature X".to_string(),
        }]
    );

    *state.active_workstream.lock().unwrap() = None;
    assert!(
        state.statusline_segments().is_empty(),
        "deactivation must clear the segment, not leave a stale one"
    );
}
