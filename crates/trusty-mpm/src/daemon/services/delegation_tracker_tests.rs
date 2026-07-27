//! Tests for the #2864 hook-driven delegation tracker.
//!
//! Why: `delegation_tracker.rs` sits under the 500-SLOC production cap, and its
//! suite is large because the correlation rules it protects are the difference
//! between accurate in-flight tracking and a false "no agents running" report —
//! coverage worth keeping in full. Splitting the tests into this file (scored
//! against the test cap by `scripts/check_line_cap.sh`) keeps both the module
//! and its coverage intact.
//! What: unit coverage of dispatch -> launch -> stop correlation, the
//! async-launch guard, per-session scoping, concurrency independence, and dedup.
//! Test: this file IS the test module
//! (`#[path = "delegation_tracker_tests.rs"] mod tests;`).

use super::*;
use crate::core::session::{ControlModel, Session, SessionStatus};
use crate::daemon::idle_nudge::has_live_children;
use std::sync::Arc;

fn state_with_session() -> (Arc<DaemonState>, SessionId) {
    let state = Arc::new(DaemonState::new());
    let id = SessionId::new();
    let mut s = Session::new(id, "/tmp/p", ControlModel::Tmux, None);
    s.status = SessionStatus::Active;
    state.register_session(s);
    (state, id)
}

/// A `PreToolUse` dispatch payload as `tm hook` forwards it.
fn pre(tool: &str, agent: &str, desc: &str, tool_use_id: &str) -> Value {
    serde_json::json!({
        "tool": tool,
        "cwd": "/tmp/p",
        "transcript_path": "/tmp/p/parent.jsonl",
        "tool_use_id": tool_use_id,
        "input": { "subagent_type": agent, "description": desc, "prompt": "do it" }
    })
}

/// A `PostToolUse` async-launch payload (the real Claude Code shape).
fn post_async(tool: &str, tool_use_id: &str, agent_id: &str) -> Value {
    serde_json::json!({
        "tool": tool,
        "tool_use_id": tool_use_id,
        "tool_response": {
            "isAsync": true,
            "status": "async_launched",
            "agentId": agent_id,
            "resolvedModel": "claude-haiku-4-5-20251001"
        }
    })
}

fn stop(agent_id: &str) -> Value {
    serde_json::json!({ "agent_id": agent_id, "agent_type": "general-purpose" })
}

fn only(state: &DaemonState, session: SessionId) -> Delegation {
    let all = state.delegations_for(session);
    assert_eq!(all.len(), 1, "expected exactly one delegation, got {all:?}");
    all.into_iter().next().expect("one")
}

#[test]
fn ignores_unrelated_tools() {
    let (state, sid) = state_with_session();
    let payload = serde_json::json!({ "tool": "Bash", "input": { "command": "ls" } });
    observe(&state, sid, HookEvent::PreToolUse, &payload);
    assert!(state.delegations_for(sid).is_empty());
}

#[test]
fn pre_tool_use_creates_running_delegation() {
    let (state, sid) = state_with_session();
    observe(
        &state,
        sid,
        HookEvent::PreToolUse,
        &pre("Agent", "engineer", "build the thing", "toolu_1"),
    );
    let d = only(&state, sid);
    assert_eq!(d.status, DelegationStatus::Running);
    assert_eq!(d.source, DelegationSource::HookObserved);
    assert_eq!(d.agent, "engineer");
    assert_eq!(d.task, "build the thing");
    assert_eq!(d.tool_use_id.as_deref(), Some("toolu_1"));
    assert_eq!(d.cwd, Some(std::path::PathBuf::from("/tmp/p")));
    assert!(d.started_at.is_some());
}

#[test]
fn legacy_task_tool_name_is_tracked() {
    // Older Claude Code names the dispatch tool `Task`, not `Agent`.
    let (state, sid) = state_with_session();
    observe(
        &state,
        sid,
        HookEvent::PreToolUse,
        &pre("Task", "qa", "verify", "toolu_t"),
    );
    assert_eq!(only(&state, sid).status, DelegationStatus::Running);
}

