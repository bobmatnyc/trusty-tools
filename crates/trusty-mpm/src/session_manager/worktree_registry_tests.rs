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

/// `locked` is git's removal veto and must be read in BOTH spellings the
/// porcelain format emits — bare, and with an operator-supplied reason.
#[test]
fn parse_worktree_list_reads_locked_with_and_without_reason() {
    let stdout = "\
worktree /repos/owner/repo
HEAD abc123
branch refs/heads/main

worktree /repos/owner/repo/.worktrees/bare-lock
HEAD def456
locked

worktree /repos/owner/repo/.worktrees/lock-with-reason
HEAD 789abc
locked keeping this for the postmortem

worktree /repos/owner/repo/.worktrees/unlocked
HEAD 000111
";
    let got = parse_worktree_list(stdout);
    assert_eq!(got.len(), 4, "one record per stanza; got {got:?}");
    assert!(got[1].locked, "the bare `locked` line must set the flag");
    assert!(
        got[2].locked,
        "`locked <reason>` must set the flag too, not just the bare word"
    );
    assert!(
        !got[3].locked,
        "control: a worktree with no `locked` line must not be flagged"
    );
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

// ── the structural containment boundary (#4224 review, HIGH) ─────────────
//
// Deriving from git widened what is SEEN from five path shapes to anything
// registered under `repos_root`. Replayed against the dogfood machine that
// admitted five of the operator's own long-lived checkouts, leaving the #3649
// sentinel as the only thing between enumeration and an unattended
// `remove_dir_all`. The rule these tests pin is that a candidate must be a
// STRICT DESCENDANT of the managed project directory whose registry named it.
//
// Each test below asserts the negative AND a positive control that must be
// enumerated in the same call, so none of them can pass because enumeration
// returned nothing at all — the way this module fails when git rejects a flag
// or the anchor check regresses.

/// An operator checkout the operator parked at the project-slot position, next
/// to a managed project, is NOT a reclaim candidate.
///
/// Why: this is the live shape the #4224 review measured —
/// `<owner>/ft-follow-links`, `<owner>/trusty-tools-main-build` and
/// `<owner>/gb-connect-card` are all real linked worktrees of a managed project
/// that the operator parked beside it, and all three became enumerable the
/// moment discovery stopped being shape-based. They are excluded by POSITION:
/// a candidate must live inside the project that owns it, which no checkout
/// parked as a sibling can ever satisfy, whatever it is named.
#[test]
fn enumerate_excludes_a_sibling_checkout_of_the_same_repo() {
    let fixture = GitWorktreeFixture::new();
    let owner_dir = fixture.repo.parent().expect("<repos_root>/<owner>");
    let sibling = fixture.add_worktree_at(owner_dir, "operator-checkout");
    // Positive control: an ordinary in-project worktree in the SAME call.
    let control = fixture.add_worktree("control-session");

    let found = enumerate_registered_worktrees(&fixture.repos_root);
    let canonical_control = std::fs::canonicalize(&control).unwrap_or_else(|_| control.clone());
    assert!(
        found.contains(&canonical_control),
        "control: an in-project worktree MUST still be enumerated, or this test \
         proves nothing; got {found:?}"
    );

    let canonical_sibling = std::fs::canonicalize(&sibling).unwrap_or_else(|_| sibling.clone());
    assert!(
        !found.contains(&canonical_sibling),
        "a checkout parked beside the managed project must never be a reclaim \
         candidate; got {found:?}"
    );
    assert!(sibling.exists(), "the sibling checkout must be untouched");
}

/// A worktree in a sibling DIRECTORY whose name merely shares a string prefix
/// with the project is NOT a candidate.
///
/// Why: two things at once. It is the live
/// `<owner>/trusty-tools-worktrees/wt-*` shape — the operator's own worktree
/// parking lot, three of which the review measured as newly enumerable. And it
/// is the string-prefix trap: `repo-worktrees` starts with `repo` as a string,
/// so a containment check written on strings rather than on PATH COMPONENTS
/// would admit every one of them. `Path::starts_with` is component-wise; this
/// test is what stops someone "simplifying" it into a `to_string_lossy()`
/// comparison.
#[test]
fn enumerate_excludes_a_worktree_parked_beside_the_project() {
    let fixture = GitWorktreeFixture::new();
    let owner_dir = fixture.repo.parent().expect("<repos_root>/<owner>");
    let parking_lot = owner_dir.join("repo-worktrees");
    let parked = fixture.add_worktree_at(&parking_lot, "wt-1");
    let control = fixture.add_worktree("control-session");

    let found = enumerate_registered_worktrees(&fixture.repos_root);
    let canonical_control = std::fs::canonicalize(&control).unwrap_or_else(|_| control.clone());
    assert!(
        found.contains(&canonical_control),
        "control: an in-project worktree MUST still be enumerated; got {found:?}"
    );

    let canonical_parked = std::fs::canonicalize(&parked).unwrap_or_else(|_| parked.clone());
    assert!(
        !found.contains(&canonical_parked),
        "`<owner>/repo-worktrees/wt-1` merely shares a string prefix with the \
         project `<owner>/repo` and must NOT be enumerated; got {found:?}"
    );
    assert!(parked.exists(), "the parked worktree must be untouched");
}

/// Containment is STRICT: a path equal to its own containment boundary is
/// never a candidate.
///
/// Why: `starts_with` alone is reflexive — `p.starts_with(p)` is `true` — so
/// without the explicit `!=` a path equal to the boundary satisfies its own
/// containment test.
///
/// Scope honestly: on every topology observed today it is `is_main`, NOT this
/// `!=`, that keeps the operator's project checkout unselectable — git lists a
/// project's own root as the main record, and main records are already
/// filtered. Zero instances on this machine reach the `!=`. It is kept as
/// defence in depth for the case `is_main` does not cover: a project directory
/// that is itself a NON-main linked worktree of some other registry being
/// interrogated, which is reachable in principle and would otherwise put the
/// operator's checkout on a deletion list.
///
/// That same masking is why the strictness is pinned HERE rather than through
/// [`enumerate_registered_worktrees`] — an enumeration-level test would keep
/// passing with the `!=` deleted, making it a guard no test defends. Driving
/// [`scan_from_anchor`] directly lets the boundary be set to a path that IS
/// in the registry, so the `!=` is the only thing that can exclude it.
///
/// #4288 strengthened this: the scan now carries the verdict as a VALUE, so the
/// test asserts the exclusion RULE that fired ([`Admission::OutsideProject`])
/// rather than mere absence from a set — a much harder assertion to satisfy by
/// accident.
#[test]
fn scan_from_anchor_never_admits_the_containment_boundary_itself() {
    let fixture = GitWorktreeFixture::new();
    let wt = fixture.add_worktree("session-a");
    let canonical_wt = std::fs::canonicalize(&wt).unwrap_or_else(|_| wt.clone());
    let canonical_root = std::fs::canonicalize(&fixture.repos_root).expect("canonical root");
    let canonical_repo = std::fs::canonicalize(&fixture.repo).expect("canonical repo");

    let admission_under_boundary = |boundary: &std::path::Path| -> Admission {
        let mut collected = std::collections::BTreeSet::new();
        scan_from_anchor(&fixture.repo, &canonical_root, boundary, &mut collected);
        collected
            .into_iter()
            .find(|(path, _, _)| path == &canonical_wt)
            .map(|(_, _, key)| key.admission)
            .expect("the worktree must be SCANNED under either boundary")
    };

    // Control: bounded by the real project, the worktree IS admitted — so the
    // exclusion below cannot be attributed to the anchor or the registry.
    assert_eq!(
        admission_under_boundary(&canonical_repo),
        Admission::Admitted,
        "control: bounded by the project, a real in-project worktree must be admitted"
    );

    // Now make the worktree its OWN boundary: it is no longer a STRICT
    // descendant, and must not be admitted.
    assert_eq!(
        admission_under_boundary(&canonical_wt),
        Admission::OutsideProject,
        "a path equal to its containment boundary must never be a candidate — \
         this is what keeps the operator's project checkout unselectable"
    );
}

/// A git-`locked` worktree — the operator's explicit "do not remove this" — is
/// never a candidate.
///
/// Why: NOT because `--force` overrides the lock — it does not. Git refuses a
/// single `--force` on a locked worktree (exit 128, "use 'remove -f -f' to
/// override or unlock first") and leaves the directory intact. The problem is
/// that `decommission::remove_session_worktree` reads that refusal as a generic
/// git failure and falls back to `std::fs::remove_dir_all`, deleting the
/// directory anyway and orphaning git's registry entry. So the veto is defeated
/// through the error path, and never enumerating a locked worktree is the only
/// place it survives.
#[test]
fn enumerate_excludes_a_locked_worktree() {
    let fixture = GitWorktreeFixture::new();
    let locked = fixture.add_worktree("locked-by-operator");
    fixture.lock_worktree(&locked);
    let control = fixture.add_worktree("not-locked");

    let found = enumerate_registered_worktrees(&fixture.repos_root);
    let canonical_control = std::fs::canonicalize(&control).unwrap_or_else(|_| control.clone());
    assert!(
        found.contains(&canonical_control),
        "control: an identically-placed UNLOCKED worktree must be enumerated, so \
         the exclusion below is attributable to the lock and nothing else; got {found:?}"
    );

    let canonical_locked = std::fs::canonicalize(&locked).unwrap_or_else(|_| locked.clone());
    assert!(
        !found.contains(&canonical_locked),
        "a git-locked worktree must never be enumerated for reclaim; got {found:?}"
    );
}

/// A worktree registered to a repo under `repos_root` but living OUTSIDE it is
/// not enumerated.
///
/// Why: git will happily report a worktree parked in `/tmp`; enumerating it
/// would put the sweep's blast radius outside the managed workspace entirely.
/// Since #4224 the strict-descendant rule already rejects this path (a `/tmp`
/// worktree is not inside the project either), so the `repos_root` check this
/// test names is now the OUTER of two independent bounds rather than the only
/// one — it still matters for a project directory that symlinks out of the
/// managed root, which is the only way the two can disagree.
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

// ── the full scan and its exclusion reasons (#4288) ──────────────────────

/// Every registered worktree the scan drops must come back with the reason it
/// was dropped (#4288 in-scope item 2).
///
/// Why: `enumerate_registered_worktrees` returns only admitted paths, so 33 of
/// the 118 registered worktrees on the dogfood machine — including the two
/// landmines #4288 documents, six of them holding uncommitted work — are
/// invisible to every operator surface. Reconciliation cannot report what
/// enumeration silently discards. This asserts the scan reports BOTH sets and
/// names the rule that fired, not just a count.
#[test]
fn scan_reports_the_excluded_set_with_reasons() {
    let fixture = GitWorktreeFixture::new();
    let admitted = fixture.add_worktree("admitted-one");
    let locked = fixture.add_worktree("locked-one");
    fixture.lock_worktree(&locked);
    let outside = fixture.add_worktree_at(&fixture.repos_root.join("owner"), "beside-the-project");

    let scan = scan_registered_worktrees(&fixture.repos_root);
    let verdict = |p: &std::path::Path| -> Admission {
        let c = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
        scan.iter()
            .find(|s| s.path == c)
            .unwrap_or_else(|| panic!("scan must report {}; got {scan:?}", c.display()))
            .admission
    };

    assert_eq!(verdict(&admitted), Admission::Admitted);
    assert_eq!(verdict(&locked), Admission::Locked);
    assert_eq!(verdict(&outside), Admission::OutsideProject);
    assert_eq!(verdict(&fixture.repo), Admission::MainCheckout);
    assert_eq!(
        Admission::Locked.reason(),
        "excluded: git-locked by the operator"
    );

    // The admitted subset is exactly what `enumerate` still returns — the scan
    // widened what is REPORTED, never what is proposed for deletion.
    let enumerated = enumerate_registered_worktrees(&fixture.repos_root);
    let admitted_from_scan: Vec<_> = scan
        .iter()
        .filter(|s| s.admission == Admission::Admitted)
        .map(|s| s.path.clone())
        .collect();
    assert_eq!(admitted_from_scan.len(), 1, "got {admitted_from_scan:?}");
    assert_eq!(enumerated.len(), 1, "got {enumerated:?}");
    assert_eq!(enumerated[0], admitted_from_scan[0]);
}

/// Two worktrees under the SAME path prefix, registered in DIFFERENT
/// registries, are each attributed to the registry that actually holds them
/// (#4288).
///
/// Why: this is the decisive counterexample from #4288 — path shape
/// misclassifies 29% of worktrees, and the proof is two sibling directories
/// inside `.base/.worktrees/` whose registries differ. A rule reading `/.base/`
/// out of the path maps both to the base clone and is wrong about one of them.
#[test]
fn scan_reports_both_registries_for_the_same_path_prefix() {
    let fixture = GitWorktreeFixture::new();
    let base_owned = fixture.add_base_clone_worktree("in-base-registry");
    let base_worktrees_dir = fixture.repo.join(".base").join(".worktrees");
    let repo_owned = fixture.add_worktree_at(&base_worktrees_dir, "in-repo-registry");

    let scan = scan_registered_worktrees(&fixture.repos_root);
    let registry_of = |p: &std::path::Path| -> std::path::PathBuf {
        let c = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
        scan.iter()
            .find(|s| s.path == c)
            .unwrap_or_else(|| panic!("scan must report {}", c.display()))
            .registry_root
            .clone()
    };

    let base_root = std::fs::canonicalize(fixture.repo.join(".base")).expect("canonical .base");
    let repo_root = std::fs::canonicalize(&fixture.repo).expect("canonical repo");
    assert_eq!(registry_of(&base_owned), base_root);
    assert_eq!(registry_of(&repo_owned), repo_root);
    assert_ne!(
        base_root, repo_root,
        "the two registries must genuinely differ, or this test proves nothing"
    );
}

/// A worktree nested inside another worktree is enumerated BEFORE its
/// container (#4288 in-scope item 4).
///
/// Why: the pre-#4288 order came out of a `BTreeSet<PathBuf>`, which sorts a
/// parent before its children. Nine registered worktrees on the dogfood machine
/// sit inside another registered worktree; removing the parent first takes the
/// child's directory with it and leaves git holding a dangling registry entry.
#[test]
fn enumerate_orders_nested_worktree_before_its_parent() {
    let fixture = GitWorktreeFixture::new();
    let parent = fixture.add_worktree("outer");
    let child = fixture.add_nested_worktree(&parent, ".claude/worktrees", "inner");

    let found = enumerate_registered_worktrees(&fixture.repos_root);
    let canonical = |p: &std::path::Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.into());
    let idx = |p: &std::path::Path| {
        found
            .iter()
            .position(|f| f == &canonical(p))
            .unwrap_or_else(|| panic!("{} must be enumerated; got {found:?}", p.display()))
    };
    assert!(
        idx(&child) < idx(&parent),
        "the nested worktree must precede its container; got {found:?}"
    );
}
