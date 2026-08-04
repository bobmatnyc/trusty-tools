//! Tests for the "does git still hold state here?" classifier (#4732).
//!
//! Why: this module is the only thing standing between a non-zero `git`
//! exit and `std::fs::remove_dir_all`, so every gate is driven against REAL
//! git — a mocked git would test the mock, and the whole defect was a
//! misreading of what real git actually says. Each fixture is built inside a
//! `tempfile::TempDir`; nothing here ever touches a real worktree.
//! What: one test per gate, plus the near-miss cases where git's message means
//! the opposite of what it reads like.
//! Test: this file IS the test module.

use std::path::Path;

use super::*;

use crate::session_manager::worktree_git_fixture::{GitWorktreeFixture, deny_all};

/// The stderr git really prints when it has nothing registered at a path.
///
/// Why: the constant under test is a substring match, so the tests must feed it
/// git's real sentence rather than the fragment — otherwise the test passes for
/// a match the production path would never see.
fn not_a_working_tree_stderr(path: &Path) -> String {
    format!("fatal: '{}' is not a working tree\n", path.display())
}

/// Run `git -C <dir> worktree remove --force <path>` and hand back its stderr.
///
/// Why: every "git declined" test must classify what git ACTUALLY said, not a
/// remembered paraphrase of it.
fn real_removal_stderr(repo: &Path, path: &Path) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["worktree", "remove", "--force"])
        .arg(path)
        .output()
        .expect("spawn git worktree remove");
    assert!(
        !out.status.success(),
        "precondition: git must have DECLINED to remove {}",
        path.display()
    );
    String::from_utf8_lossy(&out.stderr).into_owned()
}

// ── gate 1: the message ──────────────────────────────────────────────────

/// A `git worktree lock` is the operator saying "leave this alone", and git
/// enforces it with the same exit code it uses for everything else (#4732).
#[test]
fn a_locked_worktree_is_protected() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("locked-4732");
    fx.lock_worktree(&wt);

    let stderr = real_removal_stderr(&fx.repo, &wt);
    assert!(
        stderr.contains("locked working tree"),
        "precondition: git must have refused for the LOCK reason: {stderr}"
    );
    let verdict = protection_after_failed_removal(&wt, &fx.repo, &stderr);
    assert!(
        matches!(verdict, GitProtection::Protected(_)),
        "a locked worktree must never be deletable: {verdict:?}"
    );
    assert!(wt.exists(), "and the directory must still be there");
}

/// `is not a .git file` reads a hair away from `is not a working tree` and
/// means the opposite — the worktree is real and git is protecting it (#4732).
#[test]
fn a_broken_git_file_is_protected() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("broken-git-file-4732");
    std::fs::write(wt.join("precious.txt"), "work\n").expect("write precious file");
    std::fs::write(wt.join(".git"), "gitdir: /nonexistent/xyz\n").expect("corrupt .git");

    let stderr = real_removal_stderr(&fx.repo, &wt);
    let verdict = protection_after_failed_removal(&wt, &fx.repo, &stderr);
    assert!(
        matches!(verdict, GitProtection::Protected(_)),
        "git validated and declined; the near-miss message must not read as \
         'nothing here': {stderr} -> {verdict:?}"
    );
}

/// Any wording this code does not recognize refuses. A reworded git, a `git`
/// shim on `PATH`, a locale change — all fail closed (#4732).
///
/// Mutation note: the subject is a path gates 2 and 3 would both PERMIT — no
/// `.git` entry, not in the registry — so the message gate is the only thing
/// that can refuse it. Run against a real worktree instead, this test passes
/// with the message gate deleted, because the later gates cover for it (caught
/// by mutating gate 1 during #4732).
#[test]
fn an_unrecognized_git_message_is_protected() {
    let fx = GitWorktreeFixture::new();
    let plain = fx.repo.join(".worktrees").join("unrecognized-4732");
    std::fs::create_dir_all(&plain).expect("mkdir");
    assert_eq!(
        registry_verdict(&fx.repo, &plain),
        GitProtection::Unclaimed,
        "precondition: every OTHER gate permits this path"
    );

    let verdict =
        protection_after_failed_removal(&plain, &fx.repo, "fatal: something entirely new\n");
    assert!(
        matches!(verdict, GitProtection::Protected(_)),
        "an unrecognized refusal must fail closed: {verdict:?}"
    );
}

