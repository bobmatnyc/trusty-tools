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
use crate::tui_client::session_events::{forward_session_event, terminal_stream_failure_event};
use crate::tui_client::workstream_subscription::parse_workstream_envelope;
use trusty_code_tui::{DelegationOutcome, StatuslineSegment, WorkstreamSummary};

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

/// tcode streaming epic #3696 Slice 2, re-keyed by #7940: a non-final
/// `AgentMessageDelta` (`done: false`) must forward as an `AgentOutput`
/// chunk carrying the producer's `(agent_id, turn_id)` pair, which is what
/// the reducer opens a per-stream bubble on.
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
        ReplEvent::AgentOutput {
            agent_id: "agent-1".into(),
            turn_id: "turn-1".into(),
            chunk: "Hello".into(),
            done: false,
        }
    );
}

/// The final delta (`done: true`) for a turn must forward `done: true` (so
/// the reducer finalizes that stream's bubble) — and, unlike `SessionDone`,
/// is NOT terminal for the SSE pump itself: one agent's turn finishing
/// doesn't mean the whole session stream is over.
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
        ReplEvent::AgentOutput {
            agent_id: "agent-1".into(),
            turn_id: "turn-1".into(),
            chunk: " world".into(),
            done: true,
        }
    );
}

/// Two agents streaming concurrently and sharing a `turn_id` (the Slice 0
/// contract's defensive `(agent_id, turn_id)` grouping scenario) must each
/// forward carrying their OWN `agent_id` — `forward_session_event` never
/// accumulates or merges chunks itself; it is a stateless 1:1 mapper, and
/// the id it preserves is what lets the reducer keep the two apart. The
/// reducer half of that guarantee is
/// `trusty_code_tui::app::reduce::tests::agent_output_keys_concurrent_streams_separately`.
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
        ReplEvent::AgentOutput {
            agent_id: "agent-1".into(),
            turn_id: "turn-shared".into(),
            chunk: "from-agent-1".into(),
            done: false,
        },
        "agent-1's delta must keep its own agent_id, not merge with agent-2's"
    );
    assert_eq!(
        rx.try_recv().expect("event"),
        ReplEvent::AgentOutput {
            agent_id: "agent-2".into(),
            turn_id: "turn-shared".into(),
            chunk: "from-agent-2".into(),
            done: false,
        },
        "agent-2's delta must keep its own agent_id, not merge with agent-1's"
    );
}

// ── #7940: delegation brackets ────────────────────────────────────────────

/// The daemon's `AgentSpawned` must open a delegation block carrying the
/// spawn's own `agent_id` — the key every later tool/message event of that
/// sub-agent's run is attributed with.
#[test]
fn forward_agent_spawned_opens_a_delegation() {
    let (tx, mut rx) = unbounded_channel();
    let terminal = forward_session_event(
        envelope(Event::AgentSpawned {
            session_id: "s-1".into(),
            agent: "engineer".into(),
            agent_id: "spawn-7".into(),
            task_preview: "add the delegation renderer".into(),
        }),
        &tx,
    );
    assert!(!terminal);
    assert_eq!(
        rx.try_recv().expect("event"),
        ReplEvent::DelegationStarted {
            agent_id: "spawn-7".into(),
            agent: "engineer".into(),
            task: "add the delegation renderer".into(),
        }
    );
}

/// `PmDelegating` is the delegating agent's ANNOUNCEMENT and carries no
/// `agent_id` — it must still open a block (with an empty id) so the task
/// summary is visible before the sub-agent exists; the reducer adopts that
/// block when the real `AgentSpawned` lands.
#[test]
fn forward_pm_delegating_opens_a_delegation_without_an_id() {
    let (tx, mut rx) = unbounded_channel();
    forward_session_event(
        envelope(Event::PmDelegating {
            session_id: "s-1".into(),
            agent: "engineer".into(),
            task_preview: "add the delegation renderer".into(),
        }),
        &tx,
    );
    assert_eq!(
        rx.try_recv().expect("event"),
        ReplEvent::DelegationStarted {
            agent_id: String::new(),
            agent: "engineer".into(),
            task: "add the delegation renderer".into(),
        }
    );
}

/// `AgentDone` closes the block with a `Finished` outcome, and — like an
/// agent turn ending — is NOT terminal for the session stream.
#[test]
fn forward_agent_done_closes_the_delegation() {
    let (tx, mut rx) = unbounded_channel();
    let terminal = forward_session_event(
        envelope(Event::AgentDone {
            session_id: "s-1".into(),
            agent: "engineer".into(),
            agent_id: "spawn-7".into(),
            status: "success".into(),
        }),
        &tx,
    );
    assert!(
        !terminal,
        "a delegation finishing must not end the session stream"
    );
    assert_eq!(
        rx.try_recv().expect("event"),
        ReplEvent::DelegationFinished {
            agent_id: "spawn-7".into(),
            agent: "engineer".into(),
            outcome: DelegationOutcome::Finished("success".into()),
        }
    );
}

