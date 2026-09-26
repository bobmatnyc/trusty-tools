//! Tests for the content gates on decommission's `remove_dir_all` routes
//! (#8663).
//!
//! Why: an SM-owned workspace and a directory git has disowned were deleted
//! with no dirt, unpushed-commit or kept-output check. These tests drive real
//! git and a real `SessionManager`; a mocked git would test nothing.
//! What: effect 3 through `decommission_with_root`, the verdict through
//! `remove_owned_workspace`, and the disowned-directory fallback through
//! `remove_session_worktree`.
//! Test: this file IS the test module.

use std::path::{Path, PathBuf};
use std::process::Command;

use super::decommission::{WORKTREE_SENTINEL_FILE, remove_session_worktree};
use super::decommission_force::ProvisioningDirt;
use super::decommission_owned::{owned_workspace_keep_reason, remove_owned_workspace};
use super::manager::SessionManager;
use super::record::{ManagedSessionId, ManagedSessionState};
use super::tests::FakeTmuxDriver;
use super::worktree_git_fixture::{GitWorktreeFixture, deny_all};
use super::worktree_ignored_output::kept_unversioned_content;
use super::worktree_ownership::sentinel_payload_bytes;
use super::worktree_ownership_location::write_sentinel_bytes;
use super::worktree_safety::DirtyWorktreePolicy;

/// Run `git -C <dir> <args>` and assert it succeeded.
fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Write `rel` under `root`, creating parent directories. The ownership
/// sentinel is written empty, the owner-less form.
fn write(root: &Path, rel: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    let body = if rel == WORKTREE_SENTINEL_FILE {
        ""
    } else {
        "x\n"
    };
    std::fs::write(path, body).expect("write");
}

/// A manager plus an owned record whose workspace is `ws`.
async fn owned_session(store: &Path, ws: &Path) -> (SessionManager, ManagedSessionId) {
    let mgr = SessionManager::new(store, FakeTmuxDriver::new())
        .await
        .expect("manager");
    let record = mgr
        .create_with_id(
            ManagedSessionId::new(),
            "regression: #8663".into(),
            Some(ws.to_path_buf()),
            None,
            Some(ws.to_path_buf()),
            None,
            None,
            crate::runtime::RuntimeKind::default(),
            false,
            true, // owned: the `remove_dir_all` branch
        )
        .await
        .expect("create");
    (mgr, record.id)
}

/// Decommission an owned `ws` under `managed_root`; assert the tombstone and
/// that the workspace and its pointer were kept.
async fn assert_decommission_keeps(managed_root: &Path, ws: &Path) {
    let store = crate::test_support::hermetic_temp_dir();
    let (mgr, id) = owned_session(store.path(), ws).await;
    let (tomb, removed) = mgr
        .decommission_with_root(&id, managed_root, None)
        .await
        .expect("a refusal is not an error");
    assert!(!removed, "decommission must not report a removal");
    assert!(ws.exists(), "the workspace must stay: {}", ws.display());
    assert_eq!(tomb.state, ManagedSessionState::Decommissioned);
    assert_eq!(
        tomb.workspace_path.as_deref(),
        Some(ws),
        "a kept workspace keeps its pointer"
    );
}

/// A non-git owned workspace at `<root>/owner/repo/<leaf>`.
fn non_git_workspace(root: &Path, leaf: &str) -> PathBuf {
    let ws = root.join("owner").join("repo").join(leaf);
    std::fs::create_dir_all(&ws).expect("mkdir workspace");
    ws
}

/// #8663: an owned workspace that is a git worktree holding an unpushed
/// commit is kept, and the reason names the path and the commit.
#[tokio::test]
async fn owned_worktree_with_an_unpushed_commit_is_kept() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("owned-8663-commit");
    write(&wt, "work.rs");
    git(&wt, &["add", "work.rs"]);
    git(&wt, &["commit", "-q", "-m", "agent work"]);

    let reason =
        owned_workspace_keep_reason(&wt, &ManagedSessionId::new(), ProvisioningDirt::Refuse)
            .expect("an unpushed commit keeps the workspace");
    assert!(reason.contains(&wt.display().to_string()), "{reason}");
    assert!(reason.contains("1 unpushed commit"), "{reason}");
    assert_decommission_keeps(&fx.repos_root, &wt).await;
}

