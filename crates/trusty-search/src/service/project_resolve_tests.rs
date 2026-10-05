//! Tests for the project→index resolver (#9169, rulings f6/f7).
//!
//! Why: the pick is a contract other clients will dial by name — the main
//! checkout wins, recency breaks ties, a worktree never wins, an exact id or a
//! path's owner is never redirected to another repo, and nothing that matched
//! is dropped silently.
//! What: most cases build candidates directly and read them back through
//! [`AsBuilt`], so the f7 order is tested without a filesystem; recency, root
//! classification and path canonicalisation get real tempdir cases through
//! [`LiveDisk`].
//! Test: this module.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use super::*;
use trusty_common::workspace_layout::WorktreeDirNames;

const REPO: &str = "bobmatnyc/trusty-tools";

fn cand(
    id: &str,
    root: &str,
    identity: Option<&str>,
    kind: RootKind,
    corpus: Option<u64>,
) -> Candidate {
    Candidate {
        index_id: id.to_string(),
        root_path: PathBuf::from(root),
        repo_identity: identity.map(str::to_string),
        kind,
        resident: true,
        corpus_modified_unix: corpus,
        // Colocated, so a `LiveDisk` read never reaches the real data dir.
        colocated: true,
    }
}

/// A [`Disk`] that reports each candidate's kind and recency as built.
struct AsBuilt {
    /// `None`: deriving an identity fails the test. `Some(x)`: derive answers `x`.
    derived: Option<Option<&'static str>>,
}

/// An [`AsBuilt`] whose `derive_identity` fails the test if reached.
const NO_DERIVE: AsBuilt = AsBuilt { derived: None };

impl Disk for AsBuilt {
    fn kind(&self, c: &Candidate) -> RootKind {
        c.kind
    }
    fn corpus_modified_unix(&self, c: &Candidate) -> Option<u64> {
        c.corpus_modified_unix
    }
    fn canonicalize(&self, _: &Path) -> Option<PathBuf> {
        None
    }
    fn derive_identity(&self, path: &Path) -> Option<String> {
        match self.derived {
            Some(id) => id.map(str::to_string),
            None => panic!("derive must not run for {path:?}: the path is under a registered root"),
        }
    }
}

/// A [`Disk`] that records the id of every candidate it is asked about.
struct Recording<D> {
    inner: D,
    probed: RefCell<Vec<String>>,
}

impl<D: Disk> Disk for Recording<D> {
    fn kind(&self, c: &Candidate) -> RootKind {
        self.probed.borrow_mut().push(c.index_id.clone());
        self.inner.kind(c)
    }
    fn corpus_modified_unix(&self, c: &Candidate) -> Option<u64> {
        self.probed.borrow_mut().push(c.index_id.clone());
        self.inner.corpus_modified_unix(c)
    }
    fn canonicalize(&self, path: &Path) -> Option<PathBuf> {
        self.inner.canonicalize(path)
    }
    fn derive_identity(&self, path: &Path) -> Option<String> {
        self.inner.derive_identity(path)
    }
}