// ── gate 2: the filesystem witness ───────────────────────────────────────

/// The worktree's admin directory is gone, so every `git` call from inside it
/// reports `not a git repository: (null)` — while the working tree, and the
/// uncommitted work in it, is entirely intact. This is the state ~70 worktrees
/// were left in on 2026-07-21 (#4732).
#[test]
fn a_stale_worktree_pointer_is_protected() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("stale-pointer-4732");
    std::fs::write(wt.join("precious.txt"), "uncommitted work\n").expect("write precious file");
    std::fs::remove_dir_all(fx.repo.join(".git").join("worktrees")).expect("drop admin dir");

    assert!(
        super::registry_root_for(&wt).is_none(),
        "precondition: git must be unable to name the owning checkout"
    );
    let verdict = protection_without_registry_root(&wt);
    assert!(
        matches!(verdict, GitProtection::Protected(_)),
        "a stale worktree pointer is a broken worktree, not an unmanaged \
         directory: {verdict:?}"
    );
}

/// An unreadable `.git` answers exactly like an absent one to git, and not at
/// all like one to `symlink_metadata` (#4732).
#[test]
fn an_unreadable_git_entry_is_protected() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("unreadable-git-4732");
    let _restore = deny_all(&wt.join(".git"));

    let verdict = protection_without_registry_root(&wt);
    assert!(
        matches!(verdict, GitProtection::Protected(_)),
        "an unreadable .git is a worktree git could not read, not an absent \
         one: {verdict:?}"
    );
}

/// Gate 2 also guards the post-removal path: git's `not a working tree`
/// message must not override a `.git` sitting right there (#4732).
///
/// Mutation note: the subject is a stale-pointer worktree, so gate 1 sees the
/// permissive message and gate 3's registry no longer names the path — the
/// filesystem witness is the ONLY thing that still knows this is a worktree.
/// Against a healthy worktree the registry covers for it and the test passes
/// with gate 2 deleted (caught by mutating gate 2 during #4732).
#[test]
fn a_dot_git_entry_overrides_gits_not_a_working_tree_message() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("witness-beats-message-4732");
    std::fs::remove_dir_all(fx.repo.join(".git").join("worktrees")).expect("drop admin dir");
    assert_eq!(
        registry_verdict(&fx.repo, &wt),
        GitProtection::Unclaimed,
        "precondition: git's registry no longer names this path"
    );

    let verdict = protection_after_failed_removal(&wt, &fx.repo, &not_a_working_tree_stderr(&wt));
    assert!(
        matches!(verdict, GitProtection::Protected(_)),
        "the filesystem witness must win over the message: {verdict:?}"
    );
}

// ── gate 3: git's own registry ───────────────────────────────────────────

/// If the registry still names the path, it is protected no matter how the
/// removal command phrased its refusal (#4732).
#[test]
fn a_registered_worktree_is_protected_even_when_git_says_otherwise() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("registered-4732");
    // Drop the pointer file so gate 2 cannot answer, leaving the registry as
    // the only thing that knows this is a worktree.
    std::fs::remove_file(wt.join(".git")).expect("remove .git pointer");

    let verdict = protection_after_failed_removal(&wt, &fx.repo, &not_a_working_tree_stderr(&wt));
    assert!(
        matches!(verdict, GitProtection::Protected(_)),
        "git's registry outranks git's message: {verdict:?}"
    );
}

/// The lock is reported as a lock, so the operator is told to unlock rather
/// than left guessing (#4732).
#[test]
fn the_registry_reports_a_lock_as_a_lock() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("registry-lock-4732");
    fx.lock_worktree(&wt);

    let verdict = registry_verdict(&fx.repo, &wt);
    match verdict {
        GitProtection::Protected(reason) => assert!(
            reason.contains("git-locked"),
            "the reason must name the lock: {reason}"
        ),
        other => panic!("a locked worktree must be Protected: {other:?}"),
    }
}

