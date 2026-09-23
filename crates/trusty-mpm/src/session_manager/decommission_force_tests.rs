//! Tests for #7660: decommission's `--force` excuse and kept-workspace reason.
//!
//! Why: `--force` narrows a data-safety gate, so every arm that must still keep
//! the workspace — user work, unpushed commits, a check that cannot run — needs
//! its own test against a real git worktree.
//! What: [`remove_in_project_worktree`] on [`GitWorktreeFixture`] trees dirtied
//! the way `tm session start` provisioning dirties them.
//! Test: this file IS the test module.

use std::path::{Path, PathBuf};

use super::*;
use crate::session_manager::decommission::WORKTREE_SENTINEL_FILE;
use crate::session_manager::worktree_git_fixture::{GitWorktreeFixture, deny_all};

/// A pushed worktree carrying the ownership sentinel and a tracked
/// `.gitignore`, then dirtied exactly as the issue's `git status` showed:
/// ` M .gitignore`, `?? .claude/settings.json`, `?? .claude/settings.json.bak`,
/// `?? CLAUDE.md`.
fn provisioned_tree(fx: &GitWorktreeFixture, name: &str) -> PathBuf {
    let wt = fx.add_worktree(name);
    std::fs::write(wt.join(".gitignore"), "target/\n").expect("write .gitignore");
    GitWorktreeFixture::commit_all_and_push(&wt, "track .gitignore");
    std::fs::write(wt.join(WORKTREE_SENTINEL_FILE), b"").expect("write sentinel");
    std::fs::write(wt.join(".gitignore"), "target/\n.trusty-mpm/\n").expect("modify .gitignore");
    std::fs::create_dir_all(wt.join(".claude")).expect("mkdir .claude");
    std::fs::write(wt.join(".claude/settings.json"), "{}\n").expect("write settings");
    std::fs::write(wt.join(".claude/settings.json.bak"), "{}\n").expect("write settings bak");
    std::fs::write(wt.join("CLAUDE.md"), "# tm\n").expect("write CLAUDE.md");
    wt
}

async fn remove(wt: &Path, policy: ProvisioningDirt) -> WorkspaceVerdict {
    remove_in_project_worktree(&ManagedSessionId::new(), wt, policy).await
}

#[test]
fn provisioning_entry_matches_only_the_four_paths_in_provisioning_states() {
    assert!(is_provisioning_entry(" M .gitignore"));
    assert!(is_provisioning_entry("?? .claude/settings.json"));
    assert!(is_provisioning_entry("?? .claude/settings.json.bak"));
    assert!(is_provisioning_entry("?? CLAUDE.md"));
    assert!(
        !is_provisioning_entry("M  .gitignore"),
        "a staged change is not provisioning"
    );
    assert!(
        !is_provisioning_entry(" D CLAUDE.md"),
        "a deletion is not provisioning"
    );
    assert!(
        !is_provisioning_entry("?? .claude/"),
        "a collapsed directory hides its files"
    );
    assert!(!is_provisioning_entry("?? src/main.rs"));
}

/// #7660 error arm: without `--force` the provisioned tree is kept, and the
/// reason names the blocker and the flag. Fails before #7660, which kept the
/// tree with no reason at all and let the CLI exit 0.
#[tokio::test]
async fn decommission_reports_why_it_kept_a_provisioned_worktree() {
    let fx = GitWorktreeFixture::new();
    let wt = provisioned_tree(&fx, "decom-refuse-7660");

    let verdict = remove(&wt, ProvisioningDirt::Refuse).await;

    assert!(!verdict.removed && wt.exists());
    let reason = verdict.kept_reason.expect("a kept workspace must say why");
    assert!(reason.contains("--force"), "reason: {reason}");
}

/// The reported case: `--force` removes a tree dirty only from provisioning.
#[tokio::test]
async fn force_decommission_removes_a_provisioning_only_worktree() {
    let fx = GitWorktreeFixture::new();
    let wt = provisioned_tree(&fx, "decom-force-7660");

    let verdict = remove(&wt, ProvisioningDirt::Discard).await;

    assert!(verdict.removed, "kept: {:?}", verdict.kept_reason);
    assert!(!wt.exists(), "the workspace directory must be gone");
}

/// `--force` never discards a file provisioning did not write.
#[tokio::test]
async fn force_decommission_still_refuses_user_work() {
    let fx = GitWorktreeFixture::new();
    let wt = provisioned_tree(&fx, "decom-force-work-7660");
    std::fs::write(wt.join("notes.rs"), "// unsaved\n").expect("write user work");

    let verdict = remove(&wt, ProvisioningDirt::Discard).await;

    assert!(!verdict.removed && wt.join("notes.rs").exists());
    let reason = verdict.kept_reason.expect("reason");
    assert!(reason.contains("never discards"), "reason: {reason}");
}

/// `--force` never discards a commit that is on no remote.
#[tokio::test]
async fn force_decommission_still_refuses_unpushed_commits() {
    let fx = GitWorktreeFixture::new();
    let wt = provisioned_tree(&fx, "decom-force-unpushed-7660");
    GitWorktreeFixture::commit_unpushed(&wt);

    let verdict = remove(&wt, ProvisioningDirt::Discard).await;

    assert!(!verdict.removed && wt.exists());
    assert!(verdict.kept_reason.is_some());
}

/// FAIL-CLOSED: a dirty check that cannot complete removes nothing, forced
/// or not.
#[tokio::test]
async fn force_decommission_removes_nothing_when_the_dirty_check_cannot_complete() {
    let fx = GitWorktreeFixture::new();
    let wt = provisioned_tree(&fx, "decom-force-unreadable-7660");
    let verdict = {
        let _restore = deny_all(&wt.join(".git"));
        remove(&wt, ProvisioningDirt::Discard).await
    };

    assert!(!verdict.removed, "an unanswered check must keep the tree");
    assert!(wt.join("CLAUDE.md").exists());
    assert!(verdict.kept_reason.is_some());
}
