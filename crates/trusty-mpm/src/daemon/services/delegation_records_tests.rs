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
    let state = DaemonState::new();
    let now = chrono::Utc::now();
    let with_id = record(Some("a1b2c3"), "/repo", DelegationStatus::Running);
    assert_eq!(
        DelegationRecordView::of(&state, &with_id, now, true).repair_command,
        "tm repair delegation a1b2c3"
    );
    let without = record(None, "/repo", DelegationStatus::Running);
    let view = DelegationRecordView::of(&state, &without, now, true);
    assert_eq!(
        view.repair_command,
        format!("tm repair delegation --delegation-id {}", without.id.0)
    );
    assert!(view.age_secs >= 0);
}

/// Assert `text` carries neither form of `session`'s UUID.
pub(crate) fn assert_no_uuid(text: &str, session: SessionId) {
    for form in [
        session.0.hyphenated().to_string(),
        session.0.simple().to_string(),
    ] {
        assert!(!text.contains(&form), "owner UUID {form} leaked: {text}");
    }
}

// #8257 owner ruling: the view is what the deny JSON and the unauthenticated
// listing carry, so its wire form must not hold the owner's UUID — and the
// label it holds instead must not be a value the caller-session header takes.
#[test]
fn owner_label_never_carries_the_owner_uuid_8257() {
    use crate::core::session::{ControlModel, Session};
    use crate::daemon::services::delegation_repair::RepairCaller;

    let state = DaemonState::new();
    let named = SessionId::new();
    state.register_session(Session::new(
        named,
        "/repo",
        ControlModel::Tmux,
        Some(Path::new("/repo")),
    ));
    // A tmux name carrying the session's own UUID, and one that IS a UUID.
    let self_named = SessionId::new();
    let mut s = Session::new(self_named, "/x", ControlModel::Tmux, None);
    s.tmux_name = format!("tm-{}", self_named.0.hyphenated());
    state.register_session(s);
    let bare_uuid = SessionId::new();
    let mut s = Session::new(bare_uuid, "/x", ControlModel::Tmux, None);
    s.tmux_name = bare_uuid.0.simple().to_string();
    state.register_session(s);
    let unknown = SessionId::new();

    let now = chrono::Utc::now();
    for (session, want) in [
        (named, "session `tm-repo`"),
        (self_named, "a session with no caller-safe name"),
        (bare_uuid, "a session with no caller-safe name"),
        (unknown, "a session the daemon holds no record of"),
    ] {
        let mut d = record(None, "/repo", DelegationStatus::Running);
        d.session = session;
        let view = DelegationRecordView::of(&state, &d, now, true);
        assert_eq!(view.owner, want);
        assert_no_uuid(&serde_json::to_string(&view).expect("json"), session);
        assert!(
            matches!(
                RepairCaller::from_request(Some(&view.owner)),
                RepairCaller::Unestablished(_)
            ),
            "the label must not establish a caller: {}",
            view.owner
        );
    }
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
