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
use crate::core::agent::DelegationId;
use crate::core::session::SessionId;
use crate::session_manager::decommission::WORKTREE_SENTINEL_FILE;
use crate::session_manager::worktree_git_fixture::{GitWorktreeFixture, deny_all};
use crate::session_manager::worktree_ownership::{
    AgentWorktreeOwner, sentinel_payload_bytes, write_agent_sentinel,
};
use crate::session_manager::worktree_ownership_location::{
    admin_sentinel_path, write_sentinel_bytes,
};

/// Mark `wt` as tm-created the way provisioning does since #8511: the marker
/// goes into the git admin dir, never the working tree.
fn mark_owned(wt: &Path) {
    write_sentinel_bytes(wt, b"").expect("write the admin-dir marker");
}

/// The admin-dir marker path of `wt`, which must resolve.
fn admin_marker(wt: &Path) -> PathBuf {
    admin_sentinel_path(wt).expect("the admin dir resolves")
}

/// A pushed worktree carrying the ownership marker and a tracked
/// `.gitignore`, then dirtied exactly as the issue's `git status` showed:
/// ` M .gitignore`, `?? .claude/settings.json`, `?? .claude/settings.json.bak`,
/// `?? CLAUDE.md`.
fn provisioned_tree(fx: &GitWorktreeFixture, name: &str) -> PathBuf {
    let wt = fx.add_worktree(name);
    std::fs::write(wt.join(".gitignore"), "target/\n").expect("write .gitignore");
    GitWorktreeFixture::commit_all_and_push(&wt, "track .gitignore");
    mark_owned(&wt);
    // #7660: the real provisioning write, so the excused diff is the real one.
    crate::core::scaffold_gitignore::ensure_scaffold_gitignored(&wt).expect("scaffold .gitignore");
    std::fs::create_dir_all(wt.join(".claude")).expect("mkdir .claude");
    std::fs::write(wt.join(".claude/settings.json"), "{}\n").expect("write settings");
    std::fs::write(wt.join(".claude/settings.json.bak"), "{}\n").expect("write settings bak");
    std::fs::write(wt.join("CLAUDE.md"), "# tm\n").expect("write CLAUDE.md");
    wt
}

/// A `.claude/settings.json` snapshot named the way `snapshot_then_prune`
/// names one (#8688).
const SETTINGS_SNAPSHOT: &str = ".claude/settings.json.20260926T140608Z.bak";

/// [`provisioned_tree`] plus the two files a task-bearing spawn adds (#8688):
/// `TASK.md` and a timestamped `.claude/settings.json` snapshot.
fn task_bearing_tree(fx: &GitWorktreeFixture, name: &str) -> PathBuf {
    let wt = provisioned_tree(fx, name);
    std::fs::write(wt.join("TASK.md"), "Fix the bug\n").expect("write TASK.md");
    std::fs::write(wt.join(SETTINGS_SNAPSHOT), "{}\n").expect("write the snapshot");
    wt
}

/// The file count a kept reason states and the entries it lists (#8688).
fn count_and_list(reason: &str) -> (usize, Vec<String>) {
    let (head, _) = reason
        .split_once(" uncommitted/untracked file(s)")
        .expect("the reason states a file count");
    let count = head
        .rsplit(|c: char| !c.is_ascii_digit())
        .next()
        .and_then(|n| n.parse().ok())
        .expect("a numeric count");
    let listed = reason
        .split_once("unpushed commit(s): ")
        .and_then(|(_, rest)| rest.split_once(')'))
        .map(|(list, _)| list.split(", ").map(str::to_string).collect())
        .unwrap_or_default();
    (count, listed)
}

async fn remove(wt: &Path, policy: ProvisioningDirt) -> WorkspaceVerdict {
    remove_as(&ManagedSessionId::new(), wt, policy).await
}

