//! Tests that a stopped builder's slot index is not reissued while the agent
//! may resume (#8548 critic round).
//!
//! Why: a stop frees the slot's capacity at once, but a resumed agent goes on
//! building in the directory it was given. Each test drives only the surfaces
//! that predate the fix — the sweep, the hook tracker, the claim — so it
//! compiles and fails against the tree before it.
//! Test: this file IS the test module.

use std::path::{Path, PathBuf};

use crate::core::agent::{Delegation, DelegationStatus, ModelTier};
use crate::core::hook::HookEvent;
use crate::core::session::SessionId;
use crate::daemon::services::delegation_tracker::observe;

use super::builder_slots::BUILDER_LEASE_TTL_SECS;
use super::core::DaemonState;

const AGENT_ID: &str = "a77c0ffeebuilder";
const TOOL_USE_ID: &str = "toolu_01Qstopped";

/// A running builder on slot 0, dispatched from `<root>/proj/sess.jsonl`.
fn builder_on_slot_zero(state: &DaemonState, root: &Path) -> Delegation {
    let mut d = Delegation::new(
        SessionId::new(),
        None,
        "rust-engineer",
        ModelTier::Sonnet,
        "build it",
    );
    d.status = DelegationStatus::Running;
    d.started_at = Some(chrono::Utc::now() - chrono::Duration::minutes(29));
    d.agent_id = Some(AGENT_ID.to_string());
    d.tool_use_id = Some(TOOL_USE_ID.to_string());
    d.transcript_path = Some(root.join("proj").join("sess.jsonl"));
    d.cwd = Some(root.join("repo"));
    d.builder_slot = Some(0);
    state.upsert_delegation(d.clone());
    d
}

/// Write the agent's sidecar with `stoppedByUser` set to `stopped`.
fn write_sidecar(root: &Path, stopped: bool) -> PathBuf {
    let dir = root.join("proj").join("sess").join("subagents");
    std::fs::create_dir_all(&dir).expect("sidecar dir");
    let path = dir.join(format!("agent-{AGENT_ID}.meta.json"));
    let body = format!(r#"{{"toolUseId":"{TOOL_USE_ID}","stoppedByUser":{stopped}}}"#);
    std::fs::write(&path, body).expect("sidecar");
    path
}

fn record(state: &DaemonState, d: &Delegation) -> Delegation {
    state
        .delegations_for(d.session)
        .into_iter()
        .find(|r| r.id == d.id)
        .expect("record kept")
}

/// Claim a builder slot for a new dispatch; returns (admitted, slot index).
fn claim(state: &DaemonState, cap: u32, tool_use_id: &str) -> (bool, Option<u32>) {
    let session = SessionId::new();
    let mut d = Delegation::new(session, None, "python-engineer", ModelTier::Sonnet, "b");
    d.status = DelegationStatus::Running;
    d.started_at = Some(chrono::Utc::now());
    d.tool_use_id = Some(tool_use_id.to_string());
    let (_, claimed) = state.claim_builder_slot(
        cap,
        Some(tool_use_id),
        true,
        |s| s.upsert_delegation(d.clone()),
        |_| {},
    );
    (claimed, record(state, &d).builder_slot)
}

/// The BLOCK: the user stops a builder, resumes it, and before the daemon sees
/// the resume a new builder claims. It must get the freed capacity but NOT the
/// stopped builder's directory.
#[test]
fn a_resumed_user_stopped_builder_keeps_its_slot_index_8548() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = DaemonState::new();
    let holder = builder_on_slot_zero(&state, tmp.path());
    write_sidecar(tmp.path(), true);
    state.sweep_delegations();
    assert_eq!(record(&state, &holder).status, DelegationStatus::Cancelled);

    write_sidecar(tmp.path(), false);
    let (claimed, index) = claim(&state, 1, "toolu_NEXT");

    assert!(claimed, "the stop frees the capacity at once");
    assert_eq!(
        index,
        Some(1),
        "slot 0 belongs to an agent that may resume into it"
    );
    state.sweep_delegations();
    assert_eq!(record(&state, &holder).status, DelegationStatus::Running);
    assert_eq!(record(&state, &holder).builder_slot, Some(0));
}

/// The same hole through the PM's `TaskStop`, re-armed by the resumed agent's
/// own tool call.
#[test]
fn a_resumed_task_stopped_builder_keeps_its_slot_index_8548() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = DaemonState::new();
    let holder = builder_on_slot_zero(&state, tmp.path());
    let stop = serde_json::json!({"tool": "TaskStop", "input": {"task_id": AGENT_ID}});
    observe(&state, holder.session, HookEvent::PostToolUse, &stop);
    assert_eq!(record(&state, &holder).status, DelegationStatus::Cancelled);

    let (claimed, index) = claim(&state, 1, "toolu_NEXT");
    assert!(claimed, "the stop frees the capacity at once");
    assert_eq!(index, Some(1), "slot 0 belongs to a stopped agent");

    let activity = serde_json::json!({
        "agent_id": AGENT_ID,
        "cwd": tmp.path().join("repo"),
        "tool": "Bash",
        "tool_use_id": "toolu_sub",
    });
    // A tool call inside the grace window predates the stop: no re-arm.
    observe(&state, holder.session, HookEvent::PreToolUse, &activity);
    assert_eq!(record(&state, &holder).status, DelegationStatus::Cancelled);
    state.mutate_delegation(holder.id, |d| {
        d.ended_at = Some(chrono::Utc::now() - chrono::Duration::seconds(60));
    });
    observe(&state, holder.session, HookEvent::PreToolUse, &activity);
    assert_eq!(
        record(&state, &holder).status,
        DelegationStatus::Running,
        "the resumed agent's tool call re-arms its lease"
    );
}

/// The sidecar flipping to `stoppedByUser: false` re-arms the released lease.
#[test]
fn a_resume_marker_rearms_the_released_lease_8548() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = DaemonState::new();
    let holder = builder_on_slot_zero(&state, tmp.path());
    write_sidecar(tmp.path(), true);
    state.sweep_delegations();
    assert!(state.builder_slot_holders(None).is_empty(), "precondition");

    write_sidecar(tmp.path(), false);
    state.sweep_delegations();

    let now = record(&state, &holder);
    assert_eq!(now.status, DelegationStatus::Running);
    assert_eq!(now.ended_at, None);
    assert_eq!(state.builder_slot_holders(None).len(), 1);
}

/// The quarantine is bounded by the lease TTL: past it, the index is free.
#[test]
fn a_quarantined_index_is_reusable_after_the_ttl_8548() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = DaemonState::new();
    let holder = builder_on_slot_zero(&state, tmp.path());
    write_sidecar(tmp.path(), true);
    state.sweep_delegations();

    let (_, inside) = claim(&state, 1, "toolu_B");
    assert_eq!(inside, Some(1), "inside the TTL slot 0 is quarantined");
    let b = state
        .all_delegations()
        .into_iter()
        .find(|d| d.tool_use_id.as_deref() == Some("toolu_B"))
        .expect("B recorded");
    state.terminate_delegation(b.id, DelegationStatus::Completed);
    state.mutate_delegation(holder.id, |d| {
        d.started_at =
            Some(chrono::Utc::now() - chrono::Duration::seconds(BUILDER_LEASE_TTL_SECS + 60));
    });

    let (claimed, after) = claim(&state, 1, "toolu_C");
    assert!(claimed);
    assert_eq!(after, Some(0), "past the TTL the index is reusable");
}
