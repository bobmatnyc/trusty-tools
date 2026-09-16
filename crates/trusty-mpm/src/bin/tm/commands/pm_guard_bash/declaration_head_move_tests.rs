//! #7905 review round 2: every HEAD-moving verb is gated on the declaration.
//!
//! Why: the rule's whole value is that it reads REAL git — a resolved ref, a
//! blob at that ref, a `diff-tree` of a replayed commit — so a mocked git would
//! test nothing. Every fixture here is a real repository with real commits.
//! Test: this file.

use super::*;

/// Run `git -C <dir> <args>`, panicking with git's own stderr on failure.
fn git_ok(dir: &Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("fixture: `git {}` could not be run: {e}", args.join(" ")));
    assert!(
        out.status.success(),
        "fixture: `git {}` failed in {}: {}",
        args.join(" "),
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A real main checkout on `main`, with one seed commit.
fn repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    git_ok(dir.path(), &["init", "--initial-branch=main"]);
    git_ok(dir.path(), &["config", "user.email", "t@example.com"]);
    git_ok(dir.path(), &["config", "user.name", "t"]);
    std::fs::write(dir.path().join("README.md"), "# t\n").expect("seed");
    git_ok(dir.path(), &["add", "README.md"]);
    git_ok(dir.path(), &["commit", "-m", "seed"]);
    dir
}

/// Commit `body` as the declaration on a side branch, and return to `main`.
///
/// Why: the reported attack shape — the declaring commit exists on a ref the
/// session can name, and never passed this checkout's commit gate.
fn declaring_branch(dir: &Path, name: &str, body: &str) {
    git_ok(dir, &["checkout", "-q", "-b", name]);
    std::fs::write(dir.join(".trusty-mpm.toml"), body).expect("write");
    git_ok(dir, &["add", ".trusty-mpm.toml"]);
    git_ok(dir, &["commit", "-m", "declare"]);
    git_ok(dir, &["checkout", "-q", "main"]);
}

/// The rule's answer for `command` run in `dir`.
fn verdict(dir: &Path, command: &str) -> Option<String> {
    evaluate_declaration_head_move_in(command, dir, &PathEnv::from_process())
}

fn assert_denied(dir: &Path, command: &str) {
    let reason = verdict(dir, command)
        .unwrap_or_else(|| panic!("a HEAD move that changes the declaration must deny: {command}"));
    assert!(reason.contains("ADR-0044"), "{reason}");
}

fn assert_allowed(dir: &Path, command: &str) {
    assert_eq!(
        verdict(dir, command),
        None,
        "this command cannot change the declaration: {command}"
    );
}

/// 🔴 The reported defect verbatim: a solo session merging a declaring ref.
///
/// Why: ADR-0048 decision 10 covers `merge`, but denies only when the daemon
/// reports another live writer — so this exact command was ALLOWED on the built
/// binary, and `git show HEAD:.trusty-mpm.toml` read `documents_only = true`
/// afterwards. No daemon is consulted here, and none is running in this test.
/// Test: itself.
#[test]
fn a_merge_of_a_declaring_ref_is_denied() {
    let dir = repo();
    declaring_branch(dir.path(), "declare", "documents_only = true\n");
    assert_denied(dir.path(), "git merge --no-edit declare");
    // And through the composition forms the shared walker resolves.
    assert_denied(dir.path(), "git rebase declare");
}

/// 🔴 The bound: a merge that lands the SAME declaration value is ordinary work.
///
/// Why: the rule must gate the declaration CHANGING, not merging at all. A
/// branch that never touched `.trusty-mpm.toml` carries whatever HEAD carries,
/// so the merge is exactly as safe as it was before #7905 — and a rule that
/// denied it would make every main checkout unmergeable.
/// Test: itself.
#[test]
fn a_merge_of_a_matching_ref_is_allowed() {
    // Neither side declares.
    let dir = repo();
    git_ok(dir.path(), &["checkout", "-q", "-b", "docs"]);
    std::fs::write(dir.path().join("NOTES.md"), "x\n").expect("write");
    git_ok(dir.path(), &["add", "NOTES.md"]);
    git_ok(dir.path(), &["commit", "-m", "notes"]);
    git_ok(dir.path(), &["checkout", "-q", "main"]);
    assert_allowed(dir.path(), "git merge --no-edit docs");

    // Both sides declare the same value.
    let dir = repo();
    std::fs::write(
        dir.path().join(".trusty-mpm.toml"),
        "documents_only = true\n",
    )
    .expect("write");
    git_ok(dir.path(), &["add", ".trusty-mpm.toml"]);
    git_ok(dir.path(), &["commit", "-m", "declare on main"]);
    git_ok(dir.path(), &["checkout", "-q", "-b", "more"]);
    std::fs::write(dir.path().join("NOTES.md"), "x\n").expect("write");
    git_ok(dir.path(), &["add", "NOTES.md"]);
    git_ok(dir.path(), &["commit", "-m", "notes"]);
    git_ok(dir.path(), &["checkout", "-q", "main"]);
    assert_allowed(dir.path(), "git merge --no-edit more");
}

/// 🔴 A named ref that does not resolve is a revision the guard cannot vouch
/// for.
///
/// Why: [`declared_documents_only_at_rev`] answers `None` both for "carries no
/// declaration" and for "could not be read", and only the first of those means
/// nothing changes. Without [`rev_is_resolvable`] a typo'd or not-yet-fetched
/// ref would compare equal to an undeclared HEAD and ALLOW.
/// Test: itself.
#[test]
fn a_merge_of_an_unresolvable_ref_is_denied() {
    let dir = repo();
    assert_denied(dir.path(), "git merge --no-edit no/such/ref");
}

