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
use crate::daemon::state::sessions::{
    DECLARED_STALE_AFTER_SECS, DELEGATION_RETENTION_SECS, RUNNING_STALE_AFTER_SECS,
    STALE_RETENTION_SECS,
};
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
    // A dispatch that returns with no launch marker and no `agentId` handle IS
    // complete — there is nothing left for a `SubagentStop` to quote back.
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
        "tool_response": { "isAsync": false, "is_error": false }
    });
    observe(&state, sid, HookEvent::PostToolUse, &payload);
    let d = only(&state, sid);
    assert_eq!(d.status, DelegationStatus::Completed);
    assert!(d.ended_at.is_some());
}

#[test]
fn liveness_silent_response_is_read_as_a_return_known_gap() {
    // KNOWN LIMITATION, asserted so it is visible rather than lurking: a
    // recognized response that says nothing about liveness (`resolvedModel`
    // alone) is treated as a synchronous return. Closing this band requires
    // `on_subagent_stop` to gain a recovery path first — it resolves only by
    // `agent_id`, which only `on_launched` teaches, so a response with no
    // `agentId` has no other route to termination and tightening here would
    // turn a rare fail-open into a guaranteed 6 h phantom "agent in flight".
    // See `classify_dispatch`'s KNOWN LIMITATION note. Unobserved against
    // Claude Code 2.1.220, whose dispatch response takes the `Launched` branch.
    let (state, sid) = state_with_session();
    observe(
        &state,
        sid,
        HookEvent::PreToolUse,
        &pre("Agent", "engineer", "t", "toolu_1"),
    );
    let payload = serde_json::json!({
        "tool": "Agent",
        "tool_use_id": "toolu_1",
        "tool_response": { "resolvedModel": "claude-haiku-4-5-20251001" }
    });
    observe(&state, sid, HookEvent::PostToolUse, &payload);
    let d = only(&state, sid);
    assert_eq!(
        d.status,
        DelegationStatus::Completed,
        "documented residual — change this assertion only together with the \
         stop-side recovery path"
    );
    assert_eq!(d.tier, ModelTier::Haiku, "tier is still refined");
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
    // A stale Queued declaration must not swallow a much later dispatch — even
    // one whose task text is identical, so this isolates the window from the
    // task discriminator.
    let (state, sid) = state_with_session();
    let mut old = Delegation::new(sid, None, "engineer", ModelTier::Sonnet, "same task");
    old.created_at = Utc::now() - chrono::Duration::seconds(DEDUP_WINDOW_SECS + 60);
    state.upsert_delegation(old);

    observe(
        &state,
        sid,
        HookEvent::PreToolUse,
        &pre("Agent", "engineer", "same task", "toolu_1"),
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

// ---- HIGH 1: an unusable `tool_response` must fail CLOSED -----------------

#[test]
fn post_tool_use_without_tool_response_stays_running() {
    // The regression: `is_some_and` on an absent response yielded `false`, so
    // "not async" was inferred from no evidence at all and the delegation was
    // marked Completed ~1ms after launch while the subagent ran on. `tm hook`
    // omits `tool_response` entirely whenever its five-key projection finds
    // nothing it recognises, so this arrives with no bug on our side.
    let (state, sid) = state_with_session();
    observe(
        &state,
        sid,
        HookEvent::PreToolUse,
        &pre("Agent", "engineer", "t", "toolu_1"),
    );
    let payload = serde_json::json!({ "tool": "Agent", "tool_use_id": "toolu_1" });
    observe(&state, sid, HookEvent::PostToolUse, &payload);

    let d = only(&state, sid);
    assert_eq!(
        d.status,
        DelegationStatus::Running,
        "no response is 'unknown', not 'finished' — terminalizing here reports \
         'no agents in flight' while the subagent is still running"
    );
    assert!(d.ended_at.is_none());
    assert!(has_live_children(&state.delegations_for(sid)));
}

#[test]
fn post_tool_use_with_unrecognized_response_stays_running() {
    // A Claude Code key rename or a content-block-array response: an object we
    // cannot interpret. It must be treated exactly like an absent response.
    let (state, sid) = state_with_session();
    observe(
        &state,
        sid,
        HookEvent::PreToolUse,
        &pre("Agent", "engineer", "t", "toolu_1"),
    );
    let payload = serde_json::json!({
        "tool": "Agent",
        "tool_use_id": "toolu_1",
        "tool_response": { "content": [{ "type": "text", "text": "…" }] }
    });
    observe(&state, sid, HookEvent::PostToolUse, &payload);

    assert_eq!(only(&state, sid).status, DelegationStatus::Running);
    assert!(has_live_children(&state.delegations_for(sid)));
}

#[test]
fn post_tool_use_with_only_an_agent_id_stays_running() {
    // The self-contradiction case: storing `agent_id` (whose ONLY purpose is to
    // let a future SubagentStop resolve this record) while marking it Completed
    // in the same closure. `agentId` is by design the ASYNC correlation key, so
    // handing one back is evidence of a launch, never of a return.
    let (state, sid) = state_with_session();
    observe(
        &state,
        sid,
        HookEvent::PreToolUse,
        &pre("Agent", "engineer", "t", "toolu_1"),
    );
    let payload = serde_json::json!({
        "tool": "Agent",
        "tool_use_id": "toolu_1",
        "tool_response": { "agentId": "aid_p1" }
    });
    observe(&state, sid, HookEvent::PostToolUse, &payload);

    let d = only(&state, sid);
    assert_eq!(d.status, DelegationStatus::Running);
    assert_eq!(
        d.agent_id.as_deref(),
        Some("aid_p1"),
        "the join key is still learned — it is just not read as completion"
    );
    // …and the stop it implies still resolves the record.
    observe(&state, sid, HookEvent::SubagentStop, &stop("aid_p1"));
    assert_eq!(only(&state, sid).status, DelegationStatus::Completed);
}

#[test]
fn changed_async_status_value_with_an_agent_id_stays_running() {
    // Value drift, not key drift: `status` keeps its name but stops saying
    // "async_launched". The `agentId` handle is what keeps this safe.
    let (state, sid) = state_with_session();
    observe(
        &state,
        sid,
        HookEvent::PreToolUse,
        &pre("Agent", "engineer", "t", "toolu_1"),
    );
    let payload = serde_json::json!({
        "tool": "Agent",
        "tool_use_id": "toolu_1",
        "tool_response": { "status": "launched", "agentId": "aid_p3" }
    });
    observe(&state, sid, HookEvent::PostToolUse, &payload);
    assert_eq!(only(&state, sid).status, DelegationStatus::Running);
}

#[test]
fn renamed_async_marker_does_not_terminalize() {
    // The specific drift scenario: `isAsync`/`status` renamed but `agentId`
    // kept. We still recognise the response, so we would infer a synchronous
    // return — except the delegation is then closed only if nothing else says
    // otherwise. Assert the safe half explicitly: an *entirely* renamed
    // response is unrecognised and leaves the record alone.
    let (state, sid) = state_with_session();
    observe(
        &state,
        sid,
        HookEvent::PreToolUse,
        &pre("Agent", "engineer", "t", "toolu_1"),
    );
    let payload = serde_json::json!({
        "tool": "Agent",
        "tool_use_id": "toolu_1",
        "tool_response": { "agent_id": "a403", "is_async": true, "state": "async_launched" }
    });
    observe(&state, sid, HookEvent::PostToolUse, &payload);

    let d = only(&state, sid);
    assert_eq!(d.status, DelegationStatus::Running);
    assert!(
        d.agent_id.is_none(),
        "no key we recognise, so nothing learned"
    );
}

// ---- HIGH 2: bounded liveness --------------------------------------------

#[test]
fn stale_running_delegation_stops_suppressing_the_nudge() {
    // A Running delegation whose SubagentStop never arrives (dropped hook POST,
    // interrupted subagent) used to be immortal and suppress this session's
    // idle nudge for the daemon's lifetime.
    let (state, sid) = state_with_session();
    observe(
        &state,
        sid,
        HookEvent::PreToolUse,
        &pre("Agent", "engineer", "t", "toolu_1"),
    );
    assert!(has_live_children(&state.delegations_for(sid)));

    // Well inside the budget: a genuinely long-running agent is untouched.
    let sweep = state.sweep_delegations_at(Utc::now() + chrono::Duration::hours(3));
    assert_eq!(sweep.staled, 0, "a 3h-old agent is still plausibly running");
    assert!(has_live_children(&state.delegations_for(sid)));

    let past = Utc::now() + chrono::Duration::seconds(RUNNING_STALE_AFTER_SECS + 60);
    let sweep = state.sweep_delegations_at(past);
    assert_eq!(sweep.staled, 1);

    let d = only(&state, sid);
    assert_eq!(
        d.status,
        DelegationStatus::Stale,
        "tracking gave up — but it must NOT claim the agent completed"
    );
    assert_ne!(d.status, DelegationStatus::Completed);
    assert!(!has_live_children(&state.delegations_for(sid)));
}

#[test]
fn late_subagent_stop_still_resolves_a_stale_delegation() {
    // Staleness must be recoverable, or a too-short budget would be a one-way
    // false negative for a genuinely long-running agent.
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
        &post_async("Agent", "toolu_1", "aid1"),
    );
    state
        .sweep_delegations_at(Utc::now() + chrono::Duration::seconds(RUNNING_STALE_AFTER_SECS + 1));
    assert_eq!(only(&state, sid).status, DelegationStatus::Stale);

    observe(&state, sid, HookEvent::SubagentStop, &stop("aid1"));
    assert_eq!(
        only(&state, sid).status,
        DelegationStatus::Completed,
        "a late stop replaces 'we lost track' with the truth"
    );
}

#[test]
fn declared_but_never_dispatched_goes_stale_quickly() {
    // `agent_delegate` declares intent and explicitly does not execute (#1942),
    // so an undispatched Queued record is not evidence of a running agent and
    // gets a far shorter budget than a Running one.
    let (state, sid) = state_with_session();
    state.upsert_delegation(Delegation::new(
        sid,
        None,
        "engineer",
        ModelTier::Sonnet,
        "declared",
    ));
    let sweep = state.sweep_delegations_at(
        Utc::now() + chrono::Duration::seconds(DECLARED_STALE_AFTER_SECS + 1),
    );
    assert_eq!(sweep.staled, 1);
    assert_eq!(only(&state, sid).status, DelegationStatus::Stale);
    assert!(!has_live_children(&state.delegations_for(sid)));
}

// ---- MEDIUM 1: bounded growth --------------------------------------------

#[test]
fn terminal_delegations_are_evicted_after_retention() {
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
        &post_async("Agent", "toolu_1", "aid1"),
    );
    observe(&state, sid, HookEvent::SubagentStop, &stop("aid1"));
    assert_eq!(state.delegations_for(sid).len(), 1);

    let sweep = state.sweep_delegations_at(
        Utc::now() + chrono::Duration::seconds(DELEGATION_RETENTION_SECS + 1),
    );
    assert_eq!(sweep.evicted, 1);
    assert!(
        state.delegations_for(sid).is_empty(),
        "the map must not grow monotonically for the daemon's lifetime"
    );
}