#[test]
fn duplicate_pre_tool_use_is_idempotent() {
    let (state, sid) = state_with_session();
    let p = pre("Agent", "engineer", "task", "toolu_1");
    observe(&state, sid, HookEvent::PreToolUse, &p);
    observe(&state, sid, HookEvent::PreToolUse, &p);
    assert_eq!(state.delegations_for(sid).len(), 1);
}

#[test]
fn async_launch_keeps_delegation_running() {
    // THE critical regression: PostToolUse fires ~1ms after launch with
    // status=async_launched while the subagent is still running. It must
    // record agentId and must NOT terminalize.
    let (state, sid) = state_with_session();
    observe(
        &state,
        sid,
        HookEvent::PreToolUse,
        &pre("Agent", "engineer", "t", "toolu_1"),
    );
    observe(
        &state,
        sid,
        HookEvent::PostToolUse,
        &post_async("Agent", "toolu_1", "a403cdbc"),
    );
    let d = only(&state, sid);
    assert_eq!(
        d.status,
        DelegationStatus::Running,
        "an async_launched dispatch must stay Running — terminalizing here \
         reports 'no agents in flight' while they are all still running"
    );
    assert_eq!(d.agent_id.as_deref(), Some("a403cdbc"));
    assert_eq!(d.tier, ModelTier::Haiku, "tier refined from resolvedModel");
    assert!(has_live_children(&state.delegations_for(sid)));
}

#[test]
fn synchronous_post_tool_use_completes_delegation() {
    // A dispatch that returns synchronously (no isAsync marker) IS complete.
    let (state, sid) = state_with_session();
    observe(
        &state,
        sid,
        HookEvent::PreToolUse,
        &pre("Task", "qa", "t", "toolu_s"),
    );
    let payload = serde_json::json!({
        "tool": "Task",
        "tool_use_id": "toolu_s",
        "tool_response": { "is_error": false }
    });
    observe(&state, sid, HookEvent::PostToolUse, &payload);
    let d = only(&state, sid);
    assert_eq!(d.status, DelegationStatus::Completed);
    assert!(d.ended_at.is_some());
}

#[test]
fn post_tool_use_failure_marks_failed() {
    let (state, sid) = state_with_session();
    observe(
        &state,
        sid,
        HookEvent::PreToolUse,
        &pre("Agent", "qa", "t", "toolu_f"),
    );
    let payload = serde_json::json!({ "tool": "Agent", "tool_use_id": "toolu_f" });
    observe(&state, sid, HookEvent::PostToolUseFailure, &payload);
    assert_eq!(only(&state, sid).status, DelegationStatus::Failed);
}

#[test]
fn subagent_stop_completes_matching_delegation() {
    let (state, sid) = state_with_session();
    observe(
        &state,
        sid,
        HookEvent::PreToolUse,
        &pre("Agent", "engineer", "t", "toolu_1"),
    );
    observe(
        &state,
        sid,
        HookEvent::PostToolUse,
        &post_async("Agent", "toolu_1", "a403cdbc"),
    );
    observe(&state, sid, HookEvent::SubagentStop, &stop("a403cdbc"));
    let d = only(&state, sid);
    assert_eq!(d.status, DelegationStatus::Completed);
    assert!(d.ended_at.is_some());
    assert!(!has_live_children(&state.delegations_for(sid)));
}

#[test]
fn subagent_stop_failure_marks_failed() {
    let (state, sid) = state_with_session();
    observe(
        &state,
        sid,
        HookEvent::PreToolUse,
        &pre("Agent", "e", "t", "toolu_1"),
    );
    observe(
        &state,
        sid,
        HookEvent::PostToolUse,
        &post_async("Agent", "toolu_1", "aid1"),
    );
    observe(&state, sid, HookEvent::SubagentStopFailure, &stop("aid1"));
    assert_eq!(only(&state, sid).status, DelegationStatus::Failed);
}

