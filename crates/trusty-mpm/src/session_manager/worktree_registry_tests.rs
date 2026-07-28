//! Tests for the git-native worktree registry (#4207 slice 1).
//!
//! Why: the module's whole claim is that git — not a path shape — decides what
//! a worktree is and who owns it, so every test that matters here drives REAL
//! `git worktree add`. A mocked git would test the mock.
//! What: the pure porcelain parse, registry-root resolution (the replacement
//! for grandparent inference), and the enumeration contract — including the
//! two cases the five-shape walk got wrong.
//! Test: this file IS the test module.

use super::*;

use crate::session_manager::worktree_git_fixture::GitWorktreeFixture;

// ── the pure parser ──────────────────────────────────────────────────────

#[test]
fn parse_worktree_list_reads_porcelain_records() {
    let stdout = "\
worktree /repos/owner/repo
HEAD abc123
branch refs/heads/main

worktree /repos/owner/repo/.worktrees/session-a
HEAD def456
branch refs/heads/session/session-a

worktree /somewhere/detached
HEAD 0f0f0f
detached

worktree /repos/owner/repo/.base
bare

worktree /gone
HEAD 111111
prunable gitdir file points to non-existent location
";
    let got = parse_worktree_list(stdout);
    assert_eq!(got.len(), 5, "one record per stanza; got {got:?}");

    assert_eq!(got[0].path, std::path::PathBuf::from("/repos/owner/repo"));
    assert_eq!(got[0].branch.as_deref(), Some("main"));
    assert!(got[0].is_main, "git lists the main worktree first");

    assert_eq!(got[1].branch.as_deref(), Some("session/session-a"));
    assert!(!got[1].is_main);

    assert_eq!(got[2].branch, None, "a detached worktree has no branch");
    assert!(got[3].bare, "the `bare` attribute line must be read");
    assert!(
        got[4].prunable,
        "`prunable <reason>` must set the flag, not just the bare word"
    );
}

#[test]
fn parse_worktree_list_marks_only_the_first_record_main() {
    let stdout = "worktree /a\n\nworktree /b\n\nworktree /c\n";
    let got = parse_worktree_list(stdout);
    let mains: Vec<_> = got.iter().filter(|w| w.is_main).collect();
    assert_eq!(mains.len(), 1, "exactly one main record; got {got:?}");
    assert_eq!(mains[0].path, std::path::PathBuf::from("/a"));
}

#[test]
fn parse_worktree_list_empty_input_is_empty() {
    assert!(parse_worktree_list("").is_empty());
}

// ── registry_root_for: the replacement for grandparent inference ─────────

/// A linked worktree resolves to the checkout that REGISTERED it, whatever
/// directory it happens to live in.
///
/// Why: this is the fact the grandparent rule was standing in for. The
/// fixture parks the worktree at `<repo>/.worktrees/<name>`, whose grandparent
/// coincidentally IS `<repo>` — so this test alone does not distinguish the
/// two rules. `registry_root_for_ignores_where_the_worktree_is_parked` does.
#[test]
fn registry_root_for_linked_worktree_is_the_owning_checkout() {
    let fixture = GitWorktreeFixture::new();
    let wt = fixture.add_worktree("session-root-test");
    let root = registry_root_for(&wt).expect("a real worktree must resolve a registry root");
    assert_eq!(
        std::fs::canonicalize(&root).unwrap_or(root),
        std::fs::canonicalize(&fixture.repo).unwrap_or_else(|_| fixture.repo.clone()),
    );
}