#[test]
fn live_delegations_are_never_evicted() {
    // The live branch of the sweep unconditionally retains, at ANY age. At
    // +30 days the record has of course gone Stale (that is the liveness
    // bound), but nothing evicted it in that pass, and it is still there.
    let (state, sid) = state_with_session();
    observe(
        &state,
        sid,
        HookEvent::PreToolUse,
        &pre("Agent", "engineer", "t", "toolu_1"),
    );
    let sweep = state.sweep_delegations_at(Utc::now() + chrono::Duration::days(30));
    assert_eq!(sweep.evicted, 0, "a live record is never evicted");
    assert_eq!(state.delegations_for(sid).len(), 1);
}

#[test]
fn a_stale_delegation_has_no_ended_at() {
    // `ended_at` means "reached a terminal status" and nothing broader. If the
    // sweep stamped it, `Stale` would ride the terminal retention clock and the
    // recovery window would silently collapse from 18 h to 1 h.
    let (state, sid) = state_with_session();
    observe(
        &state,
        sid,
        HookEvent::PreToolUse,
        &pre("Agent", "engineer", "t", "toolu_1"),
    );
    state
        .sweep_delegations_at(Utc::now() + chrono::Duration::seconds(RUNNING_STALE_AFTER_SECS + 1));
    let d = only(&state, sid);
    assert_eq!(d.status, DelegationStatus::Stale);
    assert!(
        d.ended_at.is_none(),
        "the sweep must not claim the delegation ended — it did not end, we \
         stopped trusting the record"
    );
}

