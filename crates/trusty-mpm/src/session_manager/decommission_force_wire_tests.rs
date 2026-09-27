//! Manager-level tests for #8688: `decommission_with_root_checked` hands the
//! record's task to both removal routes.
//!
//! Why: the route-level tests pass the task in by hand, so a call site that
//! passed `None` would keep every task-bearing tree and no test would notice.
//! What: a real `SessionManager` record, a real [`GitWorktreeFixture`] tree
//! holding `TASK.md`, and `--force` through the manager, on the in-project
//! route (`workspace_owned == false`) and the owned route.
//! Test: this file IS the test module.

use std::path::{Path, PathBuf};

use crate::session_manager::decommission_force::{DecommissionReport, ProvisioningDirt};
use crate::session_manager::manager::SessionManager;
use crate::session_manager::record::ManagedSessionId;
use crate::session_manager::tests::FakeTmuxDriver;
use crate::session_manager::worktree_git_fixture::GitWorktreeFixture;
use crate::session_manager::worktree_ownership_location::write_sentinel_bytes;

/// The record's task, which `write_task_md` writes to `TASK.md` verbatim.
const TASK: &str = "Fix the bug\n";

/// A clean worktree tm created, holding `TASK.md` with `task_md`.
fn tree_with_task_md(fx: &GitWorktreeFixture, name: &str, task_md: &str) -> PathBuf {
    let wt = fx.add_worktree(name);
    write_sentinel_bytes(&wt, b"").expect("write the admin-dir marker");
    std::fs::write(wt.join("TASK.md"), task_md).expect("write TASK.md");
    wt
}

/// `--force` decommission of `ws` through the manager, for a record whose
/// task is [`TASK`], owned or not, and errored once when `errored`.
async fn force_decommission(
    fx: &GitWorktreeFixture,
    ws: &Path,
    owned: bool,
    errored: bool,
) -> DecommissionReport {
    let store = crate::test_support::hermetic_temp_dir();
    let mgr = SessionManager::new(store.path(), FakeTmuxDriver::new())
        .await
        .expect("manager");
    let record = mgr
        .create_with_id(
            ManagedSessionId::new(),
            TASK.into(),
            Some(ws.to_path_buf()),
            None,
            Some(ws.to_path_buf()),
            None,
            None,
            crate::runtime::RuntimeKind::default(),
            false,
            owned,
        )
        .await
        .expect("create");
    if errored {
        // The real writer, so the note's shape cannot drift from the rule.
        mgr.mark_errored(&record.id, "spawn failed")
            .await
            .expect("mark errored");
    }
    mgr.decommission_with_root_checked(
        &record.id,
        &fx.repos_root,
        None,
        true,
        ProvisioningDirt::Discard,
    )
    .await
    .expect("a refusal is not an error")
}

/// #8688 round 3: an unedited `TASK.md` does not keep the tree on either
/// route, errored once or not. Fails when either call site in
/// `decommission_with_root_checked` passes `None` for the task, and, for the
/// errored rows, at 9465af6d2.
#[tokio::test]
async fn force_decommission_through_the_manager_removes_an_unedited_task_md_tree() {
    for owned in [false, true] {
        for errored in [false, true] {
            let fx = GitWorktreeFixture::new();
            let name = format!("wire-8688-owned-{owned}-errored-{errored}");
            let wt = tree_with_task_md(&fx, &name, TASK);

            let report = force_decommission(&fx, &wt, owned, errored).await;

            assert!(
                report.workspace_removed,
                "{name}: kept: {:?}",
                report.workspace_kept_reason
            );
            assert!(!wt.exists(), "{name}: the tree is gone");
        }
    }
}

/// #8688 round 3: an edited `TASK.md` keeps the tree on both routes, and the
/// refusal names it.
#[tokio::test]
async fn force_decommission_through_the_manager_keeps_an_edited_task_md_tree() {
    for owned in [false, true] {
        let fx = GitWorktreeFixture::new();
        let name = format!("wire-8688-edited-owned-{owned}");
        let edited = format!("{TASK}\n## Notes\n- keep me\n");
        let wt = tree_with_task_md(&fx, &name, &edited);

        let report = force_decommission(&fx, &wt, owned, true).await;

        assert!(!report.workspace_removed, "{name}: must be kept");
        let body = std::fs::read_to_string(wt.join("TASK.md")).expect("TASK.md survives");
        assert_eq!(body, edited, "{name}");
        let reason = report.workspace_kept_reason.expect("a kept tree names why");
        assert!(reason.contains("TASK.md"), "{name}: {reason}");
    }
}
