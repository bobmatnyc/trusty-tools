//! Tests for the legacy-registry teardown paths, the shutdown pass and the
//! legacy DELETE (#8942, #9101, #9017). Hermetic: tmux resolves to a scripted
//! fake that logs its argv.

use super::skip_legacy_names;

/// #9101: a legacy registry entry is a bare tmux name, so the shutdown pass
/// kills none of them — not the Architect's names (#8942), and not an
/// ordinary one, which a restarted tmux server may have handed to an
/// unrelated session. No tmux call runs at all.
#[serial_test::serial]
#[tokio::test]
async fn the_shutdown_legacy_pass_kills_no_legacy_session() {
    let dir = crate::test_support::hermetic_temp_dir();
    let log = dir.path().join("calls.log");
    let bin = dir.path().join("fake-tmux");
    let script = format!("#!/bin/sh\necho \"$*\" >> '{}'\nexit 0\n", log.display());
    // See #10248
    trusty_common::exec_stub::write_executable(&bin, script).expect("write fake tmux");

    let names = ["tm-arch", "tm-arch-poll", "tm-work"].map(String::from);
    let skipped =
        crate::core::tmux::with_tmux_binary(bin, async { skip_legacy_names(names) }).await;

    let calls = std::fs::read_to_string(&log).unwrap_or_default();
    assert_eq!(skipped, 3, "every legacy name is reported as left running");
    assert!(calls.is_empty(), "the legacy pass ran tmux: {calls}");
}

/// #9017 row 2d: the legacy `DELETE /sessions/{id}` of a registry entry that
/// carries the Architect's tmux name removes the entry and leaves the
/// Architect's managed record live: its store reconcile never decommissions,
/// signals or kills the Architect's session. The best-effort tmux kill goes
/// to a logging fake; its host floor is proven in `architect_floor_host.rs`.
#[serial_test::serial]
#[tokio::test]
async fn the_legacy_delete_of_an_architect_named_entry_leaves_the_architect_alone() {
    use std::sync::Arc;

    use crate::core::session::{ControlModel, Session, SessionId};
    use crate::session_manager::tests::FakeTmuxDriver;
    use crate::session_manager::{ManagedSessionId, ManagedSessionState, SessionKind};

    let dir = crate::test_support::hermetic_temp_dir();
    let bin = dir.path().join("fake-tmux");
    let script = format!(
        "#!/bin/sh\necho \"$*\" >> '{}'\nexit 0\n",
        dir.path().join("calls.log").display()
    );
    // See #10248
    trusty_common::exec_stub::write_executable(&bin, script).expect("write fake tmux");

    let fake = FakeTmuxDriver::new();
    let mgr = crate::session_manager::SessionManager::new(&dir.path().join("sm"), fake.clone())
        .await
        .expect("manager");
    let id = ManagedSessionId::new();
    let mut record: crate::session_manager::SessionRecord =
        serde_json::from_value(serde_json::json!({
            "id": id.to_string(), "task": "t", "tmux_name": "tm-arch",
            "cwd": dir.path(), "state": "active", "created_at": "2026-10-16T00:00:00Z",
        }))
        .expect("record");
    record.kind = SessionKind::Architect;
    // The live Architect owns its pane on its server, so a teardown that got
    // past the guards would reach it.
    record.pane_id = Some("%1".into());
    record.tmux_server = Some(crate::session_manager::tests::FAKE_TMUX_SERVER.into());
    mgr.store.write().await.upsert(record).await.expect("seed");
    use crate::session_manager::ManagedTmuxDriver as _;
    fake.create_session("tm-arch", "/tmp").expect("live pane");
    let mgr = Arc::new(mgr);
    let state = Arc::new(crate::daemon::state::DaemonState::with_session_manager(
        Arc::clone(&mgr),
    ));
    let legacy = SessionId::new();
    let mut session = Session::new(legacy, "/tmp/p", ControlModel::Tmux, None);
    session.tmux_name = "tm-arch".into();
    state.register_session(session);

    let removed = crate::core::tmux::with_tmux_binary(
        bin,
        crate::daemon::rpc::sessions_legacy_ops::remove_session(&state, "tm-arch"),
    )
    .await
    .expect("the legacy entry is removed");

    assert_eq!(removed.removed, "tm-arch");
    assert!(state.session(legacy).is_none(), "the legacy entry stayed");
    let after = mgr.get(&id).await.expect("architect record");
    assert_eq!(after.state, ManagedSessionState::Active);
    assert_eq!(after.kind, SessionKind::Architect);
    assert!(fake.kill_calls.lock().unwrap().is_empty());
    assert!(fake.interrupt_calls.lock().unwrap().is_empty());
    assert!(fake.pane_interrupt_calls.lock().unwrap().is_empty());
}
