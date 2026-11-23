//! Daemon half of the #11674 round trip: dispatch binding and stop join.

use super::*;
use crate::core::hook::HookEvent;
use crate::core::session::{ControlModel, Session, SessionStatus};
use crate::core::subagent_task_roundtrip::{Outcome, classify};
use crate::daemon::services::delegation_tracker::observe;
use std::sync::Arc;

fn state_with_session() -> (Arc<DaemonState>, SessionId) {
    let state = Arc::new(DaemonState::new());
    let id = SessionId::new();
    let mut s = Session::new(id, "/tmp/p", ControlModel::Tmux, None);
    s.status = SessionStatus::Active;
    state.register_session(s);
    (state, id)
}

fn dispatch(state: &DaemonState, sid: SessionId, tool_use_id: &str, prompt: &str) {
    let payload = serde_json::json!({
        "tool": "Agent",
        "cwd": "/tmp/p",
        "tool_use_id": tool_use_id,
        "input": { "subagent_type": "rust-engineer", "description": tool_use_id, "prompt": prompt }
    });
    observe(state, sid, HookEvent::PreToolUse, &payload);
}

fn launched(state: &DaemonState, sid: SessionId, tool_use_id: &str, agent_id: &str) {
    let payload = serde_json::json!({
        "tool": "Agent",
        "tool_use_id": tool_use_id,
        "tool_response": { "isAsync": true, "status": "async_launched", "agentId": agent_id }
    });
    observe(state, sid, HookEvent::PostToolUse, &payload);
}

/// Process a stop the way `ingest_hook` does, then judge `message`.
fn stop(
    state: &DaemonState,
    sid: SessionId,
    payload: Value,
    message: &str,
) -> (StopBinding, Outcome) {
    let (agent_id, tool_use_id) = stop_keys(&payload);
    observe(state, sid, HookEvent::SubagentStop, &payload);
    let b = stop_binding(state, sid, agent_id.as_deref(), tool_use_id.as_deref());
    let ev = classify(Some(&b), agent_id.as_deref(), Some(message), "t".into(), "");
    (b, ev.outcome)
}

#[test]
fn subagent_task_roundtrip_dispatch_records_the_bound_task() {
    let (state, sid) = state_with_session();
    dispatch(
        &state,
        sid,
        "toolu_a",
        "Task: ti-T03@1123\nGoal: ti-G01\nFix #11040.",
    );
    dispatch(
        &state,
        sid,
        "toolu_b",
        "Goal: ti-G01\nFix #11040 (ar-Ta9@1123).",
    );
    let all = state.delegations_for(sid);
    let a = all
        .iter()
        .find(|d| d.tool_use_id.as_deref() == Some("toolu_a"));
    let b = all
        .iter()
        .find(|d| d.tool_use_id.as_deref() == Some("toolu_b"));
    assert_eq!(
        a.and_then(|d| d.ledger_task.as_deref()),
        Some("ti-T03@1123")
    );
    assert_eq!(
        a.map(|d| d.task.as_str()),
        Some("toolu_a"),
        "task stays the description"
    );
    assert_eq!(
        b.and_then(|d| d.ledger_task.clone()),
        None,
        "goal/quoted ids never bind"
    );
}

#[test]
fn subagent_task_roundtrip_concurrent_stops_in_reverse_order() {
    let (state, sid) = state_with_session();
    dispatch(&state, sid, "toolu_a", "Task: ti-T03@1123\ndo a");
    dispatch(&state, sid, "toolu_b", "Task: ti-T04@1123\ndo b");
    launched(&state, sid, "toolu_a", "aid_a");
    launched(&state, sid, "toolu_b", "aid_b");
    let (b, out_b) = stop(
        &state,
        sid,
        serde_json::json!({ "agent_id": "aid_b" }),
        "done\nTask: ti-T04@1123 | done: b",
    );
    let (a, out_a) = stop(
        &state,
        sid,
        serde_json::json!({ "agent_id": "aid_a" }),
        "done\nTask: ti-T04@1123 | done: a used b's id",
    );
    assert_eq!(b.tool_use_id.as_deref(), Some("toolu_b"));
    assert_eq!(b.ledger_task.as_deref(), Some("ti-T04@1123"));
    assert_eq!(out_b, Outcome::Ok);
    assert_eq!(a.tool_use_id.as_deref(), Some("toolu_a"));
    assert_eq!(a.ledger_task.as_deref(), Some("ti-T03@1123"));
    assert_eq!(out_a, Outcome::Mismatch);
}

#[test]
fn subagent_task_roundtrip_foreground_stop_joins_by_tool_use_id() {
    // A foreground dispatch's PostToolUse lands after its stop: no agent_id yet.
    let (state, sid) = state_with_session();
    dispatch(&state, sid, "toolu_fg", "Task: rv-T0c@1123\nwork");
    let payload = serde_json::json!({ "agent_id": "aid_fg", "dispatch_tool_use_id": "toolu_fg" });
    let (b, out) = stop(&state, sid, payload, "result\nTask: rv-T0c@1123 | done: x");
    assert!(b.delegation_found);
    assert_eq!(out, Outcome::Ok);
}

#[test]
fn subagent_task_roundtrip_unknown_stop_is_unbound_not_ok() {
    let (state, sid) = state_with_session();
    let (b, out) = stop(
        &state,
        sid,
        serde_json::json!({ "agent_id": "nobody" }),
        "Task: rv-T0c@1123 | done: x",
    );
    assert!(!b.delegation_found);
    assert_eq!(out, Outcome::Unbound);
}