#[test]
fn stale_delegation_stays_resolvable_far_past_the_terminal_window() {
    // The guarantee `Stale` is justified by, asserted rather than assumed: it
    // used to be dropped one terminal-retention window (1 h) after staling —
    // ~7 h total — after which a late SubagentStop resolved nothing because
    // there was no record left. It must survive far past that.
    let (state, sid) = state_with_session();
    let t0 = Utc::now();
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
        &post_async("Agent", "toolu_1", "aid1"),
    );
    state.sweep_delegations_at(t0 + chrono::Duration::seconds(RUNNING_STALE_AFTER_SECS + 1));
    assert_eq!(only(&state, sid).status, DelegationStatus::Stale);

    // The old 7 h horizon, and then a full day short of eviction.
    state.sweep_delegations_at(
        t0 + chrono::Duration::seconds(RUNNING_STALE_AFTER_SECS + DELEGATION_RETENTION_SECS + 60),
    );
    assert_eq!(
        state.delegations_for(sid).len(),
        1,
        "the record that used to vanish at ~7 h must still be here"
    );
    let sweep =
        state.sweep_delegations_at(t0 + chrono::Duration::seconds(STALE_RETENTION_SECS - 60));
    assert_eq!(sweep.evicted, 0);

    // …and it is still resolvable, which is the whole point.
    observe(&state, sid, HookEvent::SubagentStop, &stop("aid1"));
    assert_eq!(
        only(&state, sid).status,
        DelegationStatus::Completed,
        "a stop arriving ~24 h later still replaces 'we lost track' with the truth"
    );
}

