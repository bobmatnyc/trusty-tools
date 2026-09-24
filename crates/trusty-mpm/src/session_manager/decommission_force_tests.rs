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
    // #7660: the real provisioning write, so the excused diff is the real one.
    crate::core::scaffold_gitignore::ensure_scaffold_gitignored(&wt).expect("scaffold .gitignore");
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
    let fx = GitWorktreeFixture::new();
    let wt = provisioned_tree(&fx, "decom-entry-7660");
    let is = |line: &str| is_provisioning_entry(&wt, line);
    assert!(is(" M .gitignore"));
    assert!(is("?? .claude/settings.json"));
    assert!(is("?? .claude/settings.json.bak"));
    assert!(is("?? CLAUDE.md"));
    assert!(!is("M  .gitignore"), "a staged change is not provisioning");
    assert!(!is(" D CLAUDE.md"), "a deletion is not provisioning");
    assert!(
        !is(" M CLAUDE.md"),
        "an edit to a tracked CLAUDE.md is not provisioning"
    );
    assert!(
        !is(" M .claude/settings.json"),
        "an edit to a tracked settings file is not provisioning"
    );
    assert!(!is("?? .claude/"), "a collapsed directory hides its files");
    assert!(!is("?? src/main.rs"));
}

/// 🔴 #7660 critic HIGH-2: a repository that tracks `CLAUDE.md` shows an
/// agent's edit as ` M CLAUDE.md`, and `--force` must keep that tree. Fails
/// before the fix, which excused any ` M` on the four paths and removed it.
#[tokio::test]
async fn force_decommission_keeps_an_edited_tracked_claude_md() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("decom-force-claude-md-7660");
    std::fs::write(wt.join("CLAUDE.md"), "# project rules\n").expect("write CLAUDE.md");
    GitWorktreeFixture::commit_all_and_push(&wt, "track CLAUDE.md");
    std::fs::write(wt.join("CLAUDE.md"), "# project rules\n- a new rule\n")
        .expect("edit CLAUDE.md");

    let verdict = remove(&wt, ProvisioningDirt::Discard).await;

    assert!(
        !verdict.removed,
        "an edited tracked CLAUDE.md must keep the tree"
    );
    assert_eq!(
        std::fs::read_to_string(wt.join("CLAUDE.md")).expect("read CLAUDE.md"),
        "# project rules\n- a new rule\n",
        "the edit must survive"
    );
    assert!(verdict.kept_reason.is_some());
}

