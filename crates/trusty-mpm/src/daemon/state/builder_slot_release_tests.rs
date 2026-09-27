//! Tests for releasing a user-stopped builder's slot (#8548).
//!
//! Why: every test drives the delegation sweep and reads the holder list — the
//! two surfaces the reap loop and `tm doctor` use — rather than the release
//! function itself, so the regression compiles and fails against the tree
//! before the fix.
//! Test: this file IS the test module.

use std::path::{Path, PathBuf};

use crate::core::agent::{Delegation, DelegationStatus, ModelTier};
use crate::core::session::SessionId;

use super::core::DaemonState;

const AGENT_ID: &str = "a403cdbc078b5c474";
const TOOL_USE_ID: &str = "toolu_01DAbuilder";

/// A running builder lease whose dispatcher wrote `<root>/proj/sess.jsonl`.
fn running_builder(state: &DaemonState, root: &Path) -> Delegation {
    let mut d = Delegation::new(
        SessionId::new(),
        None,
        "typescript-engineer",
        ModelTier::Sonnet,
        "build it",
    );
    d.status = DelegationStatus::Running;
    d.started_at = Some(chrono::Utc::now() - chrono::Duration::minutes(29));
    d.agent_id = Some(AGENT_ID.to_string());
    d.tool_use_id = Some(TOOL_USE_ID.to_string());
    d.transcript_path = Some(root.join("proj").join("sess.jsonl"));
    state.upsert_delegation(d.clone());
    d
}

/// Write the sidecar Claude Code keeps beside the agent's transcript.
fn write_sidecar(root: &Path, body: &str) -> PathBuf {
    let dir = root.join("proj").join("sess").join("subagents");
    std::fs::create_dir_all(&dir).expect("sidecar dir");
    let path = dir.join(format!("agent-{AGENT_ID}.meta.json"));
    std::fs::write(&path, body).expect("sidecar");
    path
}

fn status_of(state: &DaemonState, d: &Delegation) -> DelegationStatus {
    state
        .delegations_for(d.session)
        .into_iter()
        .find(|r| r.id == d.id)
        .expect("record kept")
        .status
}

/// The leak: the user stopped the agent, no hook arrived, and the slot stayed
/// held until the TTL. The sweep must release it.
#[test]
fn a_user_stopped_builder_releases_its_slot_8548() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = DaemonState::new();
    let d = running_builder(&state, tmp.path());
    write_sidecar(
        tmp.path(),
        &format!(
            r#"{{"agentType":"typescript-engineer","toolUseId":"{TOOL_USE_ID}","stoppedByUser":true}}"#
        ),
    );
    assert_eq!(state.builder_slot_holders(None).len(), 1, "precondition");

    state.sweep_delegations();

    assert!(
        state.builder_slot_holders(None).is_empty(),
        "a user-stopped builder must stop holding its slot before the TTL"
    );
    assert_eq!(status_of(&state, &d), DelegationStatus::Cancelled);
}

/// A live builder's sidecar carries no `true` marker — at spawn, or after the
/// user resumed it (`false`). Neither may release the slot.
#[test]
fn a_live_builder_keeps_its_slot_8548() {
    for body in [
        format!(r#"{{"agentType":"typescript-engineer","toolUseId":"{TOOL_USE_ID}"}}"#),
        format!(r#"{{"toolUseId":"{TOOL_USE_ID}","stoppedByUser":false}}"#),
    ] {
        let tmp = tempfile::tempdir().expect("tempdir");
        let state = DaemonState::new();
        let d = running_builder(&state, tmp.path());
        write_sidecar(tmp.path(), &body);

        state.sweep_delegations();

        assert_eq!(state.builder_slot_holders(None).len(), 1, "sidecar {body}");
        assert_eq!(status_of(&state, &d), DelegationStatus::Running);
    }
    // No sidecar at all is the same answer.
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = DaemonState::new();
    running_builder(&state, tmp.path());
    state.sweep_delegations();
    assert_eq!(state.builder_slot_holders(None).len(), 1, "no sidecar");
}

/// The error arm: a sidecar that is not JSON is logged and the lease is kept.
#[test]
fn an_unreadable_stop_marker_keeps_the_lease_8548() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = DaemonState::new();
    let d = running_builder(&state, tmp.path());
    write_sidecar(tmp.path(), "{\"stoppedByUser\":tr");

    state.sweep_delegations();

    assert_eq!(state.builder_slot_holders(None).len(), 1);
    assert_eq!(status_of(&state, &d), DelegationStatus::Running);
}

/// Identity: a marker whose `toolUseId` names another dispatch is not
/// evidence about this lease.
#[test]
fn a_stop_marker_for_another_dispatch_keeps_the_lease_8548() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = DaemonState::new();
    let d = running_builder(&state, tmp.path());
    write_sidecar(
        tmp.path(),
        r#"{"toolUseId":"toolu_someone_else","stoppedByUser":true}"#,
    );

    state.sweep_delegations();

    assert_eq!(state.builder_slot_holders(None).len(), 1);
    assert_eq!(status_of(&state, &d), DelegationStatus::Running);
}

/// A transcript path carrying `..` derives no sidecar, even when a stopped
/// marker sits where the traversal would land.
#[test]
fn a_traversing_transcript_path_reads_no_sidecar_8548() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = DaemonState::new();
    let mut d = running_builder(&state, tmp.path());
    d.transcript_path = Some(
        tmp.path()
            .join("other")
            .join("..")
            .join("proj")
            .join("sess.jsonl"),
    );
    state.upsert_delegation(d.clone());
    write_sidecar(
        tmp.path(),
        &format!(r#"{{"toolUseId":"{TOOL_USE_ID}","stoppedByUser":true}}"#),
    );

    state.sweep_delegations();

    assert_eq!(state.builder_slot_holders(None).len(), 1);
    assert_eq!(status_of(&state, &d), DelegationStatus::Running);
}