#[test]
fn subagent_stop_without_agent_id_terminalizes_nothing() {
    // No correlation key => no guess. Closing "the most recent" would close
    // the wrong delegation under concurrency and manufacture a false idle.
    let (state, sid) = state_with_session();
    observe(
        &state,
        sid,
        HookEvent::PreToolUse,
        &pre("Agent", "engineer", "t", "toolu_1"),
    );
    observe(
        &state,
        sid,
        HookEvent::PostToolUse,
        &post_async("Agent", "toolu_1", "a403cdbc"),
    );
    observe(
        &state,
        sid,
        HookEvent::SubagentStop,
        &serde_json::json!({ "transcript_path": "/tmp/p/parent.jsonl" }),
    );
    assert_eq!(only(&state, sid).status, DelegationStatus::Running);
    assert!(has_live_children(&state.delegations_for(sid)));
}

#[test]
fn subagent_stop_with_unknown_agent_id_is_a_noop() {
    let (state, sid) = state_with_session();
    observe(
        &state,
        sid,
        HookEvent::PreToolUse,
        &pre("Agent", "e", "t", "toolu_1"),
    );
    observe(
        &state,
        sid,
        HookEvent::PostToolUse,
        &post_async("Agent", "toolu_1", "known"),
    );
    observe(&state, sid, HookEvent::SubagentStop, &stop("stranger"));
    assert_eq!(only(&state, sid).status, DelegationStatus::Running);
}

#[test]
fn subagent_stop_is_idempotent() {
    let (state, sid) = state_with_session();
    observe(
        &state,
        sid,
        HookEvent::PreToolUse,
        &pre("Agent", "e", "t", "toolu_1"),
    );
    observe(
        &state,
        sid,
        HookEvent::PostToolUse,
        &post_async("Agent", "toolu_1", "aid1"),
    );
    observe(&state, sid, HookEvent::SubagentStop, &stop("aid1"));
    let first = only(&state, sid).ended_at;
    observe(&state, sid, HookEvent::SubagentStop, &stop("aid1"));
    assert_eq!(only(&state, sid).ended_at, first, "must not re-terminalize");
}

#[test]
fn concurrent_delegations_terminalize_independently() {
    // Two subagents in flight; stopping one must not close the other.
    let (state, sid) = state_with_session();
    observe(
        &state,
        sid,
        HookEvent::PreToolUse,
        &pre("Agent", "alpha", "a", "toolu_a"),
    );
    observe(
        &state,
        sid,
        HookEvent::PreToolUse,
        &pre("Agent", "beta", "b", "toolu_b"),
    );
    observe(
        &state,
        sid,
        HookEvent::PostToolUse,
        &post_async("Agent", "toolu_a", "aid_alpha"),
    );
    observe(
        &state,
        sid,
        HookEvent::PostToolUse,
        &post_async("Agent", "toolu_b", "aid_beta"),
    );
    assert_eq!(state.delegations_for(sid).len(), 2);

    observe(&state, sid, HookEvent::SubagentStop, &stop("aid_alpha"));

    let all = state.delegations_for(sid);
    let alpha = all.iter().find(|d| d.agent == "alpha").expect("alpha");
    let beta = all.iter().find(|d| d.agent == "beta").expect("beta");
    assert_eq!(alpha.status, DelegationStatus::Completed);
    assert_eq!(
        beta.status,
        DelegationStatus::Running,
        "terminalizing alpha must NOT close the concurrently-running beta"
    );
    assert!(
        has_live_children(&all),
        "beta is still live, so the session must not look idle"
    );

    observe(&state, sid, HookEvent::SubagentStop, &stop("aid_beta"));
    assert!(!has_live_children(&state.delegations_for(sid)));
}