/// `AgentFailed` closes the block carrying the error text — the whole point
/// of a two-variant outcome is that a failed delegation cannot render the
/// same as a finished one.
#[test]
fn forward_agent_failed_closes_with_the_error() {
    let (tx, mut rx) = unbounded_channel();
    forward_session_event(
        envelope(Event::AgentFailed {
            session_id: "s-1".into(),
            agent: "engineer".into(),
            agent_id: "spawn-7".into(),
            error: "turn cap exceeded".into(),
        }),
        &tx,
    );
    assert_eq!(
        rx.try_recv().expect("event"),
        ReplEvent::DelegationFinished {
            agent_id: "spawn-7".into(),
            agent: "engineer".into(),
            outcome: DelegationOutcome::Failed("turn cap exceeded".into()),
        }
    );
}

/// #3422: `permission_requested` must reach the TUI field for field. Until
/// this mapping existed the event fell into `forward_session_event`'s
/// catch-all, so a tool call suspended on the daemon looked like the TUI had
/// simply stopped responding.
#[test]
fn forward_permission_requested_opens_the_prompt() {
    let (tx, mut rx) = unbounded_channel();
    forward_session_event(
        envelope(Event::PermissionRequested {
            session_id: "s-1".into(),
            request_id: "req-9".into(),
            agent: "python-engineer".into(),
            agent_id: "spawn-7".into(),
            tool: "bash".into(),
            subject: "rm -rf build".into(),
            rule: "bash[rm *]".into(),
        }),
        &tx,
    );
    assert_eq!(
        rx.try_recv().expect("event"),
        ReplEvent::PermissionRequested {
            request_id: "req-9".into(),
            agent: "python-engineer".into(),
            agent_id: "spawn-7".into(),
            tool: "bash".into(),
            subject: "rm -rf build".into(),
            rule: "bash[rm *]".into(),
        }
    );
}

/// #3422: `permission_resolved` is the daemon's own close for the prompt, so
/// its `decision`/`source` words cross the seam verbatim — the TUI renders
/// them and parses neither.
#[test]
fn forward_permission_resolved_closes_the_prompt() {
    let (tx, mut rx) = unbounded_channel();
    forward_session_event(
        envelope(Event::PermissionResolved {
            session_id: "s-1".into(),
            request_id: "req-9".into(),
            agent: "python-engineer".into(),
            agent_id: "spawn-7".into(),
            decision: "allow_for_session".into(),
            source: "client".into(),
        }),
        &tx,
    );
    assert_eq!(
        rx.try_recv().expect("event"),
        ReplEvent::PermissionResolved {
            request_id: "req-9".into(),
            agent: "python-engineer".into(),
            agent_id: "spawn-7".into(),
            decision: "allow_for_session".into(),
            source: "client".into(),
        }
    );
}

/// Classify every [`Event`] variant as agent-attributed or not (#7940).
///
/// Why: `forward_session_event`'s `_ => false` catch-all silently drops
/// anything it has no arm for, which is exactly how `PmDelegating`/
/// `AgentSpawned` went unrendered for as long as they did. This `match` has
/// NO catch-all, so a new `Event` variant fails to COMPILE here until
/// someone decides which side it falls on — that compile break, not the
/// runtime assertions below, is what stops the next agent-attributed
/// variant from being dropped unnoticed.
/// What: returns `Some(kind)` for a variant carrying an `agent` field,
/// `None` otherwise.
fn agent_attributed_kind(event: &Event) -> Option<&'static str> {
    match event {
        // Agent-attributed: every variant carrying an `agent` field.
        Event::ToolStarted { .. }
        | Event::ToolFinished { .. }
        | Event::ToolError { .. }
        | Event::SearchPerformed { .. }
        | Event::MemoryRecalled { .. }
        | Event::PmDelegating { .. }
        | Event::AgentSpawned { .. }
        | Event::AgentMessage { .. }
        | Event::AgentMessageDelta { .. }
        | Event::AgentDone { .. }
        | Event::AgentFailed { .. }
        // (#7948) Both carry `agent`/`agent_id` so a client can route a
        // permission prompt to the delegation that raised it.
        | Event::PermissionRequested { .. }
        | Event::PermissionResolved { .. } => Some(event.kind()),
        // Not agent-attributed. `AgentStarted`/`ReportGenerated` carry an
        // `agent_name`, not an `agent`, and neither has a producer on this
        // daemon's session path.
        Event::AgentStarted { .. }
        | Event::SessionStarted { .. }
        | Event::SessionDone { .. }
        | Event::SessionCancelled { .. }
        | Event::SessionStatusChanged { .. }
        | Event::SessionInput { .. }
        | Event::Log { .. }
        | Event::Progress { .. }
        | Event::Message { .. }
        | Event::PmThinking { .. }
        | Event::ToolCalled { .. }
        | Event::ToolResult { .. }
        | Event::AstOperation { .. }
        | Event::PhaseStarted { .. }
        | Event::PhaseDone { .. }
        | Event::PhaseSkipped { .. }
        | Event::PersonaDetected { .. }
        | Event::LlmRequested { .. }
        | Event::LlmResponded { .. }
        | Event::ReportGenerated { .. }
        | Event::RecapGenerated { .. }
        | Event::IndexReadiness { .. }
        | Event::ContextBudget { .. }
        | Event::SessionAdded { .. }
        | Event::SessionActivityUpdate { .. }
        | Event::WorkstreamActivationChanged { .. }
        | Event::WorkstreamStateInferred { .. }
        | Event::Ping => None,
    }
}

