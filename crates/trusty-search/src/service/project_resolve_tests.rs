//! Tests for the project→index resolver (#9169, rulings f6/f7).
//!
//! Why: the pick is a contract other clients will dial by name — the main
//! checkout wins, recency breaks ties, a worktree never wins, and nothing that
//! matched is dropped silently.
//! What: candidates are built directly so the f7 order is tested without a
//! filesystem; root classification gets its own tempdir case.
//! Test: this module.

use std::path::{Path, PathBuf};

use super::*;
use trusty_common::workspace_layout::WorktreeDirNames;

const REPO: &str = "bobmatnyc/trusty-tools";

fn cand(
    id: &str,
    root: &str,
    identity: Option<&str>,
    kind: RootKind,
    last: Option<u64>,
) -> Candidate {
    Candidate {
        index_id: id.to_string(),
        root_path: PathBuf::from(root),
        repo_identity: identity.map(str::to_string),
        kind,
        resident: true,
        last_indexed_unix: last,
    }
}

/// A repo with a main checkout and a NEWER worktree, plus an unrelated repo.
fn fleet() -> Vec<Candidate> {
    vec![
        cand(
            "trusty-tools-4e2cf878",
            "/w/bobmatnyc/trusty-tools",
            Some(REPO),
            RootKind::MainCheckout,
            Some(10),
        ),
        cand(
            "feat-x",
            "/w/bobmatnyc/trusty-tools/.worktrees/feat-x",
            Some(REPO),
            RootKind::Worktree,
            Some(1_000),
        ),
        cand(
            "apex-9a4a584b",
            "/w/duetto/apex",
            Some("duetto/apex"),
            RootKind::MainCheckout,
            Some(5),
        ),
    ]
}

/// A `derive` that fails the test if the resolver reaches for git.
fn no_derive(path: &Path) -> Option<String> {
    panic!("derive must not run for {path:?}: the path is under a registered root");
}

fn ids(list: &[Candidate]) -> Vec<&str> {
    list.iter().map(|c| c.index_id.as_str()).collect()
}

/// Why: one project, three spellings, one answer — and the duplicate worktree
/// index is reported rather than dropped.
/// Test: this test.
#[test]
fn resolves_by_name_by_identity_and_by_path() {
    let fleet = fleet();
    for (query, how) in [
        (ProjectQuery::Name("trusty-tools".into()), "name"),
        (ProjectQuery::Identity(REPO.into()), "repo_identity"),
        (
            ProjectQuery::Path("/w/bobmatnyc/trusty-tools/.worktrees/feat-x/src".into()),
            "path",
        ),
    ] {
        let got = resolve(&query, &fleet, no_derive).expect("resolves");
        assert_eq!(got.index.index_id, "trusty-tools-4e2cf878", "{query:?}");
        assert_eq!(got.matched_by, how, "{query:?}");
        assert_eq!(ids(&got.duplicates), ["feat-x"], "{query:?}");
        assert_eq!(got.duplicates[0].kind, RootKind::Worktree);
    }
}

/// Why: a caller holding a worktree's own id still gets the canonical index.
/// Test: this test.
#[test]
fn an_exact_index_id_resolves_to_its_repos_main_checkout() {
    let got = resolve(&ProjectQuery::Name("feat-x".into()), &fleet(), no_derive).expect("resolves");
    assert_eq!(got.index.index_id, "trusty-tools-4e2cf878");
    assert_eq!(got.matched_by, "index_id");
}

/// Why: ruling f7 — the main checkout wins outright, recency only ranks the rest.
/// Test: this test.
#[test]
fn the_main_checkout_beats_a_newer_plain_checkout() {
    let group = vec![
        cand(
            "main",
            "/r/main",
            Some(REPO),
            RootKind::MainCheckout,
            Some(10),
        ),
        cand("copy", "/r/copy", Some(REPO), RootKind::Checkout, Some(500)),
    ];
    let got = resolve(&ProjectQuery::Identity(REPO.into()), &group, no_derive).expect("resolves");
    assert_eq!(got.index.index_id, "main");
    assert_eq!(ids(&got.duplicates), ["copy"]);
}

/// Why: ruling f7 — a worktree root never wins, however recently indexed. This
/// is the case that fails if the worktree rule is inverted.
/// Test: this test.
#[test]
fn a_worktree_never_wins_even_when_newest() {
    let group = vec![
        cand(
            "wt",
            "/r/.worktrees/wt",
            Some(REPO),
            RootKind::Worktree,
            Some(9_999),
        ),
        cand("copy", "/r/copy", Some(REPO), RootKind::Checkout, Some(1)),
    ];
    let got = resolve(&ProjectQuery::Identity(REPO.into()), &group, no_derive).expect("resolves");
    assert_eq!(got.index.index_id, "copy", "the newest root is a worktree");
    assert_eq!(ids(&got.duplicates), ["wt"]);
}

/// Why: ruling f7 — with no main checkout, the most recently indexed wins and a
/// never-indexed root sorts last.
/// Test: this test.
#[test]
fn recency_breaks_a_tie_between_two_plain_checkouts() {
    let group = vec![
        cand("never", "/r/never", Some(REPO), RootKind::Checkout, None),
        cand("old", "/r/old", Some(REPO), RootKind::Checkout, Some(50)),
        cand("new", "/r/new", Some(REPO), RootKind::Checkout, Some(100)),
    ];
    let got = resolve(&ProjectQuery::Identity(REPO.into()), &group, no_derive).expect("resolves");
    assert_eq!(got.index.index_id, "new");
    assert_eq!(ids(&got.duplicates), ["never", "old"]);
}