#[test]
fn dedups_declaration_and_observation() {
    // A PM that calls agent_delegate AND dispatches natively must yield ONE
    // record, promoted in place to Running — not two.
    let (state, sid) = state_with_session();
    let declared = Delegation::new(sid, None, "engineer", ModelTier::Opus, "declared task");
    let declared_id = declared.id;
    state.upsert_delegation(declared);

    observe(
        &state,
        sid,
        HookEvent::PreToolUse,
        &pre("Agent", "engineer", "declared task", "toolu_1"),
    );

    let d = only(&state, sid);
    assert_eq!(d.id, declared_id, "must reuse the declared record");
    assert_eq!(d.status, DelegationStatus::Running);
    assert_eq!(d.tool_use_id.as_deref(), Some("toolu_1"));
    assert_eq!(d.source, DelegationSource::McpDeclared);

    // And it still terminalizes correctly through the full chain.
    observe(
        &state,
        sid,
        HookEvent::PostToolUse,
        &post_async("Agent", "toolu_1", "aid1"),
    );
    observe(&state, sid, HookEvent::SubagentStop, &stop("aid1"));
    assert_eq!(only(&state, sid).status, DelegationStatus::Completed);
}

#[test]
fn dedup_ignores_different_agent() {
    let (state, sid) = state_with_session();
    state.upsert_delegation(Delegation::new(
        sid,
        None,
        "research",
        ModelTier::Sonnet,
        "investigate",
    ));
    observe(
        &state,
        sid,
        HookEvent::PreToolUse,
        &pre("Agent", "engineer", "build", "toolu_1"),
    );
    assert_eq!(
        state.delegations_for(sid).len(),
        2,
        "a different agent is a different delegation"
    );
}

#[test]
fn dedup_window_expires() {
    // A stale Queued declaration must not swallow a much later dispatch.
    let (state, sid) = state_with_session();
    let mut old = Delegation::new(sid, None, "engineer", ModelTier::Sonnet, "old");
    old.created_at = Utc::now() - chrono::Duration::seconds(DEDUP_WINDOW_SECS + 60);
    state.upsert_delegation(old);

    observe(
        &state,
        sid,
        HookEvent::PreToolUse,
        &pre("Agent", "engineer", "new", "toolu_1"),
    );
    assert_eq!(state.delegations_for(sid).len(), 2);
}

#[test]
fn dedup_does_not_steal_an_already_bound_record() {
    // Two dispatches to the same agent must stay two records.
    let (state, sid) = state_with_session();
    observe(
        &state,
        sid,
        HookEvent::PreToolUse,
        &pre("Agent", "engineer", "first", "toolu_1"),
    );
    observe(
        &state,
        sid,
        HookEvent::PreToolUse,
        &pre("Agent", "engineer", "second", "toolu_2"),
    );
    assert_eq!(state.delegations_for(sid).len(), 2);
}

#[test]
fn delegations_are_scoped_per_session() {
    // A stop in one session must never terminalize another session's child.
    let (state, sid_a) = state_with_session();
    let sid_b = SessionId::new();
    let mut s = Session::new(sid_b, "/tmp/q", ControlModel::Tmux, None);
    s.status = SessionStatus::Active;
    state.register_session(s);

    for sid in [sid_a, sid_b] {
        observe(
            &state,
            sid,
            HookEvent::PreToolUse,
            &pre("Agent", "engineer", "t", "toolu_shared"),
        );
        observe(
            &state,
            sid,
            HookEvent::PostToolUse,
            &post_async("Agent", "toolu_shared", "aid_shared"),
        );
    }
    observe(&state, sid_a, HookEvent::SubagentStop, &stop("aid_shared"));

    assert_eq!(only(&state, sid_a).status, DelegationStatus::Completed);
    assert_eq!(
        only(&state, sid_b).status,
        DelegationStatus::Running,
        "a stop in session A must not touch session B"
    );
}

#[test]
fn missing_subagent_type_still_tracks_as_live() {
    // Fail-safe: an unparseable dispatch must still count as a live child
    // rather than vanish (vanishing would under-report work in flight).
    let (state, sid) = state_with_session();
    let payload = serde_json::json!({ "tool": "Agent", "tool_use_id": "toolu_x" });
    observe(&state, sid, HookEvent::PreToolUse, &payload);
    let d = only(&state, sid);
    assert_eq!(d.agent, "unknown");
    assert_eq!(d.status, DelegationStatus::Running);
}
