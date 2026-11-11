//! Tests for [`SessionManager::decommission_project`] (#10988). Hermetic: a
//! scratch manager over the recording `FakeTmuxDriver` and a scratch registry.

use std::path::PathBuf;
use std::sync::Arc;

use super::super::tests::{FakeTmuxDriver, seed_record};
use super::ProjectDecommissionError;
use crate::project::{Project, ProjectRegistry, ProjectStoreError};
use crate::session_manager::{ManagedSessionId, ManagedSessionState, SessionKind, SessionManager};

const REPO: &str = "https://github.com/acme/widget";

/// A manager and a registry holding project `widget`, each on its own dir.
async fn fixture() -> (
    tempfile::TempDir,
    tempfile::TempDir,
    Arc<FakeTmuxDriver>,
    SessionManager,
    ProjectRegistry,
) {
    let dir = crate::test_support::hermetic_temp_dir();
    let reg_dir = crate::test_support::hermetic_temp_dir();
    let fake = FakeTmuxDriver::new();
    let mgr = SessionManager::new(dir.path(), fake.clone())
        .await
        .expect("manager");
    let registry = ProjectRegistry::load(reg_dir.path())
        .await
        .expect("registry");
    registry
        .register(Project {
            name: "widget".into(),
            repo_url: REPO.into(),
            default_branch: "main".into(),
            stack_hint: None,
            tags: vec![],
            description: None,
            gh_user: None,
            gh_account: None,
            github: None,
            commit_name: None,
            commit_email: None,
            worktree: None,
            project_key: Some("acme/widget".into()),
            decommissioned_at: None,
        })
        .await
        .expect("register");
    (dir, reg_dir, fake, mgr, registry)
}

/// Seed a record bound to `widget` in `state`, of `kind`; return id and workspace.
async fn seed_bound(
    mgr: &SessionManager,
    dir: &tempfile::TempDir,
    state: ManagedSessionState,
    kind: SessionKind,
) -> (ManagedSessionId, PathBuf) {
    let id = ManagedSessionId::new();
    let ws = seed_record(mgr, dir, id, state, false).await;
    let mut record = mgr.get(&id).await.expect("seeded");
    record.repo_url = Some(REPO.into());
    record.kind = kind;
    mgr.store.write().await.upsert(record).await.expect("bind");
    (id, ws)
}

async fn state_of(mgr: &SessionManager, id: &ManagedSessionId) -> ManagedSessionState {
    mgr.get(id).await.expect("record").state
}

/// Criteria 1–2: live sessions stop, a stopped one is left alone, every
/// workspace stays, and the row is stamped and visible in its JSON.
#[tokio::test]
async fn decommission_project_stops_live_sessions_and_keeps_the_workspace() {
    let (dir, _reg, _fake, mgr, registry) = fixture().await;
    let (live, live_ws) = seed_bound(
        &mgr,
        &dir,
        ManagedSessionState::Active,
        SessionKind::default(),
    )
    .await;
    let (idle, idle_ws) = seed_bound(
        &mgr,
        &dir,
        ManagedSessionState::Stopped,
        SessionKind::default(),
    )
    .await;

    let report = mgr
        .decommission_project(&registry, "widget")
        .await
        .expect("decommission");

    assert_eq!(report.stopped, vec![live.to_string()]);
    assert_eq!(report.left_as_is, vec![idle.to_string()]);
    assert_eq!(state_of(&mgr, &live).await, ManagedSessionState::Stopped);
    assert!(live_ws.is_dir(), "the live session's workspace was removed");
    assert!(
        idle_ws.is_dir(),
        "the stopped session's workspace was removed"
    );
    let stored = registry.get("widget").await.expect("row kept");
    assert!(stored.is_decommissioned(), "the row was not stamped");
    let json = serde_json::to_value(&stored).expect("serialize");
    assert!(
        json.get("decommissioned_at").is_some_and(|v| !v.is_null()),
        "project_list/project_get must show the state: {json}"
    );
}