/// #8663: an untracked `results/out.json` keeps an owned worktree, and the
/// verdict decommission reports carries the reason.
#[tokio::test]
async fn owned_worktree_with_untracked_results_is_kept() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("owned-8663-results");
    write(&wt, "results/out.json");

    let verdict = remove_owned_workspace(&ManagedSessionId::new(), &wt, ProvisioningDirt::Refuse)
        .await
        .expect("a refusal is not an error");
    assert!(!verdict.removed);
    let reason = verdict.kept_reason.expect("the kept reason is reported");
    assert!(reason.contains("results/"), "{reason}");
    assert!(wt.join("results/out.json").exists());
    assert_decommission_keeps(&fx.repos_root, &wt).await;
}

/// #8663: gitignored run output keeps an owned worktree that is otherwise
/// clean and pushed.
#[tokio::test]
async fn owned_worktree_with_gitignored_results_is_kept() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("owned-8663-ignored");
    std::fs::write(fx.repo.join(".git/info/exclude"), "results/\n").expect("exclude");
    write(&wt, "results/out.json");

    let reason =
        owned_workspace_keep_reason(&wt, &ManagedSessionId::new(), ProvisioningDirt::Refuse)
            .expect("ignored output keeps the workspace");
    assert!(reason.contains("gitignored"), "{reason}");
    assert_decommission_keeps(&fx.repos_root, &wt).await;
}

/// #8663: `--force` excuses tm's provisioning files on an owned worktree
/// whose marker names the session, and nothing else.
#[test]
fn owned_worktree_force_excuses_only_provisioning_files() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("owned-8663-force");
    let me = ManagedSessionId::new();
    write_sentinel_bytes(&wt, &sentinel_payload_bytes(me)).expect("write the owner marker");
    write(&wt, "CLAUDE.md");
    assert!(owned_workspace_keep_reason(&wt, &me, ProvisioningDirt::Refuse).is_some());
    assert_eq!(
        owned_workspace_keep_reason(&wt, &me, ProvisioningDirt::Discard),
        None
    );
    write(&wt, "notes.md");
    let reason = owned_workspace_keep_reason(&wt, &me, ProvisioningDirt::Discard)
        .expect("--force never excuses user files");
    assert!(reason.contains("notes.md"), "{reason}");
}

/// #8663: a non-git owned workspace holding a user file is kept.
#[tokio::test]
async fn owned_non_git_workspace_with_user_files_is_kept() {
    let root = crate::test_support::hermetic_temp_dir();
    let ws = non_git_workspace(root.path(), "user-files");
    write(&ws, WORKTREE_SENTINEL_FILE);
    write(&ws, "notes.md");

    let reason =
        owned_workspace_keep_reason(&ws, &ManagedSessionId::new(), ProvisioningDirt::Refuse)
            .expect("a user file keeps the workspace");
    assert!(reason.contains(&ws.display().to_string()), "{reason}");
    assert!(reason.contains("notes.md"), "{reason}");
    assert_decommission_keeps(root.path(), &ws).await;
}

/// #8663: an owned workspace holding only harness files and build output is
/// still removed — git and non-git alike.
#[tokio::test]
async fn clean_owned_workspace_is_still_removed() {
    let root = crate::test_support::hermetic_temp_dir();
    let ws = non_git_workspace(root.path(), "clean");
    for rel in [
        WORKTREE_SENTINEL_FILE,
        ".trusty-mpm/state.json",
        ".claude/settings.json",
        "target/debug/app",
        "node_modules/pkg/index.js",
    ] {
        write(&ws, rel);
    }
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("owned-8663-clean");
    std::fs::write(fx.repo.join(".git/info/exclude"), "target/\n").expect("exclude");
    write(&wt, "target/debug/app");

    for (managed_root, path) in [(root.path(), &ws), (fx.repos_root.as_path(), &wt)] {
        let store = crate::test_support::hermetic_temp_dir();
        let (mgr, id) = owned_session(store.path(), path).await;
        let (tomb, removed) = mgr
            .decommission_with_root(&id, managed_root, None)
            .await
            .expect("decommission");
        assert!(
            removed,
            "a clean owned workspace is removed: {}",
            path.display()
        );
        assert!(!path.exists());
        assert_eq!(tomb.workspace_path, None);
    }
}

