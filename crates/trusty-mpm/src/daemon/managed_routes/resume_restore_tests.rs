//! A resume after a project decommission unparks the project's index (#10989
//! F3), on the operator route and on auto-resume (#11230).
//!
//! Why: decommission keeps the records resumable (ruling D8e), so both resume
//! paths must undo the decommission park exactly as the spawn route does.
//! What: decommissions a project against a stateful trusty-search stand-in,
//! then drives the real `resume_managed`, or `SessionManager::resume_auto`
//! through the installed `DaemonRelauncher`, and asserts on the park.
//! Test: this file.

use std::sync::Arc;
use std::time::Duration;

use tracing_subscriber::layer::SubscriberExt as _;

use crate::daemon::state::DaemonState;
use crate::project::Project;
use crate::session_manager::index_restore::tests::{Parked, row, stateful_search};
use crate::session_manager::index_restore::{METHOD_MAP, METHOD_UNPARK};
use crate::session_manager::index_shutdown::tests::{Answer, Rig, healthy_memory, rig};
use crate::session_manager::{
    ManagedError, ManagedSessionId, ManagedSessionState, ManagedTmuxDriver,
};
use crate::uds_mock::RpcError;

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

/// A daemon with project `widget` registered and one `Active` session bound
/// to it, over [`RefuseLaunchDriver`].
pub(super) async fn bound_session(dir: &std::path::Path) -> (Arc<DaemonState>, ManagedSessionId) {
    let state = Arc::new(
        DaemonState::with_root_isolated_managed_and_driver(
            dir.join("framework"),
            Arc::new(RefuseLaunchDriver),
        )
        .await,
    );
    let mgr = state.session_manager().await;
    state
        .project_registry()
        .await
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
    let ws = dir.join("ws");
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
    (state, id)
}

/// Decommission `widget` against a stateful search that parks into `parked`.
/// Returns the rig, which must outlive the test's restore.
pub(super) async fn decommission(state: &Arc<DaemonState>, id: &ManagedSessionId, parked: &Parked) -> Rig {
    let mgr = state.session_manager().await;
    let rig = rig(stateful_search(parked), healthy_memory()).await;
    mgr.decommission_project_with(
        &*state.project_registry().await,
        "widget",
        Ok(rig.capability()),
    )
    .await
    .expect("decommission");
    assert_eq!(
        parked.lock().expect("parked").len(),
        1,
        "premise: decommission parked the index"
    );
    assert_eq!(
        mgr.get(id).await.expect("record").state,
        ManagedSessionState::Stopped,
        "premise: decommission stopped the session"
    );
    rig
}

/// Run `SessionManager::resume_auto` with the daemon's own relauncher
/// installed, capturing every log line. Answers the outcome and the lines.
async fn auto_resume_logged(
    state: &Arc<DaemonState>,
    id: &ManagedSessionId,
) -> (Result<(), String>, Vec<String>) {
    super::auto_relaunch::install_auto_relauncher(state).await;
    let mgr = state.session_manager().await;
    let buffer = trusty_common::log_buffer::LogBuffer::new(256);
    let subscriber = tracing_subscriber::registry().with(
        trusty_common::log_buffer::LogBufferLayer::new(buffer.clone()),
    );
    let guard = tracing::subscriber::set_default(subscriber);
    let got = tokio::time::timeout(Duration::from_secs(60), mgr.resume_auto(id))
        .await
        .expect("resume_auto must finish inside the budget");
    drop(guard);
    (got.map(drop).map_err(|e| e.to_string()), buffer.tail(256))
}

/// #10989 F3: decommission parks the index; the operator resume of a session
/// it stopped unparks it.
/// Test: this function IS the test. Fails if `resume_managed` does not run
/// `SessionManager::restore_parked_index`.
#[tokio::test]
async fn a_resume_after_a_decommission_unparks_the_index() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (state, id) = bound_session(dir.path()).await;
    let parked: Parked = Arc::default();
    let rig = decommission(&state, &id, &parked).await;
    let mgr = state.session_manager().await;
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

