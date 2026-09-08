//! Unit tests for `mpm.residency.active` (#7087 slice 1b).
//!
//! Why a sibling file rather than an inline `#[cfg(test)] mod`: keeps
//! `residency.rs` itself small, matching the `tests.rs`/`staleness_bench_tests.rs`
//! precedent already established in this directory.
//! What: exercises [`active_projects_core`] directly against a
//! `DaemonState::with_root_isolated_managed_and_driver` fixture — no HTTP, no
//! real tmux, no real daemon process.
//! Test: this file.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use trusty_common::residency::ActiveProjectSet;

use super::active_projects_core;
use crate::daemon::managed_routes::tests::make_record;
use crate::daemon::rpc::managed::outcome::{RouteBody, RouteOutcome};
use crate::daemon::state::DaemonState;
use crate::session_manager::{ManagedError, ManagedSessionState, ManagedTmuxDriver, StopCause};

/// A tmux driver whose `list_sessions` answer is set by the test — `Ok(names)`
/// by default, switchable to a failure mid-test (test (d) needs sessions
/// created while the driver still answers `Ok`, then a failure for the actual
/// route call).
struct ControllableTmux {
    live: Mutex<Result<Vec<String>, String>>,
}

impl ControllableTmux {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            live: Mutex::new(Ok(Vec::new())),
        })
    }

    fn set_live(&self, names: &[&str]) {
        *self.live.lock().unwrap() = Ok(names.iter().map(|s| s.to_string()).collect());
    }

    fn set_failing(&self, message: &str) {
        *self.live.lock().unwrap() = Err(message.to_string());
    }
}

impl ManagedTmuxDriver for ControllableTmux {
    fn create_session(&self, _name: &str, _workdir: &str) -> Result<(), ManagedError> {
        Ok(())
    }
    fn kill_session(&self, _name: &str) -> Result<(), ManagedError> {
        Ok(())
    }
    fn send_line(&self, _name: &str, _text: &str) -> Result<(), ManagedError> {
        Ok(())
    }
    fn capture(&self, _name: &str, _lines: usize) -> Result<String, ManagedError> {
        Ok(String::new())
    }
    fn list_sessions(&self) -> Result<Vec<String>, ManagedError> {
        match &*self.live.lock().unwrap() {
            Ok(names) => Ok(names.clone()),
            Err(e) => Err(ManagedError::TmuxUnavailable(e.clone())),
        }
    }
}

/// Build a `DaemonState` over a fresh temp dir with a caller-controlled tmux
/// driver, returning the state alongside the driver handle (for `set_live`/
/// `set_failing`) and the temp dir (kept alive for the test's lifetime).
async fn test_state() -> (Arc<DaemonState>, Arc<ControllableTmux>, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("temp dir");
    let driver = ControllableTmux::new();
    let state = DaemonState::with_root_isolated_managed_and_driver(
        dir.path().to_path_buf(),
        driver.clone(),
    )
    .await;
    (Arc::new(state), driver, dir)
}

/// Decode a `RouteOutcome`'s JSON body into `ActiveProjectSet`, panicking on
/// anything else (every test here expects a 200).
fn decode(outcome: &RouteOutcome) -> ActiveProjectSet {
    assert_eq!(
        outcome.status, 200,
        "expected 200, body: {:?}",
        outcome.body
    );
    match &outcome.body {
        RouteBody::Json(v) => serde_json::from_value(v.clone()).expect("decode ActiveProjectSet"),
        RouteBody::Text(t) => panic!("expected JSON body, got text: {t}"),
    }
}

/// (a) 2 Active-with-tmux + 1 Active-without-tmux + 1 Stopped → exactly 2
/// projects — the persisted-state filter AND the tmux-liveness filter both
/// gate inclusion, and a filtered-out record must not silently sneak in.
#[tokio::test]
async fn active_with_tmux_counted_others_excluded() {
    let (state, driver, _dir) = test_state().await;
    driver.set_live(&["tm-proj-a", "tm-proj-b"]);
    let mgr = state.session_manager().await;

    let mut active_a = make_record(None);
    active_a.tmux_name = "tm-proj-a".into();
    active_a.state = ManagedSessionState::Active;
    active_a.workspace_path = Some(PathBuf::from("/repo/a"));
    mgr.store
        .write()
        .await
        .upsert(active_a)
        .await
        .expect("upsert a");

    let mut active_b = make_record(None);
    active_b.tmux_name = "tm-proj-b".into();
    active_b.state = ManagedSessionState::Active;
    active_b.workspace_path = Some(PathBuf::from("/repo/b"));
    mgr.store
        .write()
        .await
        .upsert(active_b)
        .await
        .expect("upsert b");

    // Persisted Active, but tmux does not confirm it — excluded.
    let mut active_no_tmux = make_record(None);
    active_no_tmux.tmux_name = "tm-proj-ghost".into();
    active_no_tmux.state = ManagedSessionState::Active;
    active_no_tmux.workspace_path = Some(PathBuf::from("/repo/ghost"));
    mgr.store
        .write()
        .await
        .upsert(active_no_tmux)
        .await
        .expect("upsert ghost");

    // Stopped, even though tmux would confirm it if asked — excluded.
    let mut stopped = make_record(None);
    stopped.tmux_name = "tm-proj-stopped".into();
    stopped.state = ManagedSessionState::Stopped;
    stopped.workspace_path = Some(PathBuf::from("/repo/stopped"));
    mgr.store
        .write()
        .await
        .upsert(stopped)
        .await
        .expect("upsert stopped");

    let set = decode(&active_projects_core(&state).await);
    assert_eq!(set.projects.len(), 2, "projects: {:?}", set.projects);
}