/// [`remove`] as the session `id`, for the tests whose marker names an owner.
async fn remove_as(id: &ManagedSessionId, wt: &Path, policy: ProvisioningDirt) -> WorkspaceVerdict {
    remove_in_project_worktree(id, wt, policy).await
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

/// #7660 interim ruling (#8540): the plain refusal recommends `--force`, which
/// excuses an untracked `CLAUDE.md` / `.claude/settings.json` by path alone,
/// so it must name them and say `--force` discards edits to them. Fails
/// before the ruling, whose refusal carried no such warning.
#[tokio::test]
async fn decommission_refusal_warns_force_discards_untracked_claude_md_edits() {
    let fx = GitWorktreeFixture::new();
    let wt = provisioned_tree(&fx, "decom-refuse-warn-7660");

    let verdict = remove(&wt, ProvisioningDirt::Refuse).await;

    assert!(!verdict.removed && wt.exists());
    let reason = verdict.kept_reason.expect("a kept workspace must say why");
    assert!(
        reason.ends_with(
            "unpushed commits. WARNING: --force deletes the untracked .claude/settings.json \
             and CLAUDE.md with the worktree, including any edits made to them; copy out \
             anything you added there first"
        ),
        "reason: {reason}"
    );
}

/// #7660: one untracked content-unchecked file is named alone, with a
/// singular pronoun — never a list with a dangling comma.
#[tokio::test]
async fn decommission_refusal_warning_names_a_single_untracked_file() {
    let fx = GitWorktreeFixture::new();
    let wt = provisioned_tree(&fx, "decom-refuse-warn-one-7660");
    std::fs::remove_file(wt.join(".claude/settings.json")).expect("drop settings.json");

    let verdict = remove(&wt, ProvisioningDirt::Refuse).await;

    assert!(!verdict.removed && wt.exists());
    let reason = verdict.kept_reason.expect("a kept workspace must say why");
    assert!(
        reason.ends_with(
            "unpushed commits. WARNING: --force deletes the untracked CLAUDE.md with the \
             worktree, including any edits made to it; copy out anything you added there first"
        ),
        "reason: {reason}"
    );
}

/// #7660 interim ruling (#8540): with no untracked `CLAUDE.md` or
/// `.claude/settings.json` in the tree, the refusal carries no warning.
#[tokio::test]
async fn decommission_refusal_omits_the_warning_without_untracked_claude_files() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("decom-refuse-no-warn-7660");
    mark_owned(&wt);
    std::fs::write(wt.join("notes.rs"), "// unsaved\n").expect("write user work");

    let verdict = remove(&wt, ProvisioningDirt::Refuse).await;

    assert!(!verdict.removed && wt.join("notes.rs").exists());
    let reason = verdict.kept_reason.expect("a kept workspace must say why");
    assert!(reason.contains("?? notes.rs"), "reason: {reason}");
    assert!(!reason.contains("WARNING"), "reason: {reason}");
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

/// #8688: `TASK.md` and a timestamped settings snapshot are excused only
/// untracked, at their exact paths, in the snapshot's exact name shape.
#[test]
fn provisioning_entry_excuses_task_md_and_settings_snapshots_only_when_untracked() {
    let fx = GitWorktreeFixture::new();
    let wt = task_bearing_tree(&fx, "decom-entry-8688");
    let is = |line: &str| is_provisioning_entry(&wt, line);
    assert!(is("?? TASK.md"));
    assert!(is(&format!("?? {SETTINGS_SNAPSHOT}")));
    assert!(is("?? .claude/settings.json.20260926T140608Z-1.bak"));
    assert!(!is(" M TASK.md"), "an edit to a tracked TASK.md is work");
    assert!(!is("?? docs/TASK.md"));
    assert!(!is("?? .claude/settings.json.mine.bak"));
    assert!(!is("?? .claude/settings.local.json.20260926T140608Z.bak"));
    assert!(!is("?? settings.json.20260926T140608Z.bak"));
    assert!(!is("?? .claude/sub/settings.json.20260926T140608Z.bak"));
    assert!(!is(&format!(" M {SETTINGS_SNAPSHOT}")));
}

/// #8688: the live report — a task-bearing managed worktree holding only
/// tm-written files is removed by `--force`. Fails before the fix, which
/// kept it for `?? TASK.md` and `?? .claude/settings.json.<ts>.bak`.
#[tokio::test]
async fn force_decommission_removes_a_worktree_holding_task_md_and_a_settings_snapshot() {
    let fx = GitWorktreeFixture::new();
    let wt = task_bearing_tree(&fx, "decom-force-task-8688");

    let verdict = remove(&wt, ProvisioningDirt::Discard).await;

    assert!(verdict.removed, "kept: {:?}", verdict.kept_reason);
    assert!(!wt.exists(), "the workspace directory must be gone");
}

/// #8688: user work beside the tm-written files still blocks `--force`, and
/// the reason counts and names only that work.
#[tokio::test]
async fn force_decommission_keeps_user_work_beside_task_md() {
    let fx = GitWorktreeFixture::new();
    let wt = task_bearing_tree(&fx, "decom-force-task-work-8688");
    std::fs::write(wt.join("notes.rs"), "// unsaved\n").expect("write user work");

    let verdict = remove(&wt, ProvisioningDirt::Discard).await;

    assert!(!verdict.removed && wt.join("notes.rs").exists());
    assert!(wt.join("TASK.md").exists(), "nothing is removed");
    let reason = verdict.kept_reason.expect("reason");
    assert_eq!(
        count_and_list(&reason),
        (1, vec!["?? notes.rs".to_string()]),
        "reason: {reason}"
    );
}

/// #8688: the plain refusal's count and its list agree. Fails before the
/// fix, which counted the untracked `.claude/` as one entry and listed each
/// of its three files.
#[tokio::test]
async fn decommission_refusal_count_matches_the_entries_it_lists() {
    let fx = GitWorktreeFixture::new();
    let wt = task_bearing_tree(&fx, "decom-refuse-count-8688");

    let verdict = remove(&wt, ProvisioningDirt::Refuse).await;

    assert!(!verdict.removed && wt.exists());
    let reason = verdict.kept_reason.expect("a kept workspace must say why");
    let (count, listed) = count_and_list(&reason);
    assert_eq!(count, listed.len(), "reason: {reason}");
    assert_eq!(count, 6, "reason: {reason}");
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
/// or not. #7660 critic round 3: the index is denied, not `.git`, so the
/// ownership proof passes and the dirt check's own failure arm keeps the tree.
#[tokio::test]
async fn force_decommission_removes_nothing_when_the_dirty_check_cannot_complete() {
    let fx = GitWorktreeFixture::new();
    let wt = provisioned_tree(&fx, "decom-force-unreadable-7660");
    assert_eq!(
        force_blocker(&wt, &ManagedSessionId::new()),
        None,
        "premise: the blocker passes"
    );
    let index = fx
        .repo
        .join(".git/worktrees/decom-force-unreadable-7660/index");
    let verdict = {
        let _restore = deny_all(&index);
        remove(&wt, ProvisioningDirt::Discard).await
    };

    assert!(!verdict.removed, "an unanswered check must keep the tree");
    assert!(wt.join("CLAUDE.md").exists());
    let reason = verdict.kept_reason.expect("a kept tree must say why");
    assert!(reason.contains("dirty-check failed"), "reason: {reason}");
    assert!(!reason.contains("--force declined"), "reason: {reason}");
}

/// 🔴 #7660 critic round 3 (HIGH): since #8511 a tm-created worktree carries
/// its marker only in the git admin dir. `--force` honours it. Fails before
/// the fix, which read only the legacy in-tree marker and refused the tree.
#[tokio::test]
async fn force_decommission_honours_an_admin_dir_marker() {
    let fx = GitWorktreeFixture::new();
    let wt = provisioned_tree(&fx, "decom-admin-marker-7660");
    assert!(
        admin_marker(&wt).is_file(),
        "premise: the admin marker exists"
    );
    assert!(
        !wt.join(WORKTREE_SENTINEL_FILE).exists(),
        "premise: no legacy marker"
    );

    let verdict = remove(&wt, ProvisioningDirt::Discard).await;

    assert!(verdict.removed, "kept: {:?}", verdict.kept_reason);
    assert!(!wt.exists());
}

/// #7660 critic round 3: a pre-#8511 tree whose marker still sits in the
/// working tree is honoured too.
#[tokio::test]
async fn force_decommission_honours_a_legacy_in_tree_marker() {
    let fx = GitWorktreeFixture::new();
    let wt = provisioned_tree(&fx, "decom-legacy-marker-7660");
    std::fs::remove_file(admin_marker(&wt)).expect("drop the admin marker");
    std::fs::write(wt.join(WORKTREE_SENTINEL_FILE), b"").expect("write legacy marker");

    let verdict = remove(&wt, ProvisioningDirt::Discard).await;

    assert!(verdict.removed, "kept: {:?}", verdict.kept_reason);
    assert!(!wt.exists());
}

/// 🔴 #7660 critic round 3: the marker reader follows symlinks, so a marker
/// that is a symlink is no proof of ownership. Fails before the fix, which
/// never looked at the admin marker and named no symlink.
#[tokio::test]
async fn force_decommission_keeps_a_worktree_whose_marker_is_a_symlink() {
    let fx = GitWorktreeFixture::new();
    let wt = provisioned_tree(&fx, "decom-marker-symlink-7660");
    let marker = admin_marker(&wt);
    let target = fx.repo.join("elsewhere-7660");
    std::fs::write(&target, b"").expect("write symlink target");
    std::fs::remove_file(&marker).expect("drop the admin marker");
    std::os::unix::fs::symlink(&target, &marker).expect("symlink the marker");

    let reason = kept_under_force(&wt).await;

    assert!(reason.contains("not a regular file"), "reason: {reason}");
}

/// 🔴 #7660 critic round 3: on a clean tree `--force` is never stricter than
/// a plain decommission. A clean `.worktrees/` tree with no marker is removed
/// either way. Fails before the fix, where `--force` demanded the marker even
/// when there was no dirt to excuse.
#[tokio::test]
async fn force_decommission_is_never_stricter_than_plain_on_a_clean_tree() {
    let fx = GitWorktreeFixture::new();
    for (name, policy) in [
        ("decom-clean-plain-7660", ProvisioningDirt::Refuse),
        ("decom-clean-force-7660", ProvisioningDirt::Discard),
    ] {
        let wt = fx.add_worktree(name);

        let verdict = remove(&wt, policy).await;

        assert!(
            verdict.removed,
            "{policy:?} kept: {:?}",
            verdict.kept_reason
        );
        assert!(!wt.exists(), "{policy:?}: the tree must be gone");
    }
}

/// A worktree whose repository has NO `.gitignore`, provisioned so tm's
/// scaffolding creates one: `?? .gitignore` holding only the managed block.
fn untracked_gitignore_tree(fx: &GitWorktreeFixture, name: &str) -> PathBuf {
    let wt = fx.add_worktree(name);
    mark_owned(&wt);
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
    std::fs::remove_file(admin_marker(&wt)).expect("drop the marker");

    let reason = kept_under_force(&wt).await;

    assert!(reason.contains("no ownership sentinel"), "reason: {reason}");
}

/// 🔴 #7660 critic round 3 MEDIUM-1: a marker naming ANOTHER session is no
/// licence for `--force`. Records A and B can name one worktree while B is
/// still `Provisioning`, which the foreign-`Active` claim check misses. Fails
/// before the fix, whose ownership check accepted any marker and removed it.
#[tokio::test]
async fn force_decommission_keeps_a_worktree_another_session_owns() {
    let fx = GitWorktreeFixture::new();
    let wt = provisioned_tree(&fx, "decom-foreign-owner-7660");
    let other = ManagedSessionId::new();
    write_sentinel_bytes(&wt, &sentinel_payload_bytes(other)).expect("write the owner marker");

    let verdict = remove_as(&ManagedSessionId::new(), &wt, ProvisioningDirt::Discard).await;

    assert!(
        !verdict.removed && wt.exists(),
        "another session's tree must stay"
    );
    let reason = verdict.kept_reason.expect("a kept tree must say why");
    assert!(reason.contains("--force declined"), "reason: {reason}");
    assert!(reason.contains(&other.to_string()), "reason: {reason}");
}

/// #7660 critic round 3 MEDIUM-1: a marker naming a dispatched agent keeps the
/// tree under `--force`, and the reason names the agent.
#[tokio::test]
async fn force_decommission_keeps_a_worktree_an_agent_owns() {
    let fx = GitWorktreeFixture::new();
    let wt = provisioned_tree(&fx, "decom-agent-owner-7660");
    let agent = AgentWorktreeOwner {
        agent_id: "agent-synthetic-7660".to_string(),
        delegation_id: DelegationId::new(),
        parent_session_id: SessionId::new(),
    };
    write_agent_sentinel(&wt, agent).expect("write the agent marker");

    let reason = kept_under_force(&wt).await;

    assert!(reason.contains("--force declined"), "reason: {reason}");
    assert!(reason.contains("agent-synthetic-7660"), "reason: {reason}");
}

/// #7660 critic round 3 MEDIUM-1: a marker naming the decommissioned session
/// itself is the proof `--force` needs, so the provisioning-dirty tree goes.
#[tokio::test]
async fn force_decommission_removes_a_worktree_its_own_session_owns() {
    let fx = GitWorktreeFixture::new();
    let wt = provisioned_tree(&fx, "decom-own-owner-7660");
    let me = ManagedSessionId::new();
    write_sentinel_bytes(&wt, &sentinel_payload_bytes(me)).expect("write the owner marker");

    let verdict = remove_as(&me, &wt, ProvisioningDirt::Discard).await;

    assert!(verdict.removed, "kept: {:?}", verdict.kept_reason);
    assert!(!wt.exists());
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
    mark_owned(&main);
    std::fs::write(main.join("CLAUDE.md"), "# tm\n").expect("write CLAUDE.md");

    let reason = kept_under_force(&main).await;

    assert!(reason.contains("main checkout"), "reason: {reason}");
    assert!(main.join("CLAUDE.md").exists());
}

/// #7660 round 2, FAIL-CLOSED: a worktree git cannot resolve (a `.git` file
/// naming a missing admin dir) is kept, naming the failed proof. Since #8511
/// the marker itself lives behind that `.git`, so the marker read fails first.
#[tokio::test]
async fn force_decommission_keeps_a_worktree_git_cannot_resolve() {
    let fx = GitWorktreeFixture::new();
    let wt = provisioned_tree(&fx, "decom-unresolvable-7660");
    std::fs::write(wt.join(".git"), "gitdir: /nonexistent/7660\n").expect("break .git");

    let reason = kept_under_force(&wt).await;

    assert!(
        reason.contains("cannot prove tm created it"),
        "reason: {reason}"
    );
    assert!(reason.contains("does not resolve"), "reason: {reason}");
}

/// #7660 critic round 3, FAIL-CLOSED: a marker that is present but does not
/// parse is no proof.
#[tokio::test]
async fn force_decommission_keeps_a_worktree_whose_marker_is_corrupt() {
    let fx = GitWorktreeFixture::new();
    let wt = provisioned_tree(&fx, "decom-marker-corrupt-7660");
    std::fs::write(admin_marker(&wt), b"not a marker").expect("corrupt the marker");

    let reason = kept_under_force(&wt).await;

    assert!(reason.contains("corrupt"), "reason: {reason}");
}

/// #7660 round 2: a `git worktree lock` keeps the tree, and the reason says
/// it is locked. #7660 round 3: the refusal must be tm's own — git's "cannot
/// remove a locked working tree" also names the lock, so without the
/// `--force declined` check this passed with the lock probe removed.
#[tokio::test]
async fn force_decommission_keeps_a_locked_worktree() {
    let fx = GitWorktreeFixture::new();
    let wt = provisioned_tree(&fx, "decom-locked-7660");
    fx.lock_worktree(&wt);

    let reason = kept_under_force(&wt).await;

    assert!(reason.contains("--force declined"), "reason: {reason}");
    assert!(reason.contains("locked"), "reason: {reason}");
}

/// #7660 round 3, FAIL-CLOSED: a plain directory under `.worktrees/` that
/// carries a legacy marker is not a worktree root — git answers for the
/// enclosing main checkout — so `--force` has no proof to act on. Fails on the
/// 1.7.2 fix, which ran no provenance check at all.
#[test]
fn force_blocker_refuses_a_directory_that_is_not_a_worktree_root() {
    let fx = GitWorktreeFixture::new();
    let plain = fx.repo.join(".worktrees").join("plain-7660");
    std::fs::create_dir_all(&plain).expect("mkdir plain");
    std::fs::write(plain.join(WORKTREE_SENTINEL_FILE), b"").expect("write legacy marker");

    let blocker =
        force_blocker(&plain, &ManagedSessionId::new()).expect("a non-root directory must block");

    assert!(blocker.contains("not a worktree root"), "{blocker}");
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

/// #7660 critic round 3: the by-design reason names the other two kinds of
/// workspace tm never removes — a worktree tm did not create (`.git` is a
/// file) and a plain local-path or adopted directory (no `.git`).
#[test]
fn unowned_kept_reason_names_a_foreign_worktree_and_a_plain_directory() {
    let fx = GitWorktreeFixture::new();
    let foreign = fx.add_worktree("foreign-7660");
    let plain = tempfile::tempdir().expect("tempdir");

    for (ws, kind) in [
        (foreign.as_path(), "a git worktree tm did not create"),
        (plain.path(), "a local-path or adopted directory"),
    ] {
        let reason = unowned_kept_reason(ws, ProvisioningDirt::Refuse).expect("kept");
        assert!(reason.contains(kind), "{reason}");
        assert!(reason.starts_with("kept by design"), "{reason}");
    }
}