/// The owning checkout is resolved from git's registry, NOT from the
/// candidate's position on disk — the #4207 defect stated as a test.
///
/// Why: fourteen worktrees on this machine live under `<repo>/.base/…` but are
/// registered to `<repo>`. Grandparent inference answered `.base`, which
/// disowns them, and every downstream git call was then aimed at the wrong
/// repository. Here the worktree is parked three levels deep in a directory
/// with no relationship to the owning repo, so the grandparent of the
/// candidate is emphatically not its root.
#[test]
fn registry_root_for_ignores_where_the_worktree_is_parked() {
    let fixture = GitWorktreeFixture::new();
    let parked = fixture.add_worktree_at(
        &fixture.repo.join("deeply").join("nested").join("elsewhere"),
        "parked",
    );
    let grandparent = parked
        .parent()
        .and_then(|p| p.parent())
        .expect("candidate has a grandparent");
    assert_ne!(
        grandparent,
        fixture.repo.as_path(),
        "test invariant: the grandparent must NOT be the owning checkout, or \
         this test cannot distinguish the two rules"
    );

    let root = registry_root_for(&parked).expect("registry root");
    assert_eq!(
        std::fs::canonicalize(&root).unwrap_or(root),
        std::fs::canonicalize(&fixture.repo).unwrap_or_else(|_| fixture.repo.clone()),
        "the owning checkout must be derived from git, not from the path shape"
    );
}

#[test]
fn registry_root_for_non_repo_is_none() {
    let tmp = tempfile::tempdir().expect("tempdir");
    assert!(
        registry_root_for(tmp.path()).is_none(),
        "a directory outside any repository must resolve to None, never a guess"
    );
}

// ── list_registered_worktrees ────────────────────────────────────────────

#[test]
fn list_registered_worktrees_reports_a_real_worktree() {
    let fixture = GitWorktreeFixture::new();
    let wt = fixture.add_worktree("listed");
    // Anchor on the WORKTREE itself, which is how `git_worktree_list_agrees`
    // now calls this — git resolves the repository from the candidate.
    let listed = list_registered_worktrees(&wt).expect("list");
    let canonical_wt = std::fs::canonicalize(&wt).unwrap_or_else(|_| wt.clone());
    assert!(
        listed.iter().any(
            |w| std::fs::canonicalize(&w.path).unwrap_or_else(|_| w.path.clone()) == canonical_wt
        ),
        "the worktree must appear in its own repository's registry; got {listed:?}"
    );
}

#[test]
fn list_registered_worktrees_none_outside_a_repo() {
    let tmp = tempfile::tempdir().expect("tempdir");
    assert!(
        list_registered_worktrees(tmp.path()).is_none(),
        "an unanswerable probe must be None, never an empty list that reads as \
         `no worktrees exist`"
    );
}

// ── enumerate_registered_worktrees ───────────────────────────────────────

/// THE #4207 slice-1 regression test: a registered worktree at a location NONE
/// of the five hard-coded shapes covered is discovered.
///
/// Why: the removed walk probed `.worktrees/`, `.base/.worktrees/`,
/// `.claude/worktrees/`, `.base/.claude/worktrees/`, and
/// `.base/.worktrees/<id>/.claude/worktrees/`. This worktree sits at
/// `<repo>/agents/scratch/wt-1`, which matches none of them, so the walk
/// returned it under no circumstances. Reverting `find_orphaned_worktrees` to
/// the shape walk makes this test fail.
#[test]
fn enumerate_finds_worktree_at_an_unwalked_location() {
    let fixture = GitWorktreeFixture::new();
    let parked = fixture.add_worktree_at(&fixture.repo.join("agents").join("scratch"), "wt-1");
    let found = enumerate_registered_worktrees(&fixture.repos_root);
    let canonical = std::fs::canonicalize(&parked).unwrap_or_else(|_| parked.clone());
    assert!(
        found.contains(&canonical),
        "a registered worktree must be found wherever it lives; got {found:?}"
    );
}

/// The `.base` bare clone is the SECOND registry a managed project can own,
/// and worktrees registered to it must be enumerated too.
#[test]
fn enumerate_finds_worktree_registered_to_base_clone() {
    let fixture = GitWorktreeFixture::new();
    let wt = fixture.add_base_clone_worktree("session-in-base");
    let found = enumerate_registered_worktrees(&fixture.repos_root);
    let canonical = std::fs::canonicalize(&wt).unwrap_or_else(|_| wt.clone());
    assert!(
        found.contains(&canonical),
        "a worktree registered to the .base clone must be enumerated; got {found:?}"
    );
}