fn live() -> LiveDisk {
    LiveDisk {
        names: WorktreeDirNames::default(),
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
        let got = resolve(&query, &fleet, &NO_DERIVE).expect("resolves");
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
    let got =
        resolve(&ProjectQuery::Name("feat-x".into()), &fleet(), &NO_DERIVE).expect("resolves");
    assert_eq!(got.index.index_id, "trusty-tools-4e2cf878");
    assert_eq!(got.matched_by, "index_id");
}

/// Why: #9169 — `supervisor`, `cto-supervisor` and `architect` are three repos
/// that share only a root commit, so one `content:` identity. An exact id or a
/// path inside a live root must return that index, not the group's f7 pick.
/// Test: this test.
#[test]
fn an_exact_id_or_owned_path_wins_over_repos_sharing_its_content_identity() {
    let shared = Some("content:be8b1ba6947f050abec6b50ef34389f7fa70858d");
    let fleet = vec![
        cand(
            "architect",
            "/p/architect",
            shared,
            RootKind::MainCheckout,
            None,
        ),
        cand(
            "cto-supervisor",
            "/p/bob-duetto/cto-supervisor",
            shared,
            RootKind::MainCheckout,
            None,
        ),
        cand(
            "supervisor",
            "/p/bobmatnyc/supervisor",
            shared,
            RootKind::MainCheckout,
            None,
        ),
    ];
    for (query, how) in [
        (ProjectQuery::Name("supervisor".into()), "index_id"),
        (
            ProjectQuery::Path("/p/bobmatnyc/supervisor/src/main.rs".into()),
            "path",
        ),
    ] {
        let got = resolve(&query, &fleet, &NO_DERIVE).expect("resolves");
        assert_eq!(got.index.index_id, "supervisor", "{query:?}");
        assert_eq!(got.matched_by, how);
        assert_eq!(
            ids(&got.duplicates),
            ["architect", "cto-supervisor"],
            "{query:?}"
        );
    }
}

/// Why: #9169 — four agents keep an index on a subdirectory of one repo
/// (`/Users/masa/trusty-agents/<agent>/okg`). Each root has no `.git` of its
/// own, so each is its own group: none is another's duplicate, and the shared
/// identity alone cannot choose between them.
/// Test: this test.
#[test]
fn sibling_subdirectory_indexes_are_never_each_others_duplicates() {
    let shared = "content:ac6e482f3142b050226dfe7309b9fca5c31c4abd";
    let fleet: Vec<Candidate> = [
        ("assistant-okg-53822c21", "assistant"),
        ("assistant-okg-90440c4e", "cto-assistant"),
        ("assistant-okg-9e4b923f", "izzie"),
        ("assistant-okg-2b56a57e", "writing-assistant"),
    ]
    .iter()
    .map(|(id, agent)| {
        let root = format!("/a/trusty-agents/{agent}/okg");
        cand(id, &root, Some(shared), RootKind::Checkout, None)
    })
    .collect();
    for query in [
        ProjectQuery::Name("assistant-okg-9e4b923f".into()),
        ProjectQuery::Path("/a/trusty-agents/izzie/okg/notes/today.md".into()),
    ] {
        let got = resolve(&query, &fleet, &NO_DERIVE).expect("resolves");
        assert_eq!(got.index.index_id, "assistant-okg-9e4b923f", "{query:?}");
        assert!(
            got.duplicates.is_empty(),
            "{query:?}: {:?}",
            ids(&got.duplicates)
        );
    }
    match resolve(&ProjectQuery::Identity(shared.into()), &fleet, &NO_DERIVE) {
        Err(ResolveMiss::Ambiguous { matches }) => assert_eq!(matches.len(), 4),
        other => panic!("expected Ambiguous, got {other:?}"),
    }
}

/// Why: ruling f7 — a main checkout wins outright, recency only ranks the rest.
/// Test: this test.
#[test]
fn the_main_checkout_beats_a_newer_indeterminate_root() {
    let group = vec![
        cand(
            "main",
            "/r/main",
            Some(REPO),
            RootKind::MainCheckout,
            Some(10),
        ),
        cand(
            "ext",
            "/Volumes/Ext/r",
            Some(REPO),
            RootKind::Indeterminate,
            Some(500),
        ),
    ];
    let got = resolve(&ProjectQuery::Identity(REPO.into()), &group, &NO_DERIVE).expect("resolves");
    assert_eq!(got.index.index_id, "main");
    assert_eq!(ids(&got.duplicates), ["ext"]);
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
        cand("main", "/r", Some(REPO), RootKind::MainCheckout, Some(1)),
    ];
    let got = resolve(&ProjectQuery::Identity(REPO.into()), &group, &NO_DERIVE).expect("resolves");
    assert_eq!(got.index.index_id, "main", "the newest root is a worktree");
    assert_eq!(ids(&got.duplicates), ["wt"]);
}

/// Why: ruling f7 — between equal roots the newest corpus wins and an absent
/// corpus sorts last.
/// Test: this test.
#[test]
fn an_absent_corpus_sorts_last_among_main_checkouts() {
    let group = vec![
        cand(
            "absent",
            "/r/absent",
            Some(REPO),
            RootKind::MainCheckout,
            None,
        ),
        cand(
            "old",
            "/r/old",
            Some(REPO),
            RootKind::MainCheckout,
            Some(50),
        ),
        cand(
            "new",
            "/r/new",
            Some(REPO),
            RootKind::MainCheckout,
            Some(100),
        ),
    ];
    let got = resolve(&ProjectQuery::Identity(REPO.into()), &group, &NO_DERIVE).expect("resolves");
    assert_eq!(got.index.index_id, "new");
    assert_eq!(ids(&got.duplicates), ["absent", "old"]);
}