#[test]
fn a_stale_delegation_is_eventually_evicted() {
    // The recovery window is long, not infinite — the map must stay bounded.
    // This asserts the bound rather than leaving it incidental, and documents
    // exactly what is lost past it.
    let (state, sid) = state_with_session();
    let t0 = Utc::now();
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
        &post_async("Agent", "toolu_1", "aid1"),
    );
    state.sweep_delegations_at(t0 + chrono::Duration::seconds(RUNNING_STALE_AFTER_SECS + 1));

    let sweep =
        state.sweep_delegations_at(t0 + chrono::Duration::seconds(STALE_RETENTION_SECS + 60));
    assert_eq!(sweep.evicted, 1);
    assert!(state.delegations_for(sid).is_empty());

    // Past the bound there is nothing left to resolve — a documented limit,
    // not a surprise.
    observe(&state, sid, HookEvent::SubagentStop, &stop("aid1"));
    assert!(state.delegations_for(sid).is_empty());
}

// ---- MEDIUM 2: dedup must not false-merge --------------------------------

#[test]
fn dedup_does_not_merge_a_different_task() {
    // This repo routinely declares one agent and then dispatches that same
    // agent for different work inside the window. Merging halves the live-child
    // count AND leaves the record showing the declaration's task text — the
    // exact string `/tm-session-pause` shows a human.
    let (state, sid) = state_with_session();
    state.upsert_delegation(Delegation::new(
        sid,
        None,
        "rust-engineer",
        ModelTier::Sonnet,
        "fix the delegation tracker",
    ));
    observe(
        &state,
        sid,
        HookEvent::PreToolUse,
        &pre(
            "Agent",
            "rust-engineer",
            "rebase the release branch",
            "toolu_2",
        ),
    );

    let all = state.delegations_for(sid);
    assert_eq!(all.len(), 2, "different work is a different delegation");
    let observed = all
        .iter()
        .find(|d| d.source == DelegationSource::HookObserved)
        .expect("observed record");
    assert_eq!(
        observed.task, "rebase the release branch",
        "the dispatched task text must survive"
    );
}

#[test]
fn dedup_declines_a_description_less_dispatch() {
    // No description on the dispatch means no discriminator at all, and merging
    // would keep the declaration's text as the label for a dispatch we cannot
    // identify — the mislabel this discriminator exists to prevent.
    let (state, sid) = state_with_session();
    state.upsert_delegation(Delegation::new(
        sid,
        None,
        "engineer",
        ModelTier::Sonnet,
        "fix the delegation tracker",
    ));
    let payload = serde_json::json!({
        "tool": "Agent",
        "tool_use_id": "toolu_1",
        "input": { "subagent_type": "engineer" }
    });
    observe(&state, sid, HookEvent::PreToolUse, &payload);
    assert_eq!(state.delegations_for(sid).len(), 2);
}

#[test]
fn dedup_merges_an_unlabelled_declaration() {
    // The other empty case is NOT symmetric: an unlabelled declaration supplies
    // no discriminator of its own, but merging adopts the dispatch's text, so
    // no wrong label can survive.
    let (state, sid) = state_with_session();
    let declared = Delegation::new(sid, None, "engineer", ModelTier::Opus, "");
    let declared_id = declared.id;
    state.upsert_delegation(declared);
    observe(
        &state,
        sid,
        HookEvent::PreToolUse,
        &pre("Agent", "engineer", "rebase the release branch", "toolu_1"),
    );
    let d = only(&state, sid);
    assert_eq!(d.id, declared_id);
    assert_eq!(d.task, "rebase the release branch");
}

#[test]
fn dedup_merges_a_summarised_task_description() {
    // The dispatch `description` is typically a short summary of the longer
    // `agent_delegate` task, so a prefix match in either direction still merges.
    let (state, sid) = state_with_session();
    let declared = Delegation::new(
        sid,
        None,
        "engineer",
        ModelTier::Opus,
        "Fix the delegation tracker fail-open bug",
    );
    let declared_id = declared.id;
    state.upsert_delegation(declared);
    observe(
        &state,
        sid,
        HookEvent::PreToolUse,
        &pre(
            "Agent",
            "engineer",
            "fix   the delegation TRACKER",
            "toolu_1",
        ),
    );
    let d = only(&state, sid);
    assert_eq!(d.id, declared_id);
    assert_eq!(d.status, DelegationStatus::Running);
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