/// #11230: auto-resume of a session a decommission stopped unparks the
/// project's index, as the operator resume does.
/// Test: this function IS the test. Fails if `DaemonRelauncher::relaunch`
/// does not run `SessionManager::restore_parked_index`.
#[tokio::test]
async fn an_auto_resume_after_a_decommission_unparks_the_index() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (state, id) = bound_session(dir.path()).await;
    let parked: Parked = Arc::default();
    let rig = decommission(&state, &id, &parked).await;
    let _ = state
        .session_manager()
        .await
        .index_restore_socket
        .set(rig.search_socket());

    // The refused launch line fails the relaunch; the restore ran first.
    let _ = auto_resume_logged(&state, &id).await;

    assert!(
        parked.lock().expect("parked").is_empty(),
        "auto-resume left the decommission park in place: {:?}",
        rig.methods()
    );
    assert!(rig.methods().contains(&format!("search:{METHOD_UNPARK}")));
}

/// #11230 Fail-Open Check: an unpark that fails on auto-resume is logged at
/// WARN naming the still-parked index, and it neither replaces nor skips the
/// launch: the relaunch outcome is the launch's own.
/// Test: this function IS the test. Fails if the relauncher drops the restore
/// error silently, or turns it into the relaunch's error before the launch.
#[tokio::test]
async fn a_failed_unpark_on_auto_resume_is_warned_and_the_launch_still_runs() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (state, id) = bound_session(dir.path()).await;
    let parked: Parked = Arc::default();
    let _decommission_rig = decommission(&state, &id, &parked).await;
    let refuse_unpark: Answer = Arc::new(|m: &str, _: &serde_json::Value| match m {
        METHOD_MAP => Ok(serde_json::json!({
            "parked": [row("widget-idx", "acme/widget", "decommissioned")]
        })),
        _ => Err(RpcError::new(-32603, "project map unwritable")),
    });
    let failing = rig(refuse_unpark, healthy_memory()).await;
    let _ = state
        .session_manager()
        .await
        .index_restore_socket
        .set(failing.search_socket());

    let (got, lines) = auto_resume_logged(&state, &id).await;

    let err = got.expect_err("the refused launch line fails the relaunch");
    assert!(
        err.contains("spawn_resume failed"),
        "the outcome must be the launch's own, so the launch ran: {err}"
    );
    assert!(
        !err.contains("still parked"),
        "a restore failure must not become the relaunch error: {err}"
    );
    assert!(
        failing
            .methods()
            .contains(&format!("search:{METHOD_UNPARK}")),
        "premise: the unpark was attempted: {:?}",
        failing.methods()
    );
    let warned = lines
        .iter()
        .find(|l| l.contains("widget-idx") && l.contains("still parked"));
    assert!(
        warned.is_some_and(|l| l.contains("WARN")),
        "the failed unpark must be logged at WARN, naming the index: {lines:#?}"
    );
}

/// #11230 criterion 4: auto-resume of a project that was never decommissioned
/// unparks nothing, and the relaunch outcome is unchanged.
/// Test: this function IS the test. Fails if the restore unparks a project
/// with no decommission park, or alters the relaunch outcome.
#[tokio::test]
async fn an_auto_resume_of_a_live_project_unparks_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (state, id) = bound_session(dir.path()).await;
    let mgr = state.session_manager().await;
    mgr.stop(&id).await.expect("stop");
    let parked: Parked = Arc::default();
    let search = rig(stateful_search(&parked), healthy_memory()).await;
    let _ = mgr.index_restore_socket.set(search.search_socket());

    let (got, _lines) = auto_resume_logged(&state, &id).await;

    let err = got.expect_err("the refused launch line fails the relaunch");
    assert!(err.contains("spawn_resume failed"), "{err}");
    assert!(
        !search
            .methods()
            .contains(&format!("search:{METHOD_UNPARK}")),
        "a never-decommissioned project was unparked: {:?}",
        search.methods()
    );
    assert!(
        !state
            .project_registry()
            .await
            .get("widget")
            .await
            .expect("row")
            .is_decommissioned()
    );
}