/// Why: #9169 — `last_indexed_unix` has no production writer, so every live
/// tie fell to the lowest id (`apex` over `apex-9a4a584b`). Recency must come
/// from something a real index has: its corpus file.
/// What: two main checkouts of one repo registered as registration writes
/// them (round-tripped through `indexes.toml`, no recency field), each with an
/// `index.redb` of a different age; the newer corpus wins.
/// Test: this test.
#[test]
fn the_most_recently_written_corpus_wins_between_two_main_checkouts() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let now = SystemTime::now();
    let mut rows = Vec::new();
    // Distinct parents: `APEX` and `apex` are one directory on a case-insensitive
    // filesystem.
    for (id, dir, age) in [
        ("apex", "repos/APEX", 86_400),
        ("apex-9a4a584b", "duetto/apex", 60),
    ] {
        let root = tmp.path().join(dir);
        std::fs::create_dir_all(root.join(".git")).expect(".git");
        std::fs::create_dir_all(root.join(".trusty-search")).expect("store");
        let corpus = root.join(".trusty-search/index.redb");
        std::fs::write(&corpus, b"redb").expect("corpus");
        let file = std::fs::File::options()
            .write(true)
            .open(&corpus)
            .expect("open");
        file.set_modified(now - Duration::from_secs(age))
            .expect("mtime");
        rows.push(PersistedIndex {
            colocated: true,
            repo_identity: Some("duettoresearch/apex".to_string()),
            ..PersistedIndex::new(id, root)
        });
    }
    let toml = tmp.path().join("indexes.toml");
    crate::service::persistence::save_index_registry_at(&toml, &rows).expect("save");
    let loaded = crate::service::persistence::load_index_registry_at(&toml).expect("load");
    assert!(loaded.iter().all(|r| r.last_indexed_unix.is_none()));

    let candidates = gather_candidates(&loaded, &[]);
    let query = ProjectQuery::Identity("duettoresearch/apex".into());
    let got = resolve(&query, &candidates, &live()).expect("resolves");
    assert_eq!(got.index.index_id, "apex-9a4a584b", "{got:?}");
    assert_eq!(got.index.kind, RootKind::MainCheckout);
    assert_eq!(ids(&got.duplicates), ["apex"]);
    assert!(got.index.corpus_modified_unix > got.duplicates[0].corpus_modified_unix);
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
    match resolve(&ProjectQuery::Identity(REPO.into()), &group, &NO_DERIVE) {
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
    match resolve(&ProjectQuery::Name("APEX".into()), &fleet, &NO_DERIVE) {
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
        &NO_DERIVE,
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
    let derives = AsBuilt {
        derived: Some(Some(REPO)),
    };
    let got = resolve(&query, &fleet(), &derives).expect("resolves");
    assert_eq!(got.index.index_id, "trusty-tools-4e2cf878");
    assert_eq!(got.matched_by, "path");
    assert!(matches!(
        resolve(
            &query,
            &fleet(),
            &AsBuilt {
                derived: Some(None)
            }
        ),
        Err(ResolveMiss::NotFound { .. })
    ));
}

/// Why: #9169 — `/w/trusty-tools/../apex` names apex; matched by raw
/// components it resolved to trusty-tools.
/// What: an existing path is canonicalised through [`LiveDisk`]; a missing one
/// is normalised lexically.
/// Test: this test.
#[test]
fn a_dotdot_query_path_matches_the_root_it_names() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let base = std::fs::canonicalize(tmp.path()).expect("canonical tempdir");
    for name in ["apex", "trusty-tools"] {
        std::fs::create_dir_all(base.join(name).join(".git")).expect(".git");
    }
    let at = |id: &str, identity: &str| {
        let root = base.join(id).display().to_string();
        cand(id, &root, Some(identity), RootKind::Indeterminate, None)
    };
    let real = vec![at("apex", "duetto/apex"), at("trusty-tools", REPO)];
    let query = ProjectQuery::Path(base.join("trusty-tools/../apex"));
    let got = resolve(&query, &real, &live()).expect("resolves");
    assert_eq!(got.index.index_id, "apex", "{query:?}");

    let missing = ProjectQuery::Path("/w/bobmatnyc/trusty-tools/../../duetto/apex/src".into());
    let got = resolve(&missing, &fleet(), &NO_DERIVE).expect("resolves");
    assert_eq!(got.index.index_id, "apex-9a4a584b");
}

/// Why: #9169 — the resolver runs in the free lane with no deadline, so one
/// dead `/Volumes` root must not be touched by a resolve for another project,
/// and is reported as indeterminate rather than orphaned when it is touched.
/// What: a resolve and a miss record every candidate they probe; the
/// `/Volumes` root appears in neither, and a miss probes at most five.
/// Test: this test.
#[test]
fn a_volumes_root_is_indeterminate_and_never_probed_for_another_project() {
    let kemono = cand(
        "kemono",
        "/Volumes/nonexistent-9169/kemono",
        Some("masa/kemono"),
        RootKind::Indeterminate,
        None,
    );
    assert_eq!(live().kind(&kemono), RootKind::Indeterminate);
    assert_eq!(live().corpus_modified_unix(&kemono), None);

    let mut fleet = fleet();
    fleet.push(kemono);
    for n in 0..8 {
        let id = format!("unrelated-{n}");
        let root = format!("/u/{id}");
        fleet.push(cand(
            &id,
            &root,
            Some(&format!("o/{id}")),
            RootKind::MainCheckout,
            None,
        ));
    }
    let disk = Recording {
        inner: NO_DERIVE,
        probed: RefCell::new(Vec::new()),
    };
    resolve(&ProjectQuery::Name("trusty-tools".into()), &fleet, &disk).expect("resolves");
    let mut probed = disk.probed.take();
    probed.sort();
    probed.dedup();
    assert_eq!(probed, ["feat-x", "trusty-tools-4e2cf878"]);

    let miss = resolve(&ProjectQuery::Name("zzz-nothing".into()), &fleet, &disk);
    assert!(matches!(miss, Err(ResolveMiss::NotFound { .. })));
    assert!(
        disk.probed.borrow().len() <= MAX_NEAREST,
        "{:?}",
        disk.probed
    );
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
    let got = gather_candidates(&[row], &resident);
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
