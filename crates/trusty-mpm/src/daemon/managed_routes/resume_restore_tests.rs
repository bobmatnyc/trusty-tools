//! A resume after a project decommission unparks the project's index (#10989
//! F3).
//!
//! Why: decommission keeps the records resumable (ruling D8e), so the resume
//! route must undo the decommission park exactly as the spawn route does.
//! What: decommissions a project against a stateful trusty-search stand-in,
//! then drives the real `resume_managed` and asserts the park is gone.
//! Test: this file.

use std::sync::Arc;
use std::time::Duration;

use crate::daemon::state::DaemonState;
use crate::project::Project;
use crate::session_manager::index_restore::METHOD_UNPARK;
use crate::session_manager::index_restore::tests::{Parked, stateful_search};
use crate::session_manager::index_shutdown::tests::{healthy_memory, rig};
use crate::session_manager::{ManagedError, ManagedSessionState, ManagedTmuxDriver};

const REPO: &str = "https://github.com/acme/widget";

/// A driver whose panes name their sessions and that refuses the launch
/// line, so the route ends quickly after the restore ran.
struct RefuseLaunchDriver;

impl ManagedTmuxDriver for RefuseLaunchDriver {
    fn create_session(&self, _name: &str, _workdir: &str) -> Result<(), ManagedError> {
        Ok(())
    }

    fn kill_session(&self, _name: &str) -> Result<(), ManagedError> {
        Ok(())
    }

    fn send_line(&self, _name: &str, text: &str) -> Result<(), ManagedError> {
        if text.contains(" --launch-spec ") {
            return Err(ManagedError::TmuxUnavailable("launch refused".into()));
        }
        Ok(())
    }

    fn capture(&self, _name: &str, _lines: usize) -> Result<String, ManagedError> {
        Ok(String::new())
    }

    fn list_sessions(&self) -> Result<Vec<String>, ManagedError> {
        Ok(Vec::new())
    }

    fn get_pane_id(&self, name: &str) -> Option<String> {
        Some(crate::test_support::self_describing_pane(name))
    }

    fn pane_identity(
        &self,
        pane_id: &str,
    ) -> Result<crate::session_manager::pane_identity::PaneIdentity, ManagedError> {
        Ok(crate::test_support::self_describing_identity(pane_id))
    }
}

/// #10989 F3: decommission parks the index; the operator resume of a session
/// it stopped unparks it.
/// Test: this function IS the test. Fails if `resume_managed` does not run
/// `SessionManager::restore_parked_index`.
#[tokio::test]
async fn a_resume_after_a_decommission_unparks_the_index() {
    let dir = tempfile::tempdir().expect("tempdir");
    let state = Arc::new(
        DaemonState::with_root_isolated_managed_and_driver(
            dir.path().join("framework"),
            Arc::new(RefuseLaunchDriver),
        )
        .await,
    );
    let mgr = state.session_manager().await;
    let registry = state.project_registry().await;
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
            archived_at: None,
        })
        .await
        .expect("register");
    let ws = dir.path().join("ws");
    std::fs::create_dir_all(&ws).expect("workspace dir");
    let record = mgr
        .create(
            "restore-on-resume".into(),
            Some(ws.clone()),
            None,
            None,
            None,
            None,
        )
        .await
        .expect("create");
    let id = record.id;
    mgr.set_workspace(&id, ws, ManagedSessionState::Active)
        .await
        .expect("set Active");
    let mut record = mgr.get(&id).await.expect("record");
    record.repo_url = Some(REPO.into());
    mgr.store.write().await.upsert(record).await.expect("bind");

    let parked: Parked = Arc::default();
    let rig = rig(stateful_search(&parked), healthy_memory()).await;
    mgr.decommission_project_with(&registry, "widget", Ok(rig.capability()))
        .await
        .expect("decommission");
    assert_eq!(
        parked.lock().expect("parked").len(),
        1,
        "premise: decommission parked the index"
    );
    assert_eq!(
        mgr.get(&id).await.expect("record").state,
        ManagedSessionState::Stopped
    );
    let _ = mgr.index_restore_socket.set(rig.search_socket());

    // The refused launch line ends the route in an error; the restore ran first.
    let _ = tokio::time::timeout(
        Duration::from_secs(60),
        super::lifecycle::resume_managed(&state, &id),
    )
    .await
    .expect("resume_managed must finish inside the budget");

    assert!(
        parked.lock().expect("parked").is_empty(),
        "the resume left the decommission park in place: {:?}",
        rig.methods()
    );
    assert!(rig.methods().contains(&format!("search:{METHOD_UNPARK}")));
}