/// (b) Two sessions on one root → one project, two `session_ids`.
#[tokio::test]
async fn two_sessions_on_one_root_group_into_one_project() {
    let (state, driver, _dir) = test_state().await;
    driver.set_live(&["tm-shared-1", "tm-shared-2"]);
    let mgr = state.session_manager().await;

    let root = PathBuf::from("/repo/shared");
    let mut s1 = make_record(None);
    s1.tmux_name = "tm-shared-1".into();
    s1.state = ManagedSessionState::Active;
    s1.workspace_path = Some(root.clone());
    let id1 = s1.id;
    mgr.store.write().await.upsert(s1).await.expect("upsert s1");

    let mut s2 = make_record(None);
    s2.tmux_name = "tm-shared-2".into();
    s2.state = ManagedSessionState::Provisioning;
    s2.workspace_path = Some(root.clone());
    let id2 = s2.id;
    mgr.store.write().await.upsert(s2).await.expect("upsert s2");

    let set = decode(&active_projects_core(&state).await);
    assert_eq!(set.projects.len(), 1, "projects: {:?}", set.projects);
    let project = &set.projects[0];
    assert_eq!(project.root, root);
    let mut ids = project.session_ids.clone();
    ids.sort();
    let mut expected = vec![id1.to_string(), id2.to_string()];
    expected.sort();
    assert_eq!(ids, expected);
}

/// (c) A worktree root resolves to two `index_ids` (the worktree's own, plus
/// its base checkout's).
#[tokio::test]
async fn worktree_root_yields_two_index_ids() {
    let fixture = crate::session_manager::worktree_git_fixture::GitWorktreeFixture::new();
    let worktree = fixture.add_worktree("wt-residency");

    let (state, driver, _dir) = test_state().await;
    driver.set_live(&["tm-wt"]);
    let mgr = state.session_manager().await;

    let mut record = make_record(None);
    record.tmux_name = "tm-wt".into();
    record.state = ManagedSessionState::Active;
    record.workspace_path = Some(worktree.clone());
    mgr.store
        .write()
        .await
        .upsert(record)
        .await
        .expect("upsert worktree session");

    let set = decode(&active_projects_core(&state).await);
    assert_eq!(set.projects.len(), 1, "projects: {:?}", set.projects);
    assert_eq!(
        set.projects[0].index_ids.len(),
        2,
        "index_ids: {:?}",
        set.projects[0].index_ids
    );
}

/// (d) A `list_sessions` failure serves the PERSISTED state rather than
/// collapsing to empty (#6836) — fail closed, never fail empty.
#[tokio::test]
async fn tmux_list_failure_serves_persisted_state() {
    let (state, driver, _dir) = test_state().await;
    driver.set_live(&["tm-during-create"]);
    let mgr = state.session_manager().await;

    let mut record = make_record(None);
    record.tmux_name = "tm-during-create".into();
    record.state = ManagedSessionState::Active;
    record.workspace_path = Some(PathBuf::from("/repo/failclosed"));
    mgr.store
        .write()
        .await
        .upsert(record)
        .await
        .expect("upsert");

    // Now the producer's OWN list_sessions call fails.
    driver.set_failing("enumeration failed");

    let set = decode(&active_projects_core(&state).await);
    assert_eq!(
        set.projects.len(),
        1,
        "a tmux probe failure must still serve persisted Active/Provisioning \
         records: {:?}",
        set.projects
    );
}

/// (e) The residency generation increments across `create`/`stop`, and the
/// route reflects the CURRENT value without recomputing it from scratch.
#[tokio::test]
async fn generation_increments_across_create_and_stop() {
    let (state, driver, _dir) = test_state().await;
    let mgr = state.session_manager().await;

    let before = decode(&active_projects_core(&state).await).generation;

    let record = mgr
        .create(
            "residency generation test".into(),
            Some(PathBuf::from("/tmp/residency-gen")),
            None,
            None,
            None,
            None,
        )
        .await
        .expect("create");
    driver.set_live(&[record.tmux_name.as_str()]);

    let after_create = decode(&active_projects_core(&state).await).generation;
    assert!(
        after_create > before,
        "create must bump the generation: before={before} after={after_create}"
    );

    mgr.stop_with_cause(&record.id, StopCause::Deliberate)
        .await
        .expect("stop");

    let after_stop = decode(&active_projects_core(&state).await).generation;
    assert!(
        after_stop > after_create,
        "stop must bump the generation: after_create={after_create} after_stop={after_stop}"
    );
}