/// 🔴 `cherry-pick` replays a diff, so the question is whether the diff touches
/// the declaration.
///
/// Why: the resulting tree is not knowable before the command runs, but a
/// commit that never touches `.trusty-mpm.toml` cannot move the declaration.
/// Test: itself.
#[test]
fn a_cherry_pick_of_a_declaring_commit_is_denied() {
    let dir = repo();
    declaring_branch(dir.path(), "declare", "documents_only = true\n");
    assert_denied(dir.path(), "git cherry-pick declare");
    assert_denied(dir.path(), "git revert --no-edit declare");
}

/// The bound for the replay verbs: an unrelated commit is ordinary work.
#[test]
fn a_cherry_pick_that_leaves_the_declaration_alone_is_allowed() {
    let dir = repo();
    git_ok(dir.path(), &["checkout", "-q", "-b", "notes"]);
    std::fs::write(dir.path().join("NOTES.md"), "x\n").expect("write");
    git_ok(dir.path(), &["add", "NOTES.md"]);
    git_ok(dir.path(), &["commit", "-m", "notes"]);
    git_ok(dir.path(), &["checkout", "-q", "main"]);
    assert_allowed(dir.path(), "git cherry-pick notes");
}

/// 🔴 `git checkout -B <branch> <declaring-ref>` moves HEAD with no merge.
#[test]
fn a_checkout_b_to_a_declaring_ref_is_denied() {
    let dir = repo();
    declaring_branch(dir.path(), "declare", "documents_only = true\n");
    assert_denied(dir.path(), "git checkout -B main declare");
    assert_denied(dir.path(), "git switch -C main declare");
    // Creating a branch AT HEAD names no start point and moves nothing.
    assert_allowed(dir.path(), "git checkout -b feature");
}

/// 🔴 `git reset` moves HEAD in every mode, `--soft` included.
#[test]
fn a_reset_soft_to_a_declaring_ref_is_denied() {
    let dir = repo();
    declaring_branch(dir.path(), "declare", "documents_only = true\n");
    for mode in ["--soft", "--mixed", "--hard"] {
        assert_denied(dir.path(), &format!("git reset {mode} declare"));
    }
    // A reset that names no revision resets to HEAD: nothing moves.
    assert_allowed(dir.path(), "git reset");
    assert_allowed(dir.path(), "git reset --hard");
}

/// 🔴 `git update-ref` and `git symbolic-ref` move HEAD with no working-tree
/// operation at all.
#[test]
fn an_update_ref_of_the_current_branch_is_denied() {
    let dir = repo();
    declaring_branch(dir.path(), "declare", "documents_only = true\n");
    assert_denied(dir.path(), "git update-ref refs/heads/main declare");
    assert_denied(dir.path(), "git update-ref HEAD declare");
    assert_denied(dir.path(), "git symbolic-ref HEAD refs/heads/declare");
    // A ref outside `refs/heads/` is not HEAD's branch.
    assert_allowed(dir.path(), "git update-ref refs/notes/x declare");
}

/// 🔴 `am` and `apply --index` name no revision, so they fail closed.
///
/// Why: both were already residual bypasses of the ADR-0044 write boundary —
/// they write the files a patch names, and the patch names them, not the argv.
/// #7905 makes that bypass one that can land the declaration.
/// Test: itself.
#[test]
fn a_patch_application_that_names_no_revision_is_denied() {
    let dir = repo();
    assert_denied(dir.path(), "git am /tmp/p.patch");
    assert_denied(dir.path(), "git apply --index /tmp/p.patch");
    // A plain `git apply` does not touch the index or HEAD.
    assert_allowed(dir.path(), "git apply /tmp/p.patch");
}

/// 🔴 The bound the critic named: a verb that cannot move HEAD stays ungated.
///
/// Why: `git fetch` is how the declaring commit ARRIVES, and denying it would
/// be a new refusal on the one step that is unambiguously safe — a fetch writes
/// remote-tracking refs and never HEAD. The read-only verbs are here for the
/// same reason.
/// Test: itself.
#[test]
fn a_fetch_of_a_declaring_branch_is_allowed() {
    let dir = repo();
    declaring_branch(dir.path(), "declare", "documents_only = true\n");
    for command in [
        "git fetch origin declare",
        "git log --oneline -5 declare",
        "git show declare:.trusty-mpm.toml",
        "git status",
        "git diff declare",
        "git stash list",
    ] {
        assert_allowed(dir.path(), command);
    }
}

/// An in-progress control flag finishes a move already judged when it started.
#[test]
fn an_in_progress_control_flag_is_not_a_new_move() {
    let dir = repo();
    declaring_branch(dir.path(), "declare", "documents_only = true\n");
    for command in [
        "git rebase --continue",
        "git merge --abort",
        "git am --skip",
    ] {
        assert_allowed(dir.path(), command);
    }
}

/// A directory that is not a main checkout is outside this rule entirely.
///
/// Why: the same scope every ADR-0044 rule has — a worktree is where the work
/// is supposed to happen, so a HEAD move there is ordinary.
#[test]
fn a_directory_that_is_not_a_main_checkout_is_not_gated() {
    let plain = tempfile::tempdir().expect("tempdir");
    assert_allowed(plain.path(), "git merge --no-edit declare");
}
