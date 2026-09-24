//! Tests for the #8257 delegation record view and listing.
//! Test: this file IS the test module.

use super::*;
use crate::core::agent::{Delegation, DelegationStatus};
use crate::core::session::SessionId;

fn record(agent_id: Option<&str>, cwd: &str, status: DelegationStatus) -> Delegation {
    let mut d = Delegation::observed(SessionId::new(), "version-control", "task", None);
    d.agent_id = agent_id.map(str::to_string);
    d.cwd = Some(PathBuf::from(cwd));
    d.status = status;
    d
}

#[test]
fn record_view_carries_the_repair_command_for_each_id_shape_8257() {
    let now = chrono::Utc::now();
    let with_id = record(Some("a1b2c3"), "/repo", DelegationStatus::Running);
    assert_eq!(
        DelegationRecordView::of(&with_id, now, true).repair_command,
        "tm repair delegation a1b2c3"
    );
    let without = record(None, "/repo", DelegationStatus::Running);
    let view = DelegationRecordView::of(&without, now, true);
    assert_eq!(
        view.repair_command,
        format!("tm repair delegation --delegation-id {}", without.id.0)
    );
    assert_eq!(view.session, without.session.0.to_string());
    assert!(view.age_secs >= 0);
}

#[test]
fn listing_names_every_open_record_in_a_directory_8257() {
    let state = DaemonState::new();
    let blocking = record(None, "/repo", DelegationStatus::Running);
    let mut isolated = record(Some("iso"), "/repo", DelegationStatus::Running);
    isolated.isolation = Some("worktree".to_string());
    state.upsert_delegation(blocking.clone());
    state.upsert_delegation(isolated.clone());
    state.upsert_delegation(record(Some("done"), "/repo", DelegationStatus::Completed));
    state.upsert_delegation(record(
        Some("other"),
        "/elsewhere",
        DelegationStatus::Running,
    ));

    let listed = list_for_dir(&state, Path::new("/repo"));

    assert_eq!(
        listed.len(),
        2,
        "terminal and other-directory records are omitted"
    );
    let find = |id: &Delegation| {
        listed
            .iter()
            .find(|v| v.delegation_id == id.id.0.to_string())
            .expect("listed")
            .blocks_dispatch
    };
    assert!(
        find(&blocking),
        "an unisolated live record blocks a dispatch"
    );
    assert!(!find(&isolated), "an isolated one does not");
}
