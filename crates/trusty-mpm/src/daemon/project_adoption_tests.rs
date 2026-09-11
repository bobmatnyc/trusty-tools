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
