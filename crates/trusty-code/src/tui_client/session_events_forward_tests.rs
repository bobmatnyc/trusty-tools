//! Producer-side tests for [`super::forward_session_event`]'s tool-failure
//! mapping (#4596).
//!
//! Why: the TUI renders a failed tool call from
//! `ReplEvent::ToolInvocation`'s `failed` flag, so this file is what pins
//! that the flag is actually set. Before #4596 the only failure signal was
//! the `FAILED: `/`ERROR: ` prefix in the result text, and nothing on this
//! side of the seam exercised an unsuccessful call at all —
//! `engine_tests.rs` covers `success: true` only, so a producer reword
//! would have shipped green while the TUI hid the error behind a collapsed
//! `· ok` card.
//! What: one test per unsuccessful path (`ToolFinished { success: false }`,
//! `ToolError`) plus the success and in-flight cases the flag must NOT
//! claim. The name is `session_events_forward_tests`, not
//! `session_events_tests` — that one already belongs to `engine_state.rs`'s
//! reconnect suite (#6637).

use tokio::sync::mpsc::unbounded_channel;
use trusty_code_tui::ReplEvent;

use super::forward_session_event;
use crate::events::{Event, SessionEventEnvelope};

fn envelope(event: Event) -> SessionEventEnvelope {
    SessionEventEnvelope::new("s-1".to_string(), 1, chrono::Utc::now(), event)
}

/// Forward `event` and return the single `ToolInvocation` it produced, as
/// `(result, failed)`.
fn forward_tool(event: Event) -> (Option<String>, bool) {
    let (tx, mut rx) = unbounded_channel();
    forward_session_event(envelope(event), &tx);
    match rx.try_recv().expect("a ToolInvocation event") {
        ReplEvent::ToolInvocation { result, failed, .. } => (result, failed),
        other => panic!("unexpected {other:?}"),
    }
}

fn tool_finished(success: bool) -> Event {
    Event::ToolFinished {
        session_id: "s-1".into(),
        agent: "pm".into(),
        agent_id: String::new(),
        tool: "bash".into(),
        call_id: "call-1".into(),
        success,
        result_preview: "1 test failed".into(),
    }
}

/// The gap #4596 closed: an unsuccessful `ToolFinished` reports the failure
/// as data, not only as a human-readable prefix.
#[test]
fn tool_finished_unsuccessful_sets_the_failed_flag() {
    let (result, failed) = forward_tool(tool_finished(false));
    assert!(failed, "an unsuccessful call must say so in data");
    assert_eq!(
        result.as_deref(),
        Some("FAILED: 1 test failed"),
        "the prefix stays, as display text"
    );
}

/// A `ToolError` is unconditionally a failure — that variant carries no
/// success field to consult.
#[test]
fn tool_error_sets_the_failed_flag() {
    let (result, failed) = forward_tool(Event::ToolError {
        session_id: "s-1".into(),
        agent: "pm".into(),
        agent_id: String::new(),
        tool: "read_file".into(),
        call_id: "call-2".into(),
        error: "no such file".into(),
    });
    assert!(failed);
    assert_eq!(result.as_deref(), Some("ERROR: no such file"));
}

/// The flag must not fire on a call that succeeded or one still running —
/// a flag that is always true hides nothing and reports nothing.
#[test]
fn successful_and_in_flight_calls_are_not_marked_failed() {
    let (result, failed) = forward_tool(tool_finished(true));
    assert!(!failed);
    assert_eq!(result.as_deref(), Some("1 test failed"), "no prefix added");

    let (result, failed) = forward_tool(Event::ToolStarted {
        session_id: "s-1".into(),
        agent: "pm".into(),
        agent_id: String::new(),
        tool: "bash".into(),
        call_id: "call-3".into(),
        args_preview: "cargo test".into(),
    });
    assert!(!failed, "a call that has not finished has not failed");
    assert!(result.is_none());
}