/// Agent-attributed kinds this client deliberately does NOT render, with the
/// reason (#7940). An entry here is a decision, not an oversight.
const INTENTIONALLY_IGNORED: &[(&str, &str)] = &[
    (
        "search_performed",
        "search telemetry; the tool_started/tool_finished pair for the same call already renders",
    ),
    (
        "memory_recalled",
        "recall telemetry; same reason as search_performed",
    ),
    // #3422 removed `permission_requested`/`permission_resolved` from this
    // list: both now map, and
    // `forward_permission_requested_opens_the_prompt` /
    // `forward_permission_resolved_closes_the_prompt` pin the mapping.
];

/// Every agent-attributed `Event` the daemon can emit during a delegation
/// must map to at least one `ReplEvent`, or be named in
/// [`INTENTIONALLY_IGNORED`] (#7940).
///
/// Why: a delegation's whole value in the TUI is that nothing about it goes
/// missing. Before this, `PmDelegating` and `AgentSpawned` fell into the
/// catch-all and rendered as nothing at all, with no test that could tell.
#[test]
fn every_agent_attributed_event_maps_or_is_explicitly_ignored() {
    for event in delegation_event_samples() {
        let Some(kind) = agent_attributed_kind(&event) else {
            panic!("sample {} is not agent-attributed", event.kind());
        };
        let (tx, mut rx) = unbounded_channel();
        forward_session_event(envelope(event), &tx);
        let mapped = rx.try_recv().is_ok();
        let ignored = INTENTIONALLY_IGNORED.iter().any(|(k, _)| *k == kind);
        assert!(
            mapped != ignored,
            "`{kind}` must either map to a ReplEvent or be listed in INTENTIONALLY_IGNORED \
             (mapped={mapped}, ignored={ignored})"
        );
    }
}

