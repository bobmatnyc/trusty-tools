//! The in-place reactivate reactivates the bound project and unparks its
//! index (#12012).
//!
//! Why: bare `tm` in a managed pane relaunches through
//! `POST .../{id}/reactivate`, not spawn or resume, so it owes the same two
//! project steps (#10988, #10989) and must never answer an error while the
//! record reads `Active`.
//! What: decommissions project `widget` against a stateful trusty-search
//! stand-in (the `resume_restore_tests` rig), then drives `reactivate_core`
//! and asserts on the project row, the park and the session record.
//! Test: this file.

use std::sync::Arc;
use std::time::Duration;

use super::super::resume_restore_tests::{bound_session, decommission};
use super::{ReactivateQuery, reactivate_core};
use crate::daemon::rpc::managed::outcome::RouteOutcome;
use crate::daemon::state::DaemonState;
use crate::project::Project;
use crate::session_manager::index_restore::METHOD_MAP;
use crate::session_manager::index_restore::METHOD_UNPARK;
use crate::session_manager::index_restore::tests::{Parked, row, stateful_search};
use crate::session_manager::index_shutdown::tests::{Answer, Rig, healthy_memory, rig};
use crate::session_manager::{ManagedSessionId, ManagedSessionState};
use crate::uds_mock::RpcError;

/// Run the in-place reactivate for `id` with no caller pane.
async fn reactivate(state: &Arc<DaemonState>, id: &ManagedSessionId) -> RouteOutcome {
    reactivate_core(state, &id.to_string(), &ReactivateQuery::default()).await
}

/// Whether project `name` reads decommissioned or archived.
async fn retired(state: &Arc<DaemonState>, name: &str) -> bool {
    let p = state
        .project_registry()
        .await
        .get(name)
        .await
        .expect("project row");
    p.is_decommissioned() || p.is_archived()
}

async fn session_state(state: &Arc<DaemonState>, id: &ManagedSessionId) -> ManagedSessionState {
    state
        .session_manager()
        .await
        .get(id)
        .await
        .expect("record")
        .state
}

/// #12012 AC1+AC2: a successful in-place reactivate of a session bound
/// to a decommissioned project clears the project's retired stamps and
/// unparks its index.
/// Test: this function IS the test. Fails if `reactivate_core` only flips the
/// record (the pre-#12012 behavior).
#[tokio::test]
async fn an_inplace_reactivate_reactivates_the_bound_project_and_unparks_its_index() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (state, id) = bound_session(dir.path()).await;
    let parked: Parked = Arc::default();
    let rig = decommission(&state, &id, &parked).await;
    let _ = state
        .session_manager()
        .await
        .index_restore_socket
        .set(rig.search_socket());
    assert!(retired(&state, "widget").await, "premise: decommissioned");

    let got = reactivate(&state, &id).await;

    assert_eq!(got.status, 200, "{got:?}");
    assert_eq!(
        session_state(&state, &id).await,
        ManagedSessionState::Active
    );
    assert!(
        !retired(&state, "widget").await,
        "the bound project still reads decommissioned"
    );
    assert!(
        parked.lock().expect("parked").is_empty(),
        "the reactivate left the decommission park in place: {:?}",
        rig.methods()
    );
    assert!(rig.methods().contains(&format!("search:{METHOD_UNPARK}")));
}

/// #12012 AC1: a decommissioned project the session does not bind
/// to stays decommissioned.
/// Test: this function IS the test. Fails if the reactivate clears every
/// decommissioned project rather than the bound one.
#[tokio::test]
async fn an_inplace_reactivate_leaves_an_unbound_decommissioned_project_alone() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (state, id) = bound_session(dir.path()).await;
    let parked: Parked = Arc::default();
    let rig = decommission(&state, &id, &parked).await;
    let registry = state.project_registry().await;
    registry
        .register(Project {
            name: "gadget".into(),
            repo_url: "https://github.com/acme/gadget".into(),
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
            project_key: Some("acme/gadget".into()),
            decommissioned_at: None,
            archived_at: None,
        })
        .await
        .expect("register gadget");
    let gadget_rig = rig_for_unbound(&state).await;
    assert!(
        retired(&state, "gadget").await,
        "premise: gadget decommissioned"
    );
    let _ = state
        .session_manager()
        .await
        .index_restore_socket
        .set(rig.search_socket());

    let got = reactivate(&state, &id).await;

    assert_eq!(got.status, 200, "{got:?}");
    assert!(!retired(&state, "widget").await);
    assert!(
        retired(&state, "gadget").await,
        "an unbound decommissioned project was reactivated"
    );
    drop(gadget_rig);
}

/// Decommission `gadget`, which no session binds to.
async fn rig_for_unbound(state: &Arc<DaemonState>) -> Rig {
    let parked: Parked = Arc::default();
    let rig = rig(stateful_search(&parked), healthy_memory()).await;
    state
        .session_manager()
        .await
        .decommission_project_with(
            &*state.project_registry().await,
            "gadget",
            Ok(rig.capability()),
        )
        .await
        .expect("decommission gadget");
    rig
}