/// 🔴 #7660 critic HIGH-2: a `.gitignore` diff carrying one line provisioning
/// never writes keeps the tree under `--force`. Fails before the fix, which
/// excused ` M .gitignore` whatever its diff held.
#[tokio::test]
async fn force_decommission_keeps_a_gitignore_with_a_non_provisioning_line() {
    let fx = GitWorktreeFixture::new();
    let wt = provisioned_tree(&fx, "decom-force-gitignore-7660");
    let mut body = std::fs::read_to_string(wt.join(".gitignore")).expect("read .gitignore");
    body.push_str("secrets.env\n");
    std::fs::write(wt.join(".gitignore"), &body).expect("append a user line");

    let verdict = remove(&wt, ProvisioningDirt::Discard).await;

    assert!(
        !verdict.removed,
        "a user's .gitignore line must keep the tree"
    );
    assert!(
        std::fs::read_to_string(wt.join(".gitignore"))
            .expect("read .gitignore")
            .contains("secrets.env"),
        "the user's line must survive"
    );
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
    // #7660 round 2: the blocker is named, and the excused files are not.
    assert!(reason.contains("?? notes.rs"), "reason: {reason}");
    assert!(!reason.contains("?? CLAUDE.md"), "reason: {reason}");
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

/// A worktree whose repository has NO `.gitignore`, provisioned so tm's
/// scaffolding creates one: `?? .gitignore` holding only the managed block.
fn untracked_gitignore_tree(fx: &GitWorktreeFixture, name: &str) -> PathBuf {
    let wt = fx.add_worktree(name);
    std::fs::write(wt.join(WORKTREE_SENTINEL_FILE), b"").expect("write sentinel");
    crate::core::scaffold_gitignore::ensure_scaffold_gitignored(&wt).expect("scaffold .gitignore");
    std::fs::create_dir_all(wt.join(".claude")).expect("mkdir .claude");
    std::fs::write(wt.join(".claude/settings.json"), "{}\n").expect("write settings");
    std::fs::write(wt.join("CLAUDE.md"), "# tm\n").expect("write CLAUDE.md");
    wt
}

/// #7660 round 2: an untracked `.gitignore` holding only tm's block is
/// provisioning, so `--force` removes the tree.
#[tokio::test]
async fn force_decommission_removes_a_tree_with_an_untracked_scaffold_gitignore() {
    let fx = GitWorktreeFixture::new();
    let wt = untracked_gitignore_tree(&fx, "decom-untracked-gi-7660");

    let verdict = remove(&wt, ProvisioningDirt::Discard).await;

    assert!(verdict.removed, "kept: {:?}", verdict.kept_reason);
    assert!(!wt.exists());
}

/// 🔴 #7660 round 2: an untracked `.gitignore` carrying one line tm never
/// writes keeps the tree. Fails if the untracked case is excused by path.
#[tokio::test]
async fn force_decommission_keeps_an_untracked_gitignore_with_a_user_line() {
    let fx = GitWorktreeFixture::new();
    let wt = untracked_gitignore_tree(&fx, "decom-untracked-gi-user-7660");
    let mut body = std::fs::read_to_string(wt.join(".gitignore")).expect("read .gitignore");
    body.push_str("secrets.env\n");
    std::fs::write(wt.join(".gitignore"), &body).expect("append a user line");

    let verdict = remove(&wt, ProvisioningDirt::Discard).await;

    assert!(
        !verdict.removed,
        "a user's .gitignore line must keep the tree"
    );
    assert!(wt.join(".gitignore").exists());
}

/// 🔴 #7660 round 2: FAIL-CLOSED — when the `.gitignore` diff cannot be read
/// (a corrupt index), ` M .gitignore` is not excused and the tree is kept.
/// Fails if an unreadable diff is treated as provisioning.
#[tokio::test]
async fn force_decommission_keeps_the_tree_when_the_gitignore_diff_cannot_be_read() {
    let fx = GitWorktreeFixture::new();
    let wt = provisioned_tree(&fx, "decom-corrupt-index-7660");
    let index = fx
        .repo
        .join(".git/worktrees/decom-corrupt-index-7660/index");
    assert!(index.exists(), "premise: the worktree's index lives here");
    std::fs::write(&index, b"not an index").expect("corrupt the index");

    assert!(
        !is_provisioning_entry(&wt, " M .gitignore"),
        "an unreadable diff must never excuse .gitignore"
    );
    let verdict = remove(&wt, ProvisioningDirt::Discard).await;
    assert!(!verdict.removed && wt.exists(), "the tree must be kept");
    // #7660 round 2: the status read failure itself is what kept it.
    let reason = verdict.kept_reason.expect("a kept tree must say why");
    assert!(reason.contains("dirty-check failed"), "reason: {reason}");
}

/// 🔴 #7660 round 2: inside a hunk, the added line `++ x` shows as `+++ x`;
/// it is a body line, not a header, and it is not provisioning. Fails before
/// the fix, which skipped every `+++ ` line as a header.
#[test]
fn gitignore_body_line_shaped_like_a_header_is_not_excused() {
    let fx = GitWorktreeFixture::new();
    let wt = provisioned_tree(&fx, "decom-header-shaped-7660");
    assert!(
        is_provisioning_entry(&wt, " M .gitignore"),
        "premise: the scaffold block alone is excused"
    );
    let mut body = std::fs::read_to_string(wt.join(".gitignore")).expect("read .gitignore");
    body.push_str("++ secrets.env\n");
    std::fs::write(wt.join(".gitignore"), &body).expect("append a header-shaped line");

    assert!(!is_provisioning_entry(&wt, " M .gitignore"));
}

/// The kept reason, asserting the tree stayed on disk.
async fn kept_under_force(wt: &Path) -> String {
    let verdict = remove(wt, ProvisioningDirt::Discard).await;
    assert!(!verdict.removed, "the tree must be kept");
    assert!(wt.exists(), "the tree must still be on disk");
    verdict.kept_reason.expect("a kept tree must say why")
}

/// #7660 round 2: `--force` needs proof tm created the tree. A provisioning-
/// dirty worktree with no ownership sentinel is kept. Fails on the 1.7.2 fix,
/// which excused the dirt and removed it.
#[tokio::test]
async fn force_decommission_keeps_a_worktree_without_the_sentinel() {
    let fx = GitWorktreeFixture::new();
    let wt = provisioned_tree(&fx, "decom-no-sentinel-7660");
    std::fs::remove_file(wt.join(WORKTREE_SENTINEL_FILE)).expect("drop sentinel");

    let reason = kept_under_force(&wt).await;

    assert!(reason.contains("no ownership sentinel"), "reason: {reason}");
}

/// #7660 round 2, FAIL-CLOSED: a sentinel whose existence cannot be read is
/// no proof. Fails on the 1.7.2 fix, whose reason named no sentinel.
#[tokio::test]
async fn force_decommission_keeps_a_worktree_whose_sentinel_cannot_be_read() {
    let fx = GitWorktreeFixture::new();
    let wt = provisioned_tree(&fx, "decom-sentinel-unreadable-7660");

    let reason = {
        let _restore = deny_all(&wt);
        kept_under_force(&wt).await
    };

    assert!(
        reason.contains("sentinel could not be read"),
        "reason: {reason}"
    );
}

/// 🔴 #7660 round 2: a repository's MAIN checkout parked under `.worktrees/`,
/// sentinel and provisioning dirt included, is never removed by `--force`.
/// Fails on the 1.7.2 fix, which never asked what kind of checkout it was.
#[tokio::test]
async fn force_decommission_keeps_a_main_checkout_under_the_worktrees_dir() {
    let fx = GitWorktreeFixture::new();
    let main = fx.repo.join(".worktrees").join("standalone-7660");
    std::fs::create_dir_all(&main).expect("mkdir standalone");
    let init = std::process::Command::new("git")
        .arg("-C")
        .arg(&main)
        .args(["init", "--initial-branch=main"])
        .output()
        .expect("run git init");
    assert!(init.status.success(), "git init failed");
    std::fs::write(main.join(WORKTREE_SENTINEL_FILE), b"").expect("write sentinel");
    std::fs::write(main.join("CLAUDE.md"), "# tm\n").expect("write CLAUDE.md");

    let reason = kept_under_force(&main).await;

    assert!(reason.contains("main checkout"), "reason: {reason}");
    assert!(main.join("CLAUDE.md").exists());
}

/// #7660 round 2, FAIL-CLOSED: a worktree git cannot resolve (a `.git` file
/// naming a missing admin dir) is kept, naming the failed proof.
#[tokio::test]
async fn force_decommission_keeps_a_worktree_git_cannot_resolve() {
    let fx = GitWorktreeFixture::new();
    let wt = provisioned_tree(&fx, "decom-unresolvable-7660");
    std::fs::write(wt.join(".git"), "gitdir: /nonexistent/7660\n").expect("break .git");

    let reason = kept_under_force(&wt).await;

    assert!(
        reason.contains("cannot prove it is a tm-created linked worktree"),
        "reason: {reason}"
    );
}

/// #7660 round 2: a `git worktree lock` keeps the tree, and the reason says
/// it is locked.
#[tokio::test]
async fn force_decommission_keeps_a_locked_worktree() {
    let fx = GitWorktreeFixture::new();
    let wt = provisioned_tree(&fx, "decom-locked-7660");
    fx.lock_worktree(&wt);

    let reason = kept_under_force(&wt).await;

    assert!(reason.contains("locked"), "reason: {reason}");
}

/// #7660 round 2, FAIL-CLOSED: a lock state that cannot be read (the git dir
/// is unsearchable) is reported as a blocker, never as "unlocked".
#[test]
fn lock_blocker_fails_closed_when_the_lock_state_cannot_be_read() {
    let dir = tempfile::tempdir().expect("tempdir");
    let git_dir = dir.path().join("worktrees").join("wt");
    std::fs::create_dir_all(&git_dir).expect("mkdir git dir");
    assert_eq!(lock_blocker(&git_dir), None, "premise: no lock file");

    let blocker = {
        let _restore = deny_all(&git_dir);
        lock_blocker(&git_dir)
    };

    let blocker = blocker.expect("an unreadable lock state must block");
    assert!(
        blocker.contains("lock state could not be read"),
        "{blocker}"
    );
}

/// #7660 round 2: a worktree already gone is neither removed nor kept, so
/// the CLI does not report a refusal for it.
#[tokio::test]
async fn decommission_of_an_absent_worktree_reports_nothing_kept() {
    let fx = GitWorktreeFixture::new();
    let absent = fx.repo.join(".worktrees").join("gone-7660");

    for policy in [ProvisioningDirt::Refuse, ProvisioningDirt::Discard] {
        let verdict = remove(&absent, policy).await;
        assert!(verdict.kept_reason.is_none(), "{:?}", verdict.kept_reason);
    }
}

/// #7660 round 2: the by-design reason names a main checkout, adds the
/// `--force` note only under `--force`, and is absent when nothing is on disk.
#[test]
fn unowned_kept_reason_names_a_main_checkout() {
    let fx = GitWorktreeFixture::new();

    let plain = unowned_kept_reason(&fx.repo, ProvisioningDirt::Refuse).expect("kept");
    assert!(plain.contains("main checkout"), "{plain}");
    assert!(!plain.contains("--force"), "{plain}");
    assert!(!plain.contains('\n'), "one line: {plain}");

    let forced = unowned_kept_reason(&fx.repo, ProvisioningDirt::Discard).expect("kept");
    assert!(forced.contains("--force does not apply"), "{forced}");

    let absent = fx.repo.join("no-such-dir-7660");
    assert_eq!(
        unowned_kept_reason(&absent, ProvisioningDirt::Discard),
        None
    );
}
