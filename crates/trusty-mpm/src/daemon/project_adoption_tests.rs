//! Both registration transports adopt the same worktrees, and say so (#7357).
//!
//! Why: #7357's first round wired the backfill into the MCP tool only, so
//! `tm projects register` — which goes through `POST /api/v1/projects` —
//! persisted a project record and adopted nothing. A test that exercises ONE
//! transport cannot catch that; these two drive both against the same checkout
//! and compare what each wrote.
//! What: a transport-parity test over the adoption store, and a response-shape
//! test asserting a skipped worktree reaches the caller.
//! Test: this file IS the test module.

use super::*;
use crate::daemon::managed_routes::project_registry_routes::{
    RegisterProjectBody, register_project_registry_op,
};
use crate::daemon::mcp_project::project_register;
use crate::project::{AdoptionStore, SkipReason};
use crate::session_manager::worktree_git_fixture::GitWorktreeFixture;

async fn isolated_state() -> Arc<DaemonState> {
    let root = tempfile::tempdir().expect("tempdir").keep();
    Arc::new(DaemonState::with_root_isolated_managed(root).await)
}

/// Every adoption record a state holds, as `(project, worktree basename)`.
fn adopted(state: &Arc<DaemonState>) -> Vec<(String, String)> {
    let store = AdoptionStore::load(&crate::project::registry_data_dir_under(
        state.framework_root(),
    ));
    let mut rows: Vec<(String, String)> = store
        .entries()
        .iter()
        .map(|e| {
            (
                e.project.clone(),
                e.path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned(),
            )
        })
        .collect();
    rows.sort();
    rows
}

fn register_body(name: &str, repo_url: &str) -> RegisterProjectBody {
    RegisterProjectBody {
        name: name.to_string(),
        repo_url: repo_url.to_string(),
        default_branch: None,
        description: None,
        tags: None,
        stack_hint: None,
        gh_user: None,
        gh_account: None,
        gh_config_dir: None,
        worktree: None,
    }
}

/// The CLI transport must adopt exactly what the MCP tool adopts (#7357 review
/// finding 1).
///
/// Why: `tm projects register` calls `POST /api/v1/projects`, whose body
/// persisted the record and ran no backfill — the reported bug, still live on
/// the command an operator is most likely to run. Red against the round-1 fix.
/// Test: this function IS the test.
#[tokio::test]
async fn parity_register_adopts_the_same_worktrees_across_transports() {
    let fx = GitWorktreeFixture::new();
    fx.add_worktree("pre-one");
    fx.add_worktree("pre-two");
    let checkout = fx.repo.to_str().expect("utf8 checkout");

    let via_mcp = isolated_state().await;
    project_register(
        &via_mcp, "gnomish", checkout, None, None, None, None, None, None,
    )
    .await
    .expect("MCP register");

    let via_cli = isolated_state().await;
    register_project_registry_op(&via_cli, register_body("gnomish", checkout))
        .await
        .expect("CLI/HTTP register");

    let mcp_rows = adopted(&via_mcp);
    assert_eq!(mcp_rows.len(), 2, "the MCP transport adopts both worktrees");
    assert_eq!(
        adopted(&via_cli),
        mcp_rows,
        "both transports must write the same adoption records"
    );
}

/// A skipped worktree reaches the caller in the registration RESPONSE, not
/// only a `tracing` line (#7357 review finding 4).
///
/// Why: registration still succeeds when a candidate cannot be adopted, so the
/// report is the only way a caller learns that a worktree stayed invisible.
/// Test: this function IS the test.
#[tokio::test]
async fn register_response_names_a_skipped_worktree() {
    let fx = GitWorktreeFixture::new();
    fx.add_worktree("pre-one");
    let broken = fx.repo.join(".claude").join("worktrees").join("broken");
    std::fs::create_dir_all(&broken).expect("broken worktree dir");
    let checkout = fx.repo.to_str().expect("utf8 checkout");

    let state = isolated_state().await;
    let response = register_project_registry_op(&state, register_body("gnomish", checkout))
        .await
        .expect("register must still succeed");

    assert_eq!(response.adoption.recorded, 1);
    assert_eq!(response.adoption.skipped, 1);
    let skipped = response
        .adoption
        .unrecorded
        .iter()
        .find(|u| u.reason == SkipReason::NoGitdirPointer)
        .expect("the skipped worktree must be named");
    assert!(skipped.path.ends_with("broken"), "got {:?}", skipped.path);

    // And it survives serialization — this is what an HTTP/RPC caller sees.
    let body = serde_json::to_value(&response).expect("serialize the response");
    assert_eq!(body["name"], "gnomish", "the record stays at the top level");
    assert_eq!(body["adoption"]["recorded"], 1);
    assert_eq!(
        body["adoption"]["unrecorded"][0]["reason"],
        "no_gitdir_pointer"
    );
}

/// Registry A — what `tm project init` calls — must adopt what registry B
/// adopts (#7588).
///
/// Why: #7357 wired the backfill into `POST /api/v1/projects` and the MCP tool,
/// leaving `POST /projects` registering a path and nothing else. The operator
/// who reached for the singular verb got exit 0, no `worktrees.json`, and
/// concluded the #7357 fix was broken. Red before this fix: the store is empty.
/// Test: this function IS the test.
#[tokio::test]
async fn register_project_op_backfills_pre_existing_worktrees_7588() {
    let fx = GitWorktreeFixture::new();
    fx.add_worktree("pre-one");
    fx.add_worktree("pre-two");

    let state = isolated_state().await;
    crate::daemon::rpc::registry::projects::register_project_op(&state, fx.repo.clone())
        .await
        .expect("the fixture has an origin remote");

    let rows = adopted(&state);
    let trees: Vec<&str> = rows.iter().map(|(_, t)| t.as_str()).collect();
    assert_eq!(
        trees,
        vec!["pre-one", "pre-two"],
        "tm project init must record the checkout's pre-existing worktrees"
    );
}