/// Criterion 3: an Architect session refuses the verb before any stop.
#[tokio::test]
async fn decommission_project_refuses_an_architect_session() {
    let (dir, _reg, fake, mgr, registry) = fixture().await;
    let (arch, _) = seed_bound(
        &mgr,
        &dir,
        ManagedSessionState::Active,
        SessionKind::Architect,
    )
    .await;
    let (work, _) = seed_bound(
        &mgr,
        &dir,
        ManagedSessionState::Active,
        SessionKind::default(),
    )
    .await;

    let err = mgr
        .decommission_project(&registry, "widget")
        .await
        .expect_err("must refuse");

    assert!(
        matches!(&err, ProjectDecommissionError::Protected(why) if why.contains("decommission")),
        "{err}"
    );
    assert_eq!(state_of(&mgr, &arch).await, ManagedSessionState::Active);
    assert_eq!(state_of(&mgr, &work).await, ManagedSessionState::Active);
    assert!(
        fake.kill_calls.lock().unwrap().is_empty(),
        "a pane was killed"
    );
    assert!(
        !registry
            .get("widget")
            .await
            .expect("row")
            .is_decommissioned()
    );
}

/// Criterion 4: a stop that fails leaves the project active and names the
/// failure; the error is never swallowed into a success.
#[tokio::test]
async fn decommission_project_failed_stop_leaves_the_project_active() {
    let (dir, _reg, _fake, mgr, registry) = fixture().await;
    let (id, ws) = seed_bound(
        &mgr,
        &dir,
        ManagedSessionState::Active,
        SessionKind::default(),
    )
    .await;
    // A corrupt Architect sidecar makes the kill floor undeterminable, so the
    // stop aborts with `KillRefused` (see `architect_tests`).
    let sidecars = dir.path().join("architect-launch");
    std::fs::create_dir_all(&sidecars).expect("sidecar dir");
    std::fs::write(sidecars.join("1.architect-session"), "not json").expect("corrupt");

    let err = mgr
        .decommission_project(&registry, "widget")
        .await
        .expect_err("a failed stop must fail the verb");

    match &err {
        ProjectDecommissionError::PartialStop {
            stopped, failed, ..
        } => {
            assert!(stopped.is_empty(), "{err}");
            assert_eq!(failed.len(), 1, "{err}");
            assert_eq!(failed[0].0, id.to_string());
        }
        other => panic!("expected PartialStop, got {other}"),
    }
    assert!(
        !registry
            .get("widget")
            .await
            .expect("row")
            .is_decommissioned(),
        "a failed stop marked the project decommissioned"
    );
    assert_eq!(state_of(&mgr, &id).await, ManagedSessionState::Active);
    assert!(ws.is_dir());
}

/// #10988: ruling D8e — project decommission keeps each session record, and the
/// kept record still resumes.
///
/// Why: "kept" alone would pass for a record that survives as an unresumable
/// tombstone; the ruling says records stay resumable.
/// Test: this function IS the test.
#[tokio::test]
async fn decommission_project_keeps_records_resumable() {
    let (dir, _reg, _fake, mgr, registry) = fixture().await;
    let (live, _ws) = seed_bound(
        &mgr,
        &dir,
        ManagedSessionState::Active,
        SessionKind::default(),
    )
    .await;

    mgr.decommission_project(&registry, "widget")
        .await
        .expect("decommission");

    assert_eq!(
        state_of(&mgr, &live).await,
        ManagedSessionState::Stopped,
        "the record must survive project decommission as Stopped, not as a tombstone"
    );
    let resumed = mgr
        .resume(&live)
        .await
        .expect("a session stopped by project decommission must stay resumable");
    assert_eq!(resumed.state, ManagedSessionState::Active);
}

/// An unknown project is the registry's `NotFound`, with nothing stopped.
#[tokio::test]
async fn decommission_project_unknown_name_errors() {
    let (_dir, _reg, _fake, mgr, registry) = fixture().await;
    let err = mgr
        .decommission_project(&registry, "nope")
        .await
        .expect_err("unknown");
    assert!(
        matches!(
            err,
            ProjectDecommissionError::Registry(ProjectStoreError::NotFound(_))
        ),
        "{err}"
    );
}