/// An unreadable registry is a question that was not answered, not a "no"
/// (#4732).
#[test]
fn an_unreadable_registry_is_undetermined() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let outside = tmp.path().join("not-a-repo");
    std::fs::create_dir_all(&outside).expect("mkdir");

    let verdict = registry_verdict(&outside, &outside);
    assert!(
        matches!(verdict, GitProtection::Undetermined(_)),
        "a registry that could not be read must refuse, not permit: {verdict:?}"
    );
}

/// A path that cannot be canonicalised can never match git's own canonical
/// registry entries, so comparing it would silently answer "not registered"
/// (#4732).
#[test]
fn an_uncanonicalisable_path_is_undetermined() {
    let fx = GitWorktreeFixture::new();
    let absent = fx.repo.join(".worktrees").join("never-existed-4732");

    let verdict = registry_verdict(&fx.repo, &absent);
    assert!(
        matches!(verdict, GitProtection::Undetermined(_)),
        "an uncanonicalisable path must refuse: {verdict:?}"
    );
}

/// An ancestor carrying a `.git` git itself will not resolve is the documented
/// over-refusal: it fails closed rather than falling back to permissive
/// (#4732).
#[test]
fn an_unresolvable_ancestor_repo_is_undetermined() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let fake_repo = tmp.path().join("fake-repo");
    let leftover = fake_repo.join("leftover");
    std::fs::create_dir_all(&leftover).expect("mkdir");
    std::fs::write(fake_repo.join(".git"), "not a gitfile at all\n").expect("write bogus .git");

    let verdict = protection_without_registry_root(&leftover);
    assert!(
        matches!(verdict, GitProtection::Undetermined(_)),
        "an ancestor .git git will not resolve must refuse: {verdict:?}"
    );
}

// ── the only permitted outcome ───────────────────────────────────────────

/// The surviving `remove_dir_all` case, half one: no repository exists above
/// the path at all, so nothing can be claiming it (#4732).
#[test]
fn a_directory_no_repository_claims_is_unclaimed() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let leftover = tmp.path().join("leftover-4732");
    std::fs::create_dir_all(&leftover).expect("mkdir");

    assert_eq!(
        protection_without_registry_root(&leftover),
        GitProtection::Unclaimed,
        "a plain directory outside every repository holds no git state"
    );
}

/// The surviving `remove_dir_all` case, half two: a repository DOES own the
/// enclosing tree, and its registry positively does not name this path — a
/// worktree git already pruned, or one whose creation never registered (#4732).
#[test]
fn an_unregistered_leftover_under_a_repo_is_unclaimed() {
    let fx = GitWorktreeFixture::new();
    let leftover = fx.repo.join(".worktrees").join("leftover-4732");
    std::fs::create_dir_all(&leftover).expect("mkdir");

    assert_eq!(
        protection_without_registry_root(&leftover),
        GitProtection::Unclaimed,
        "a directory the owning repository does not register holds no git state"
    );
}

/// The same case reached through the post-removal classifier, with git's real
/// message (#4732).
#[test]
fn an_unregistered_directory_inside_a_repo_is_unclaimed() {
    let fx = GitWorktreeFixture::new();
    let leftover = fx.repo.join(".worktrees").join("unregistered-4732");
    std::fs::create_dir_all(&leftover).expect("mkdir");

    let stderr = real_removal_stderr(&fx.repo, &leftover);
    assert!(
        stderr.contains(NOT_A_WORKING_TREE),
        "precondition: git's real message for a plain directory: {stderr}"
    );
    assert_eq!(
        protection_after_failed_removal(&leftover, &fx.repo, &stderr),
        GitProtection::Unclaimed
    );
}

// ── the enum's own contract ──────────────────────────────────────────────

/// Both non-`Unclaimed` states refuse. A caller that matched only `Protected`
/// would reopen the exact fail-open this enum exists to close (#4732).
#[test]
fn refusal_covers_both_non_unclaimed_states() {
    assert!(
        GitProtection::Protected("held".into()).refusal().is_some(),
        "Protected must refuse"
    );
    assert!(
        GitProtection::Undetermined("unknown".into())
            .refusal()
            .is_some(),
        "Undetermined must refuse — an unanswerable probe is never a verdict"
    );
    assert!(
        GitProtection::Unclaimed.refusal().is_none(),
        "Unclaimed is the only state that permits removal"
    );
}