/// #8663: a content check that cannot complete keeps the workspace; it is not
/// a licence to delete, and not an I/O error either.
#[tokio::test]
async fn owned_workspace_is_kept_when_the_content_check_fails() {
    let root = crate::test_support::hermetic_temp_dir();
    let ws = non_git_workspace(root.path(), "unreadable");
    write(&ws, "data/run.json");
    let _restore = deny_all(&ws.join("data"));

    let reason =
        owned_workspace_keep_reason(&ws, &ManagedSessionId::new(), ProvisioningDirt::Refuse)
            .expect("an unreadable directory keeps the workspace");
    assert!(reason.contains("content check failed"), "{reason}");
    assert_decommission_keeps(root.path(), &ws).await;
}

/// A disowned leftover at `<tmp>/leftover` carrying the ownership sentinel.
fn leftover(root: &Path) -> PathBuf {
    let dir = root.join("leftover-8663");
    std::fs::create_dir_all(&dir).expect("mkdir");
    write(&dir, WORKTREE_SENTINEL_FILE);
    dir
}

/// #8663: a directory git has disowned is kept while it holds `results/x`.
#[test]
fn unclaimed_directory_with_results_is_kept() {
    let tmp = crate::test_support::hermetic_temp_dir();
    let dir = leftover(tmp.path());
    write(&dir, "results/x");

    let outcome = remove_session_worktree(&dir, "test", DirtyWorktreePolicy::Skip);
    let reason = outcome.reason().expect("kept").to_string();
    assert!(reason.contains("results"), "{reason}");
    assert!(dir.join("results/x").exists(), "the results must survive");
}

/// #8663: a disowned directory holding only the sentinel, harness files and
/// build output is still removed.
#[test]
fn unclaimed_directory_holding_only_harness_files_is_removed() {
    let tmp = crate::test_support::hermetic_temp_dir();
    let dir = leftover(tmp.path());
    write(&dir, ".trusty-mpm/state.json");
    write(&dir, "target/debug/app");

    let outcome = remove_session_worktree(&dir, "test", DirtyWorktreePolicy::Skip);
    assert!(outcome.removed(), "{outcome:?}");
    assert!(!dir.exists());
}

/// #8663: an unreadable subdirectory keeps a disowned directory whole — the
/// refusal comes from the content check, before anything is deleted.
#[test]
fn unclaimed_directory_is_kept_when_the_content_check_fails() {
    let tmp = crate::test_support::hermetic_temp_dir();
    let dir = leftover(tmp.path());
    write(&dir, ".claude/settings.json");
    let _restore = deny_all(&dir.join(".claude"));

    for policy in [DirtyWorktreePolicy::Skip, DirtyWorktreePolicy::ForceDiscard] {
        let outcome = remove_session_worktree(&dir, "test", policy);
        let reason = outcome.reason().expect("kept").to_string();
        assert!(reason.contains("content check failed"), "{reason}");
        assert!(dir.join(WORKTREE_SENTINEL_FILE).exists(), "nothing deleted");
    }
}

/// #8663: `prune-worktrees --discard-dirty` still removes a disowned
/// directory with content; the operator asked to lose it.
#[test]
fn force_discard_removes_an_unclaimed_directory_with_results() {
    let tmp = crate::test_support::hermetic_temp_dir();
    let dir = leftover(tmp.path());
    write(&dir, "results/x");

    let outcome = remove_session_worktree(&dir, "test", DirtyWorktreePolicy::ForceDiscard);
    assert!(outcome.removed(), "{outcome:?}");
    assert!(!dir.exists());
}

/// #8663: the non-git classifier excuses harness files, tm's scaffold and
/// build output, and names the first kept entry.
#[test]
fn unversioned_content_keeps_run_output_and_excuses_harness_files() {
    let tmp = crate::test_support::hermetic_temp_dir();
    let dir = tmp.path().join("ws");
    for rel in [
        WORKTREE_SENTINEL_FILE,
        ".trusty-mpm/log",
        ".claude/settings.local.json",
        "dist/app.js",
        "__pycache__/m.pyc",
    ] {
        write(&dir, rel);
    }
    assert_eq!(kept_unversioned_content(&dir), Ok(None));

    write(&dir, "results/a.json");
    write(&dir, "results/b.json");
    let kept = kept_unversioned_content(&dir)
        .expect("readable")
        .expect("results are kept");
    assert_eq!((kept.files, kept.first.as_str()), (2, "results"));
}
