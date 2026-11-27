//! An operator resume reactivates the decommissioned project its session binds
//! to (#11983), by the rule `spawn_managed` uses.
//!
//! Why: `resume_managed` unparked the index but left `decommissioned_at` and
//! `archived_at` set, so a running session's project still read decommissioned.
//! What: decommissions a project against a stateful trusty-search stand-in,
//! drives the real `resume_managed` over a driver that accepts the launch line,
//! and asserts on the registry record, the park, and the route's result.
//! Test: this file.

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;

use crate::daemon::state::DaemonState;
use crate::project::Project;
use crate::session_manager::index_restore::METHOD_UNPARK;
use crate::session_manager::index_restore::tests::{Parked, stateful_search};
use crate::session_manager::index_shutdown::tests::{Rig, healthy_memory, rig};
use crate::session_manager::{
    ManagedError, ManagedSessionId, ManagedSessionState, ManagedTmuxDriver, SessionRecord,
};

use super::resume_error::ResumeManagedError;

const REPO: &str = "https://github.com/acme/widget";

/// A driver whose panes name their sessions and that accepts every line, so
/// the route runs to its end. It lists no sessions, so the post-send launch
/// check reads "unverifiable" and changes nothing.
struct AcceptDriver;

impl ManagedTmuxDriver for AcceptDriver {
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

/// A live (never decommissioned) project record named `name` for `repo_url`.
fn project(name: &str, repo_url: &str, key: &str) -> Project {
    Project {
        name: name.into(),
        repo_url: repo_url.into(),
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
        project_key: Some(key.into()),
        decommissioned_at: None,
        archived_at: None,
    }
}

/// A daemon with project `widget` registered and one `Active` session bound
/// to it, over [`AcceptDriver`].
async fn bound_session(dir: &std::path::Path) -> (Arc<DaemonState>, ManagedSessionId) {
    let state = Arc::new(
        DaemonState::with_root_isolated_managed_and_driver(
            dir.join("framework"),
            Arc::new(AcceptDriver),
        )
        .await,
    );
    let mgr = state.session_manager().await;
    state
        .project_registry()
        .await
        .register(project("widget", REPO, "acme/widget"))
        .await
        .expect("register");
    let ws = dir.join("ws");
    std::fs::create_dir_all(&ws).expect("workspace dir");
    let record = mgr
        .create(
            "reactivate-on-resume".into(),
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
    (state, id)
}

/// Decommission and archive `widget` against a stateful search that parks
/// into `parked`, then point the restore at that search. Returns the rig,
/// which must outlive the resume.
async fn decommission(state: &Arc<DaemonState>, id: &ManagedSessionId, parked: &Parked) -> Rig {
    let mgr = state.session_manager().await;
    let registry = state.project_registry().await;
    let rig = rig(stateful_search(parked), healthy_memory()).await;
    mgr.decommission_project_with(&registry, "widget", Ok(rig.capability()))
        .await
        .expect("decommission");
    registry
        .mark_archived("widget", Utc::now())
        .await
        .expect("archive");
    let _ = mgr.index_restore_socket.set(rig.search_socket());
    let widget = registry.get("widget").await.expect("row");
    assert!(
        widget.is_decommissioned() && widget.is_archived(),
        "premise"
    );
    assert_eq!(
        mgr.get(id).await.expect("record").state,
        ManagedSessionState::Stopped,
        "premise: decommission stopped the session"
    );
    rig
}

/// Run the real operator resume route inside a budget.
async fn resume(
    state: &Arc<DaemonState>,
    id: &ManagedSessionId,
) -> Result<SessionRecord, ResumeManagedError> {
    tokio::time::timeout(
        Duration::from_secs(60),
        super::lifecycle::resume_managed(state, id),
    )
    .await
    .expect("resume_managed must finish inside the budget")
}

/// #11983 criteria 1 and 2: an operator resume clears `decommissioned_at` and
/// `archived_at` on the bound project and still unparks its index.
/// Test: this function IS the test. Fails if `resume_managed` does not run
/// `ProjectRegistry::reactivate_for_session`, or stops unparking.
#[tokio::test]
async fn an_operator_resume_reactivates_the_bound_decommissioned_project() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (state, id) = bound_session(dir.path()).await;
    let parked: Parked = Arc::default();
    let rig = decommission(&state, &id, &parked).await;

    let got = resume(&state, &id).await;

    assert!(got.is_ok(), "the resume must succeed: {got:?}");
    let widget = state
        .project_registry()
        .await
        .get("widget")
        .await
        .expect("row");
    assert!(
        !widget.is_decommissioned(),
        "the resumed session's project still reads decommissioned"
    );
    assert!(
        !widget.is_archived(),
        "the resumed session's project still reads archived"
    );
    assert!(
        parked.lock().expect("parked").is_empty(),
        "the resume left the decommission park in place: {:?}",
        rig.methods()
    );
    assert!(rig.methods().contains(&format!("search:{METHOD_UNPARK}")));
}

/// #11983 criterion 1: a decommissioned project the resumed session does not
/// bind to stays decommissioned.
/// Test: this function IS the test. Fails if the resume reactivates projects
/// other than the one `resolve_session_project` binds the record to.
#[tokio::test]
async fn an_operator_resume_leaves_an_unbound_decommissioned_project_decommissioned() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (state, id) = bound_session(dir.path()).await;
    let parked: Parked = Arc::default();
    let _rig = decommission(&state, &id, &parked).await;
    let registry = state.project_registry().await;
    registry
        .register(project(
            "gadget",
            "https://github.com/acme/gadget",
            "acme/gadget",
        ))
        .await
        .expect("register gadget");
    registry
        .mark_decommissioned("gadget", Utc::now())
        .await
        .expect("decommission gadget");

    let got = resume(&state, &id).await;

    assert!(got.is_ok(), "the resume must succeed: {got:?}");
    assert!(
        registry
            .get("gadget")
            .await
            .expect("row")
            .is_decommissioned(),
        "a resume reactivated a project its session does not bind to"
    );
}

/// #11983 criterion 3, Fail-Open Check: a reactivation that fails on operator
/// resume is the route's error, and the project record does not advance.
/// Test: this function IS the test. Fails if `resume_managed` drops the
/// reactivation failure, reports another error in its place, or answers `Ok`.
#[tokio::test]
async fn a_failed_reactivation_on_operator_resume_is_the_route_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (state, id) = bound_session(dir.path()).await;
    let parked: Parked = Arc::default();
    let _rig = decommission(&state, &id, &parked).await;
    let file = crate::project::registry_data_dir_under(&dir.path().join("framework"))
        .join("projects.json");
    let saved = std::fs::read(&file).expect("read the registry");
    std::fs::write(&file, "{ not json").expect("corrupt the registry");

    let got = resume(&state, &id).await;

    let err = got.expect_err("a failed reactivation must fail the resume");
    let msg = err.to_string();
    assert!(
        msg.contains("could not be reactivated"),
        "the route error must be the reactivation failure: {msg}"
    );
    assert!(
        matches!(err, ResumeManagedError::Other(_)),
        "a reactivation failure is an internal route error: {err:?}"
    );
    assert_eq!(
        std::fs::read_to_string(&file).expect("re-read"),
        "{ not json",
        "nothing may rewrite the registry after a failed reactivation"
    );
    std::fs::write(&file, saved).expect("restore the registry");
    assert!(
        state
            .project_registry()
            .await
            .get("widget")
            .await
            .expect("row")
            .is_decommissioned(),
        "a failed reactivation must leave the record decommissioned"
    );
}