/// The two registration surfaces must attribute one checkout to ONE project
/// name (#7588).
///
/// Why: registry A derives a name from the directory and registry B takes the
/// operator's. Recording under both would make every worktree `ClaimedByAnother`
/// for whichever ran second — a correct registration reported as a wall of
/// skips. The store's existing attribution wins.
/// Test: this function IS the test.
#[tokio::test]
async fn register_project_op_adopts_under_the_owning_projects_name_7588() {
    let fx = GitWorktreeFixture::new();
    fx.add_worktree("pre-one");
    let checkout = fx.repo.to_str().expect("utf8 checkout");

    let state = isolated_state().await;
    register_project_registry_op(&state, register_body("gnomish", checkout))
        .await
        .expect("registry B registers");
    crate::daemon::rpc::registry::projects::register_project_op(&state, fx.repo.clone())
        .await
        .expect("the fixture has an origin remote");

    let rows = adopted(&state);
    assert_eq!(rows, vec![("gnomish".to_string(), "pre-one".to_string())]);
}

const WIDGET_URL: &str = "https://github.com/acme/widget";

/// A state holding `widget` decommissioned AND archived, over a trusty-search
/// stand-in whose `widget-idx` is parked from a decommission (#11969).
/// Answers the state, the parked rows and the rig, which must outlive the test.
async fn decommissioned_widget() -> (
    Arc<DaemonState>,
    crate::session_manager::index_restore::tests::Parked,
    crate::session_manager::index_shutdown::tests::Rig,
) {
    use crate::session_manager::index_restore::tests::{Parked, row, stateful_search};
    use crate::session_manager::index_shutdown::tests::{healthy_memory, rig};
    let state = isolated_state().await;
    let now = chrono::Utc::now();
    state
        .project_registry()
        .await
        .register(Project {
            name: "widget".into(),
            repo_url: WIDGET_URL.into(),
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
            decommissioned_at: Some(now),
            archived_at: Some(now),
        })
        .await
        .expect("register decommissioned widget");
    let parked: Parked = Arc::default();
    parked
        .lock()
        .expect("parked")
        .push(row("widget-idx", "acme/widget", "decommissioned"));
    let search = rig(stateful_search(&parked), healthy_memory()).await;
    let _ = state
        .session_manager()
        .await
        .index_restore_socket
        .set(search.search_socket());
    (state, parked, search)
}

/// Criteria 5 and 6 for one surface's serialized response: the flags stay
/// set, nothing unparked, and the body carries the start-a-session reason.
async fn assert_register_kept_it_decommissioned(
    state: &Arc<DaemonState>,
    parked: &crate::session_manager::index_restore::tests::Parked,
    search: &crate::session_manager::index_shutdown::tests::Rig,
    body: &serde_json::Value,
) {
    let widget = state
        .project_registry()
        .await
        .get("widget")
        .await
        .expect("widget row");
    assert!(
        widget.is_decommissioned() && widget.is_archived(),
        "a register call reactivated a decommissioned project: {widget:?}"
    );
    assert_eq!(
        parked.lock().expect("parked").len(),
        1,
        "the park was undone"
    );
    assert!(
        !search.methods().iter().any(|m| m.starts_with("search:")),
        "a register call reached trusty-search: {:?}",
        search.methods()
    );
    let reason = body["inactive_reason"].as_str().unwrap_or_default();
    assert!(
        reason.contains("decommissioned") && reason.contains("start a session"),
        "the response must name how to reactivate the project: {body:#}"
    );
}

/// #11969 criteria 5–6, MCP surface: `project_register` on a decommissioned
/// project keeps both flags, unparks nothing, and returns the reason.
/// Test: this function IS the test. Fails if the MCP body clears a flag,
/// unparks, or answers without `inactive_reason`.
#[tokio::test]
async fn mcp_project_register_keeps_a_decommissioned_project_and_names_the_way_back() {
    let (state, parked, search) = decommissioned_widget().await;
    let body = project_register(
        &state, "widget", WIDGET_URL, None, None, None, None, None, None,
    )
    .await
    .expect("MCP register");
    assert_register_kept_it_decommissioned(&state, &parked, &search, &body).await;
}

/// #11969 criteria 5–6, HTTP/RPC surface: `register_project_registry_op`
/// (`POST /api/v1/projects`, `mpm.projects.registry.register`) on a
/// decommissioned project keeps both flags, unparks nothing, and returns the
/// reason.
/// Test: this function IS the test. Fails if the HTTP body clears a flag,
/// unparks, or answers without `inactive_reason`.
#[tokio::test]
async fn http_project_register_keeps_a_decommissioned_project_and_names_the_way_back() {
    let (state, parked, search) = decommissioned_widget().await;
    let response = register_project_registry_op(&state, register_body("widget", WIDGET_URL))
        .await
        .expect("HTTP register");
    let body = serde_json::to_value(&response).expect("serialize the response");
    assert_register_kept_it_decommissioned(&state, &parked, &search, &body).await;
}

/// #11969: an active project's register response carries no `inactive_reason`
/// key at all, so existing callers see the body they saw before.
/// Test: this function IS the test.
#[tokio::test]
async fn register_of_an_active_project_omits_the_inactive_reason() {
    let state = isolated_state().await;
    let response = register_project_registry_op(&state, register_body("widget", WIDGET_URL))
        .await
        .expect("HTTP register");
    let body = serde_json::to_value(&response).expect("serialize the response");
    assert!(body.get("inactive_reason").is_none(), "{body:#}");
}
