//! Tests for the #8942 lifecycle refusals. Hermetic: every manager runs on a
//! scratch directory (which is also its kill floor's root) and the recording
//! `FakeTmuxDriver`; nothing reaches tmux, the daemon or `~/.trusty-mpm`.

use std::sync::Arc;

use super::super::tests::{FakeTmuxDriver, seed_record};
use crate::session_manager::{
    ManagedError, ManagedSessionId, ManagedSessionState, PruneFilter, SessionKind, SessionManager,
};

/// A manager over a fresh scratch directory and a recording fake tmux.
async fn manager() -> (tempfile::TempDir, Arc<FakeTmuxDriver>, SessionManager) {
    let dir = crate::test_support::hermetic_temp_dir();
    let fake = FakeTmuxDriver::new();
    let mgr = SessionManager::new(dir.path(), fake.clone())
        .await
        .expect("manager");
    (dir, fake, mgr)
}

/// Seed a record in `state` with a live pane when it is running, of `kind`.
async fn seed(
    mgr: &SessionManager,
    dir: &tempfile::TempDir,
    state: ManagedSessionState,
    kind: SessionKind,
) -> ManagedSessionId {
    let id = ManagedSessionId::new();
    seed_record(mgr, dir, id, state, false).await;
    let mut record = mgr.get(&id).await.expect("seeded");
    record.kind = kind;
    mgr.store.write().await.upsert(record).await.expect("kind");
    id
}

/// Record an Architect launch sidecar for `session` under `dir`.
fn sidecar(dir: &std::path::Path, session: &str) {
    let sidecars = dir.join("architect-launch");
    std::fs::create_dir_all(&sidecars).expect("sidecar dir");
    let body = serde_json::json!({"pid": 111, "start_time": 1, "session": session});
    std::fs::write(sidecars.join("111.architect-session"), body.to_string()).expect("sidecar");
}

async fn state_of(mgr: &SessionManager, id: &ManagedSessionId) -> ManagedSessionState {
    mgr.get(id).await.expect("record").state
}

/// Critic HIGH: with a floor that cannot clear any name (a corrupt sidecar),
/// stop and decommission of an ORDINARY live session return the refusal and
/// leave the pane, the workspace and the record untouched.
#[tokio::test]
async fn an_undeterminable_floor_aborts_stop_and_decommission_of_an_ordinary_session() {
    let (dir, fake, mgr) = manager().await;
    let id = ManagedSessionId::new();
    let ws = seed_record(&mgr, &dir, id, ManagedSessionState::Active, false).await;
    let sidecars = dir.path().join("architect-launch");
    std::fs::create_dir_all(&sidecars).expect("sidecar dir");
    std::fs::write(sidecars.join("1.architect-session"), "not json").expect("corrupt");

    let err = mgr.stop(&id).await.expect_err("stop must abort");
    assert!(matches!(err, ManagedError::KillRefused(_)), "{err}");
    let err = mgr
        .decommission_with_root(&id, dir.path(), None)
        .await
        .expect_err("decommission must abort");
    assert!(matches!(err, ManagedError::KillRefused(_)), "{err}");

    assert_eq!(state_of(&mgr, &id).await, ManagedSessionState::Active);
    assert!(ws.is_dir(), "the workspace was removed");
    assert!(
        fake.kill_calls.lock().unwrap().is_empty(),
        "the pane was killed"
    );
    assert!(
        fake.interrupt_calls.lock().unwrap().is_empty()
            && fake.pane_interrupt_calls.lock().unwrap().is_empty(),
        "the pane was signalled"
    );
}

/// Ruling 1: an operator stop of the Architect is refused before any tmux
/// call, and the refusal says how to stop it.
#[tokio::test]
async fn stop_refuses_a_supervisor_record_and_never_signals_it() {
    let (dir, fake, mgr) = manager().await;
    let id = seed(
        &mgr,
        &dir,
        ManagedSessionState::Active,
        SessionKind::Supervisor,
    )
    .await;
    let err = mgr.stop(&id).await.expect_err("stop must be refused");
    assert!(
        matches!(&err, ManagedError::InvalidState(_, why) if why.contains("exit claude")),
        "{err}"
    );
    assert_eq!(state_of(&mgr, &id).await, ManagedSessionState::Active);
    assert!(fake.kill_calls.lock().unwrap().is_empty());
    assert!(fake.interrupt_calls.lock().unwrap().is_empty());
    assert!(fake.pane_interrupt_calls.lock().unwrap().is_empty());
}

/// The daemon's shutdown stops ordinary sessions and never the Architect's.
#[tokio::test]
async fn shutdown_skips_a_supervisor_session() {
    let (dir, fake, mgr) = manager().await;
    let arch = seed(
        &mgr,
        &dir,
        ManagedSessionState::Active,
        SessionKind::Supervisor,
    )
    .await;
    let work = seed(
        &mgr,
        &dir,
        ManagedSessionState::Active,
        SessionKind::Ordinary,
    )
    .await;
    // #9101: shutdown stops only a session that owns its pane.
    crate::session_manager::tests::bind_pane(&mgr, &arch).await;
    crate::session_manager::tests::bind_pane(&mgr, &work).await;
    mgr.shutdown().await;
    // #9101: the kill is by session id, so the signalled pane names the session.
    let name = |id: ManagedSessionId| format!("tmpm-seed-{id}");
    let signalled: Vec<String> = (fake.pane_interrupt_calls.lock().unwrap().iter())
        .map(|(session, _)| session.clone())
        .collect();
    assert_eq!(
        signalled,
        vec![name(work)],
        "shutdown reached {signalled:?}"
    );
    assert!(!signalled.contains(&name(arch)));
    let stopped: Vec<String> = fake.kill_calls.lock().unwrap().clone();
    assert_eq!(
        stopped,
        vec![crate::session_manager::tests::FAKE_SESSION_ID.to_string()],
        "one kill, by id: {stopped:?}"
    );
}