/// One sample of every agent-attributed `Event` the daemon emits during a
/// delegation (#7940). `agent_attributed_kind`'s exhaustive `match` is what
/// forces a new variant to be classified; this list is what actually drives
/// it through `forward_session_event`.
fn delegation_event_samples() -> Vec<Event> {
    vec![
        Event::PmDelegating {
            session_id: "s-1".into(),
            agent: "engineer".into(),
            task_preview: "t".into(),
        },
        Event::AgentSpawned {
            session_id: "s-1".into(),
            agent: "engineer".into(),
            agent_id: "spawn-1".into(),
            task_preview: "t".into(),
        },
        Event::AgentMessage {
            session_id: "s-1".into(),
            agent: "engineer".into(),
            text: "hi".into(),
        },
        Event::AgentMessageDelta {
            session_id: "s-1".into(),
            agent: "engineer".into(),
            agent_id: "spawn-1".into(),
            turn_id: "turn-1".into(),
            delta: "hi".into(),
            done: false,
        },
        Event::ToolStarted {
            session_id: "s-1".into(),
            agent: "engineer".into(),
            agent_id: "spawn-1".into(),
            tool: "bash".into(),
            call_id: "c1".into(),
            args_preview: "ls".into(),
        },
        Event::ToolFinished {
            session_id: "s-1".into(),
            agent: "engineer".into(),
            agent_id: "spawn-1".into(),
            tool: "bash".into(),
            call_id: "c1".into(),
            success: true,
            result_preview: "ok".into(),
        },
        Event::ToolError {
            session_id: "s-1".into(),
            agent: "engineer".into(),
            agent_id: "spawn-1".into(),
            tool: "bash".into(),
            call_id: "c1".into(),
            error: "boom".into(),
        },
        Event::SearchPerformed {
            session_id: "s-1".into(),
            agent: "engineer".into(),
            agent_id: "spawn-1".into(),
            lane: "lexical".into(),
            query: "q".into(),
            hit_count: Some(1),
            hits: vec![],
            latency_ms: 1,
        },
        Event::MemoryRecalled {
            session_id: "s-1".into(),
            agent: "engineer".into(),
            agent_id: "spawn-1".into(),
            query: "q".into(),
            results: vec![],
        },
        Event::AgentDone {
            session_id: "s-1".into(),
            agent: "engineer".into(),
            agent_id: "spawn-1".into(),
            status: "success".into(),
        },
        Event::AgentFailed {
            session_id: "s-1".into(),
            agent: "engineer".into(),
            agent_id: "spawn-1".into(),
            error: "boom".into(),
        },
        Event::PermissionRequested {
            session_id: "s-1".into(),
            request_id: "req-1".into(),
            agent: "engineer".into(),
            agent_id: "spawn-1".into(),
            tool: "bash".into(),
            subject: "rm -rf build".into(),
            rule: "bash[rm *]".into(),
        },
        Event::PermissionResolved {
            session_id: "s-1".into(),
            request_id: "req-1".into(),
            agent: "engineer".into(),
            agent_id: "spawn-1".into(),
            decision: "deny".into(),
            source: "client".into(),
        },
    ]
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

/// The caches must start empty before `setup()` populates them —
/// `TuiEngine::commands()`/`picker()` must degrade to "nothing yet," never
/// panic, when queried before `setup()` runs.
#[test]
fn caches_are_empty_before_setup() {
    let engine = CodeEngine::with_socket("/nonexistent/tcode.sock", None);
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
    let engine = CodeEngine::with_socket("/nonexistent/tcode.sock", None);
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
    let state = EngineState::new(UdsRpcClient::new("/nonexistent/tcode.sock"), None, false);
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

// ── #8184: the connect line names the agent shape and the working root ──────

/// A bound session's summary names the solo agent and the project root.
#[test]
fn session_shape_summary_names_the_solo_agent_and_root() {
    let summary = session_shape_summary(&json!({
        "no_delegate": true,
        "binding": {"state": "directory", "root": "/tmp/proj"},
    }));
    assert_eq!(
        summary,
        "solo agent (no delegation), file tools rooted at /tmp/proj"
    );
}

/// A projectless session says so, and names the scratch workspace — the TUI's
/// launch directory is NOT where its edits land.
#[test]
fn session_shape_summary_names_the_projectless_scratch_root() {
    let summary = session_shape_summary(&json!({
        "no_delegate": true,
        "binding": {"state": "projectless", "root": null},
    }));
    assert_eq!(
        summary,
        "solo agent (no delegation), projectless — file tools rooted at a scratch workspace"
    );
}

/// `--delegate` reads as the PM, so the two modes are distinguishable on the
/// connect line.
#[test]
fn session_shape_summary_names_the_delegating_pm() {
    let summary = session_shape_summary(&json!({
        "no_delegate": false,
        "binding": {"state": "git_repo", "root": "/tmp/repo"},
    }));
    assert_eq!(summary, "delegating PM, file tools rooted at /tmp/repo");
}

// ── #8207: the cancel outcome the TUI can render ──────────────────────────────

/// `-32010` must reach this client as a NAMED outcome, not an opaque error.
///
/// Why: the daemon mints a domain code for "accepted, not stopped yet" precisely
/// so a client can hold a cancelling state instead of showing a fault — and
/// before this, every client in the repo flattened it. The daemon's own sentence
/// has to survive too: it names what the wait is still on, which is the whole
/// content of a "still cancelling…" line.
/// Test: this test.
#[test]
fn cancel_unconfirmed_is_a_still_cancelling_outcome() {
    let outcome = classify_cancel_error(EngineError::Rpc {
        code: -32010,
        message: "session s-1: cancellation requested but the task did not stop within 10s"
            .to_string(),
        data: None,
    })
    .expect("an unconfirmed cancel is an outcome, not a failure");

    assert_eq!(
        outcome,
        CancelOutcome::StillCancelling {
            detail: "session s-1: cancellation requested but the task did not stop within 10s"
                .to_string()
        }
    );
}

/// Every other refusal stays an error (#8207).
///
/// Why: the split has to be exactly one code wide. Reading a `-32603` daemon
/// fault or a `-32007 session_not_found` as "still cancelling" would hide a real
/// failure behind a spinner that never resolves.
/// Test: this test.
#[test]
fn any_other_refusal_stays_an_error() {
    for code in [-32603, -32007, -32003] {
        let err = classify_cancel_error(EngineError::Rpc {
            code,
            message: "nope".to_string(),
            data: None,
        })
        .expect_err("only -32010 is an outcome");
        assert!(
            matches!(err, EngineError::Rpc { code: got, .. } if got == code),
            "code {code} must pass through verbatim, got {err:?}"
        );
    }
}