/// Why: a repo indexed only through worktrees has no live index to hand out;
/// the miss names what is there instead of picking a worktree.
/// Test: this test.
#[test]
fn a_repo_with_only_worktree_indexes_has_no_live_index() {
    let group = vec![
        cand(
            "wt",
            "/r/.worktrees/wt",
            Some(REPO),
            RootKind::Worktree,
            Some(9),
        ),
        cand("gone", "/r/gone", Some(REPO), RootKind::Orphaned, Some(8)),
    ];
    match resolve(&ProjectQuery::Identity(REPO.into()), &group, no_derive) {
        Err(ResolveMiss::NoLiveIndex { group }) => assert_eq!(ids(&group), ["gone", "wt"]),
        other => panic!("expected NoLiveIndex, got {other:?}"),
    }
}

/// Why: a bare name two repos share must not silently pick one of them.
/// Test: this test.
#[test]
fn a_name_shared_by_two_repos_is_ambiguous() {
    let mut fleet = fleet();
    fleet.push(cand(
        "apex",
        "/Users/masa/Duetto/repos/APEX",
        Some("masa/apex"),
        RootKind::MainCheckout,
        Some(1),
    ));
    match resolve(&ProjectQuery::Name("APEX".into()), &fleet, no_derive) {
        Err(ResolveMiss::Ambiguous { matches }) => {
            assert_eq!(ids(&matches), ["apex", "apex-9a4a584b"]);
        }
        other => panic!("expected Ambiguous, got {other:?}"),
    }
}

/// Why: #9169 — a miss returns the nearest candidates, never a bare not-found.
/// Test: this test.
#[test]
fn a_miss_reports_the_nearest_candidates() {
    match resolve(
        &ProjectQuery::Name("trusty-tool".into()),
        &fleet(),
        no_derive,
    ) {
        Err(ResolveMiss::NotFound { nearest }) => {
            assert_eq!(nearest.len(), 3, "every registration is a candidate here");
            assert_eq!(nearest[0].index_id, "trusty-tools-4e2cf878");
        }
        other => panic!("expected NotFound, got {other:?}"),
    }
}

/// Why: a path to a clone the daemon has never indexed still names the repo,
/// through its git identity.
/// Test: this test.
#[test]
fn a_path_outside_every_root_falls_back_to_the_derived_identity() {
    let query = ProjectQuery::Path("/elsewhere/clone".into());
    let got = resolve(&query, &fleet(), |_| Some(REPO.to_string())).expect("resolves");
    assert_eq!(got.index.index_id, "trusty-tools-4e2cf878");
    assert_eq!(got.matched_by, "path");
    assert!(matches!(
        resolve(&query, &fleet(), |_| None),
        Err(ResolveMiss::NotFound { .. })
    ));
}

/// Why: f7 rests on telling a main checkout from a worktree on disk.
/// What: `.git` directory, `.git` file, no `.git`, a missing root, and the two
/// worktree bases (which win even when the directory is gone).
/// Test: this test.
#[test]
fn classify_root_kind_reads_the_git_entry_and_the_worktree_base() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let names = WorktreeDirNames::default();
    let main = tmp.path().join("main");
    std::fs::create_dir_all(main.join(".git")).expect("main");
    let linked = tmp.path().join("linked");
    std::fs::create_dir_all(&linked).expect("linked");
    std::fs::write(linked.join(".git"), "gitdir: /x/.git/worktrees/linked\n").expect("file");
    let plain = tmp.path().join("plain");
    std::fs::create_dir_all(&plain).expect("plain");

    assert_eq!(classify_root_kind(&main, &names), RootKind::MainCheckout);
    assert_eq!(classify_root_kind(&linked, &names), RootKind::Worktree);
    assert_eq!(classify_root_kind(&plain, &names), RootKind::Checkout);
    assert_eq!(
        classify_root_kind(&tmp.path().join("gone"), &names),
        RootKind::Orphaned
    );
    assert_eq!(
        classify_root_kind(&main.join(".worktrees/feat"), &names),
        RootKind::Worktree
    );
    assert_eq!(
        classify_root_kind(&main.join(".claude/worktrees/agent-1"), &names),
        RootKind::Worktree
    );
}

/// Why: an index registered in memory but not yet in `indexes.toml` is still
/// resolvable by its id.
/// Test: this test.
#[test]
fn a_resident_handle_with_no_persisted_row_is_still_a_candidate() {
    let mut row = PersistedIndex::new("cold", "/r/cold");
    row.repo_identity = Some(REPO.to_string());
    let resident = vec![("hot".to_string(), PathBuf::from("/r/hot"))];
    let got = gather_candidates(&[row], &resident, |_| RootKind::Checkout);
    assert_eq!(ids(&got), ["cold", "hot"]);
    assert!(!got[0].resident);
    assert!(got[1].resident);
    assert_eq!(got[1].repo_identity, None);
}

/// Why: the one input string is classified before anything is looked up.
/// Test: this test.
#[test]
fn parse_classifies_paths_identities_and_names() {
    assert_eq!(
        ProjectQuery::parse("/abs/repo"),
        Ok(ProjectQuery::Path("/abs/repo".into()))
    );
    assert_eq!(
        ProjectQuery::parse(" BobMatNyc/Trusty-Tools "),
        Ok(ProjectQuery::Identity(REPO.into()))
    );
    assert_eq!(
        ProjectQuery::parse("content:abc123"),
        Ok(ProjectQuery::Identity("content:abc123".into()))
    );
    assert_eq!(
        ProjectQuery::parse("trusty-tools"),
        Ok(ProjectQuery::Name("trusty-tools".into()))
    );
    assert!(ProjectQuery::parse("./rel/path").is_err());
    assert!(ProjectQuery::parse("   ").is_err());
}