/// #12012 AC2 plus the rollback ruling (Fail-Open Check): an unpark the
/// search daemon refuses is the route's error, and the record is rolled back off `Active`
/// so the client never execs into a ghost.
/// Test: this function IS the test. Fails if the restore error is dropped, or
/// the route answers an error with the record still `Active`.
#[tokio::test]
async fn a_failed_unpark_rolls_the_reactivate_back_to_stopped() {
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

    let got = reactivate(&state, &id).await;

    assert_eq!(got.status, 500, "{got:?}");
    let body = format!("{:?}", got.body);
    assert!(
        body.contains("widget-idx") && body.contains("still parked"),
        "{body}"
    );
    assert!(body.contains("rolled back to stopped"), "{body}");
    assert_eq!(
        session_state(&state, &id).await,
        ManagedSessionState::Stopped,
        "an error return left the session record Active"
    );
}

/// #12012 AC2 plus the rollback ruling: a project-registry failure is the
/// route's error, the index step never runs, and the record is rolled back.
/// Test: this function IS the test. Fails if a registry failure is dropped or
/// leaves the record `Active`.
#[tokio::test]
async fn a_failed_project_reactivation_rolls_the_reactivate_back() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (state, id) = bound_session(dir.path()).await;
    let parked: Parked = Arc::default();
    let rig = decommission(&state, &id, &parked).await;
    let _ = state
        .session_manager()
        .await
        .index_restore_socket
        .set(rig.search_socket());
    let projects = crate::project::registry_data_dir_under(&dir.path().join("framework"))
        .join("projects.json");
    std::fs::write(&projects, "{ not json").expect("corrupt the registry");

    let got = reactivate(&state, &id).await;

    assert_eq!(got.status, 500, "{got:?}");
    let body = format!("{:?}", got.body);
    assert!(body.contains("could not be reactivated"), "{body}");
    assert_eq!(
        session_state(&state, &id).await,
        ManagedSessionState::Stopped
    );
    assert!(
        !rig.methods().contains(&format!("search:{METHOD_UNPARK}")),
        "the index step ran after the project step failed"
    );
}

/// #12012 AC3: a refused reactivate (workspace gone) leaves the
/// project decommissioned and the index parked.
/// Test: this function IS the test. Fails if the project steps run before the
/// reactivate's own refusal.
#[tokio::test]
async fn a_refused_inplace_reactivate_leaves_the_project_and_index_alone() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (state, id) = bound_session(dir.path()).await;
    let parked: Parked = Arc::default();
    let rig = decommission(&state, &id, &parked).await;
    let _ = state
        .session_manager()
        .await
        .index_restore_socket
        .set(rig.search_socket());
    std::fs::remove_dir_all(dir.path().join("ws")).expect("remove workspace");

    let got = reactivate(&state, &id).await;

    assert_eq!(got.status, 409, "{got:?}");
    assert!(
        retired(&state, "widget").await,
        "a refusal reactivated the project"
    );
    assert_eq!(
        parked.lock().expect("parked").len(),
        1,
        "a refusal unparked the index"
    );
    assert_eq!(
        session_state(&state, &id).await,
        ManagedSessionState::Stopped
    );

    let unknown = reactivate(&state, &ManagedSessionId::new()).await;
    assert_eq!(unknown.status, 404, "{unknown:?}");
    assert!(retired(&state, "widget").await);
}

/// How long the stub trusty-search takes to confirm an unpark: past the
/// pre-fix client budget (`bin/tm`'s 2 s `PROBE_TIMEOUT`).
const SLOW_UNPARK: Duration = Duration::from_secs(3);

/// A trusty-search whose map lists `widget-idx` as decommission-parked and
/// whose unpark answers only after [`SLOW_UNPARK`] (an index reload).
async fn slow_unpark_search() -> crate::uds_mock::MockUdsDaemon {
    crate::uds_mock::spawn(|method, _params| {
        let method = method.to_string();
        Box::pin(async move {
            match method.as_str() {
                METHOD_MAP => Ok(serde_json::json!({
                    "parked": [row("widget-idx", "acme/widget", "decommissioned")]
                })),
                METHOD_UNPARK => {
                    tokio::time::sleep(SLOW_UNPARK).await;
                    Ok(serde_json::json!({ "unparked": true }))
                }
                _ => Ok(serde_json::json!({})),
            }
        })
    })
    .await
}

/// Wait up to `bound` for `id`'s record to leave `Active`; answers its state.
async fn state_after_settling(
    state: &Arc<DaemonState>,
    id: &ManagedSessionId,
    bound: Duration,
) -> ManagedSessionState {
    let deadline = tokio::time::Instant::now() + bound;
    loop {
        let now = session_state(state, id).await;
        if now != ManagedSessionState::Active || tokio::time::Instant::now() >= deadline {
            return now;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// #12012 (critic HIGH): a client that gives up on a slow unpark drops the
/// route mid-step. The record must still end up off `Active`, since that
/// client never execs.
/// Test: this function IS the test. Fails if the post-mark steps run inside
/// the route future, where the drop skips the rollback.
#[tokio::test]
async fn a_client_that_gives_up_on_a_slow_unpark_never_leaves_the_record_active() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (state, id) = bound_session(dir.path()).await;
    let parked: Parked = Arc::default();
    let _decommission_rig = decommission(&state, &id, &parked).await;
    let search = slow_unpark_search().await;
    let _ = state
        .session_manager()
        .await
        .index_restore_socket
        .set(search.socket().to_path_buf());

    let gave_up = tokio::time::timeout(Duration::from_secs(2), reactivate(&state, &id)).await;
    assert!(gave_up.is_err(), "premise: the 2 s client budget expires first");

    assert_eq!(
        state_after_settling(&state, &id, SLOW_UNPARK * 4).await,
        ManagedSessionState::Stopped,
        "an abandoned reactivate left the session record Active"
    );
}