/// A plain directory sitting in a worktree-shaped location is NOT a worktree
/// and must not be proposed as a reclaim candidate.
///
/// Why: this is the deliberate narrowing derive-not-walk buys. The old walk
/// collected any leaf directory under a `.worktrees/` parent, so a `mkdir`
/// was indistinguishable from a checkout git actually created.
#[test]
fn enumerate_ignores_plain_directory_that_is_not_a_worktree() {
    let fixture = GitWorktreeFixture::new();
    let fake = fixture.repo.join(".worktrees").join("just-a-mkdir");
    std::fs::create_dir_all(&fake).expect("mkdir");
    let found = enumerate_registered_worktrees(&fixture.repos_root);
    let canonical = std::fs::canonicalize(&fake).unwrap_or_else(|_| fake.clone());
    assert!(
        !found.contains(&canonical),
        "a plain directory is not a registered worktree; got {found:?}"
    );
}

/// The main checkout is never a reclaim candidate.
#[test]
fn enumerate_excludes_the_main_checkout() {
    let fixture = GitWorktreeFixture::new();
    fixture.add_worktree("some-session");
    let found = enumerate_registered_worktrees(&fixture.repos_root);
    let canonical_repo =
        std::fs::canonicalize(&fixture.repo).unwrap_or_else(|_| fixture.repo.clone());
    assert!(
        !found.contains(&canonical_repo),
        "the main checkout must never be enumerated for reclaim; got {found:?}"
    );
}

/// A worktree registered to a repo under `repos_root` but living OUTSIDE it is
/// not enumerated — the containment boundary the old walk had structurally.
///
/// Why: the walk could only ever produce paths beneath the root it was handed.
/// Git will happily report a worktree parked in `/tmp`; enumerating it would
/// silently widen the sweep's blast radius beyond the managed workspace root.
#[test]
fn enumerate_excludes_worktrees_outside_the_repos_root() {
    let fixture = GitWorktreeFixture::new();
    let outside_parent = tempfile::tempdir().expect("tempdir");
    let outside = fixture.add_worktree_at(outside_parent.path(), "far-away");
    let found = enumerate_registered_worktrees(&fixture.repos_root);
    let canonical = std::fs::canonicalize(&outside).unwrap_or_else(|_| outside.clone());
    assert!(
        !found.contains(&canonical),
        "a worktree outside the managed repos root must not be enumerated; got {found:?}"
    );
}

/// A registered worktree whose DIRECTORY is gone is never proposed.
///
/// Why: the asymmetry this module commits to is that a failed observation may
/// only ever SHRINK what is proposed for deletion, never enlarge it. A path
/// that will not resolve cannot be compared against the active-session set, so
/// offering it would risk offering a live worktree. Here the directory is
/// removed behind git's back — git still lists the entry (as `prunable`) and
/// the path no longer canonicalizes — and neither route may yield a candidate.
#[test]
fn enumerate_ignores_a_worktree_whose_directory_is_gone() {
    let fixture = GitWorktreeFixture::new();
    let wt = fixture.add_worktree("vanished");
    let canonical = std::fs::canonicalize(&wt).unwrap_or_else(|_| wt.clone());
    std::fs::remove_dir_all(&wt).expect("remove the worktree directory");

    let found = enumerate_registered_worktrees(&fixture.repos_root);
    assert!(
        !found.contains(&canonical),
        "a worktree with no directory left is not a deletion candidate; got {found:?}"
    );
}

#[test]
fn enumerate_missing_repos_root_is_empty() {
    let tmp = tempfile::tempdir().expect("tempdir");
    assert!(enumerate_registered_worktrees(&tmp.path().join("nope")).is_empty());
}