/// `--state all --include-active` never selects the Architect: a dry run does
/// not list it, and a real run leaves it `Active`.
#[tokio::test]
async fn prune_include_active_never_decommissions_a_supervisor_record() {
    let (dir, _fake, mgr) = manager().await;
    let id = seed(
        &mgr,
        &dir,
        ManagedSessionState::Active,
        SessionKind::Supervisor,
    )
    .await;
    for dry_run in [true, false] {
        let outcome = mgr
            .prune_managed(PruneFilter::All, dry_run, true, None, None)
            .await
            .expect("prune");
        assert!(
            outcome.sessions.iter().all(|s| s.id != id.to_string()),
            "dry_run={dry_run}: {:?}",
            outcome.sessions
        );
    }
    assert_eq!(state_of(&mgr, &id).await, ManagedSessionState::Active);
}

/// An operator resume of a stopped Architect record is refused and points at
/// `tm fleet init`; no pane is killed or created.
#[tokio::test]
async fn resume_refuses_a_supervisor_record() {
    let (dir, fake, mgr) = manager().await;
    let id = seed(
        &mgr,
        &dir,
        ManagedSessionState::Stopped,
        SessionKind::Supervisor,
    )
    .await;
    let err = mgr.resume(&id).await.expect_err("resume must be refused");
    assert!(
        matches!(&err, ManagedError::InvalidState(_, why) if why.contains("tm fleet init")),
        "{err}"
    );
    assert_eq!(state_of(&mgr, &id).await, ManagedSessionState::Stopped);
    assert!(fake.kill_calls.lock().unwrap().is_empty());
    assert!(fake.create_cwd_calls.lock().unwrap().is_empty());
}

/// Critic MEDIUM: `delete --force` of a live Architect record is refused, so
/// the record keeps protecting its name.
#[tokio::test]
async fn delete_refuses_a_live_supervisor_record() {
    let (dir, _fake, mgr) = manager().await;
    let id = seed(
        &mgr,
        &dir,
        ManagedSessionState::Active,
        SessionKind::Supervisor,
    )
    .await;
    let err = mgr
        .delete_record(&id, true)
        .await
        .expect_err("delete must be refused");
    assert!(matches!(err, ManagedError::InvalidState(..)), "{err}");
    assert_eq!(state_of(&mgr, &id).await, ManagedSessionState::Active);
}

/// Ruling 4: a task is never typed into the Architect's ready pane.
#[tokio::test]
async fn task_injection_never_types_into_a_supervisor_pane() {
    let (dir, fake, mgr) = manager().await;
    let id = seed(
        &mgr,
        &dir,
        ManagedSessionState::Active,
        SessionKind::Supervisor,
    )
    .await;
    let sent = mgr
        .inject_task_when_ready(&id, "do the thing")
        .await
        .expect("inject");
    assert!(!sent);
    assert!(fake.send_calls.lock().unwrap().is_empty());
    assert!(fake.pane_send_calls.lock().unwrap().is_empty());
}

/// A live pane a sidecar names is adopted at boot as the Architect's record,
/// under its stable id, never as an ordinary adopted session.
#[tokio::test]
async fn boot_reconcile_adopts_a_sidecar_named_pane_as_supervisor() {
    let (dir, fake, mgr) = manager().await;
    sidecar(dir.path(), "tm-arch");
    let arch_dir = dir.path().join("architect");
    std::fs::create_dir_all(&arch_dir).expect("architect dir");
    fake.seeded_names.lock().unwrap().push("tm-arch".into());
    *fake.pane_cwd_override.lock().unwrap() = Some(arch_dir.clone());

    let report = mgr.reconcile_on_boot(false).await.expect("reconcile");
    assert!(
        report.external_adopted.contains(&"tm-arch".to_string()),
        "{report:?}"
    );
    let record = mgr
        .list()
        .await
        .into_iter()
        .find(|r| r.tmux_name == "tm-arch")
        .expect("adopted");
    assert_eq!(record.kind, SessionKind::Supervisor);
    assert_eq!(
        record.id,
        ManagedSessionId::for_supervisor(&arch_dir, "supervisor")
    );
    assert!(!record.is_auto_resumable());
}

/// One Architect dir and role always give the same id; a different role or
/// dir gives a different one.
#[test]
fn the_supervisor_id_is_stable_per_dir_and_role() {
    let dir = crate::test_support::hermetic_temp_dir();
    let other = crate::test_support::hermetic_temp_dir();
    let id = ManagedSessionId::for_supervisor(dir.path(), "supervisor");
    assert_eq!(
        id,
        ManagedSessionId::for_supervisor(dir.path(), "supervisor")
    );
    assert_ne!(id, ManagedSessionId::for_supervisor(dir.path(), "-poll"));
    assert_ne!(
        id,
        ManagedSessionId::for_supervisor(other.path(), "supervisor")
    );
}
