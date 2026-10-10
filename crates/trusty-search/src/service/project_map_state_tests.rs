//! The project map's rules: migration, park, pin, create, rebuild, boot
//! (#9169 PR-B1).
//!
//! Why: these are the decisions the socket methods, the boot path and the
//! CLI share; each is pinned here against real roots on disk.
//! Test: this module.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::*;
use crate::core::project_map::{MapView, ParkReason, PinnedBy};
use crate::core::project_map_io;
use crate::core::registry::IndexRegistry;
use crate::service::project_map_boot::BootMap;
use crate::service::project_resolve::{resolve, resolve_pinned, ProjectQuery};

const REPO: &str = "bobmatnyc/apex";

/// Four registrations of one repo: `apex` and `apex-9a4a584b` share the main
/// checkout `main/`, `apex-clone` is a second main checkout, `apex-wt` is a
/// linked worktree. With no corpus on disk every recency ties, so f7 picks
/// the lowest id among the main checkouts: `apex`.
struct Fixture {
    dir: tempfile::TempDir,
    persisted: Vec<PersistedIndex>,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let main = dir.path().join("main");
        let clone = dir.path().join("clone");
        let wt = dir.path().join("wt");
        for root in [&main, &clone] {
            std::fs::create_dir_all(root.join(".git")).expect("main checkout");
        }
        std::fs::create_dir_all(&wt).expect("worktree");
        std::fs::write(wt.join(".git"), "gitdir: elsewhere\n").expect("linked .git");
        let row = |id: &str, root: &Path| PersistedIndex {
            colocated: true,
            repo_identity: Some(REPO.to_string()),
            ..PersistedIndex::new(id, root)
        };
        let persisted = vec![
            row("apex", &main),
            row("apex-9a4a584b", &main),
            row("apex-clone", &clone),
            row("apex-wt", &wt),
        ];
        crate::service::persistence::save_index_registry_at(
            &dir.path().join("indexes.toml"),
            &persisted,
        )
        .expect("seed indexes.toml");
        Self { dir, persisted }
    }

    fn registry(&self) -> PathBuf {
        self.dir.path().join("indexes.toml")
    }

    fn map_path(&self) -> PathBuf {
        project_map_io::map_path_beside(&self.registry())
    }

    fn candidates(&self) -> Vec<Candidate> {
        gather_candidates(&self.persisted, &[])
    }
}

/// A disk with no runtime: kinds are real `stat`s, no corpus is read.
fn disk() -> LiveDisk {
    LiveDisk {
        names: crate::service::constants::ephemeral_dir_names(),
        resident: HashMap::new(),
        runtime: None,
    }
}

fn identity() -> ProjectQuery {
    ProjectQuery::Identity(REPO.to_string())
}

/// Why: the first answers from the map must equal PR-A's derived answers.
/// Test: this test.
#[test]
fn migration_pins_every_groups_f7_winner_and_parks_nothing() {
    let fx = Fixture::new();
    let candidates = fx.candidates();
    let derived = resolve(&identity(), &candidates, &disk())
        .expect("derived")
        .index
        .index_id;
    let map = ensure_map(&fx.map_path(), &candidates, &disk()).expect("migrate");
    assert_eq!(map.projects.len(), 1, "{:?}", map.projects);
    assert_eq!(map.projects[0].key, REPO);
    assert_eq!(map.projects[0].index_id, derived);
    assert_eq!(map.projects[0].pinned_by, PinnedBy::Migration);
    assert!(map.parked.is_empty());
    let pinned = resolve_pinned(&identity(), &candidates, &disk(), &map.view())
        .expect("pinned")
        .index
        .index_id;
    assert_eq!(pinned, derived);
}

/// Why: a restart must read the map it wrote, not migrate again.
/// Test: this test.
#[test]
fn migration_persists_and_a_second_run_changes_nothing() {
    let fx = Fixture::new();
    let registry_before = std::fs::read(fx.registry()).expect("registry");
    let first = ensure_map(&fx.map_path(), &fx.candidates(), &disk()).expect("migrate");
    let bytes = std::fs::read(fx.map_path()).expect("written");
    let second = ensure_map(&fx.map_path(), &fx.candidates(), &disk()).expect("reload");
    assert_eq!(first, second);
    assert_eq!(
        std::fs::read(fx.map_path()).expect("map"),
        bytes,
        "no rewrite"
    );
    assert_eq!(
        std::fs::read(fx.registry()).expect("registry"),
        registry_before
    );
}

/// Why: an unreadable `indexes.toml` must propagate and write no map, from
/// the daemon and from `rebuild-map` alike.
/// Test: this test.
#[test]
fn an_unreadable_registry_writes_no_map() {
    let fx = Fixture::new();
    std::fs::write(fx.registry(), "this is [[ not toml").expect("garbage");
    let state = SearchAppState::new(IndexRegistry::new()).with_registry_path(fx.registry());
    let err = MapContext::capture(&state).err().expect("refused");
    assert!(matches!(err, ProjectMapError::Registry { .. }), "{err:?}");
    assert!(rebuild_map(&fx.registry(), None).is_err());
    assert!(!fx.map_path().exists(), "no map may be written");
}

/// Why: `rebuild-map` keeps the bad bytes and writes a fresh migration.
/// Test: this test.
#[test]
fn rebuild_moves_the_bad_file_aside_before_it_writes() {
    let fx = Fixture::new();
    std::fs::write(fx.map_path(), "garbage [[").expect("bad map");
    let (moved, map) = rebuild_map(&fx.registry(), None).expect("rebuild");
    let aside = moved.expect("moved aside");
    assert_eq!(std::fs::read_to_string(aside).expect("aside"), "garbage [[");
    assert_eq!(map.projects.len(), 1);
    assert_eq!(
        project_map_io::load(&fx.map_path()).expect("load"),
        Some(map)
    );
}

/// Why: a pin decides a group's answer; a parked index never answers, and
/// an exact parked id yields to its group like a worktree does.
/// Test: this test.
#[test]
fn a_pin_overrides_the_f7_winner_of_its_group() {
    let fx = Fixture::new();
    let candidates = fx.candidates();
    let view = MapView {
        pins: [(REPO.to_string(), "apex-clone".to_string())].into(),
        ..MapView::default()
    };
    let got = resolve_pinned(&identity(), &candidates, &disk(), &view).expect("pinned");
    assert_eq!(got.index.index_id, "apex-clone");
    assert!(got.duplicates.iter().any(|d| d.index_id == "apex"));
}

/// Why: see [`a_pin_overrides_the_f7_winner_of_its_group`].
/// Test: this test.
#[test]
fn a_parked_index_never_wins_and_an_exact_parked_id_yields_to_its_group() {
    let fx = Fixture::new();
    let candidates = fx.candidates();
    let view = MapView {
        parked: ["apex".to_string()].into(),
        ..MapView::default()
    };
    let by_identity = resolve_pinned(&identity(), &candidates, &disk(), &view).expect("live");
    assert_eq!(by_identity.index.index_id, "apex-9a4a584b");
    let exact = ProjectQuery::Name("apex".to_string());
    let by_id = resolve_pinned(&exact, &candidates, &disk(), &view).expect("yields");
    assert_eq!(by_id.index.index_id, "apex-9a4a584b");
    assert_eq!(by_id.matched_by, "index_id");
}

/// Why: only a worktree, an orphan or a same-root sibling parks; a second
/// main checkout and a group's live index are refused.
/// Test: this test.
#[test]
fn park_refuses_a_main_checkout_and_records_a_worktree() {
    let fx = Fixture::new();
    let (map, candidates) = (fx.map_path(), fx.candidates());
    for refused in ["apex-clone", "apex"] {
        let err = park_decision(&map, refused, &candidates, &disk()).expect_err(refused);
        assert!(matches!(err, MapRefusal::Refused(_)), "{refused}: {err:?}");
    }
    let parked = park_decision(&map, "apex-wt", &candidates, &disk()).expect("wt");
    assert_eq!(parked.row.reason, ParkReason::Worktree);
    assert_eq!(parked.row.key, REPO);
    assert_eq!(parked.canonical.as_deref(), Some("apex"));
    let sibling = park_decision(&map, "apex-9a4a584b", &candidates, &disk()).expect("sibling");
    assert_eq!(sibling.row.reason, ParkReason::Operator);
    let stored = project_map_io::load(&map).expect("load").expect("map");
    assert_eq!(stored.parked.len(), 2);
    assert!(matches!(
        park_decision(&map, "nope", &candidates, &disk()),
        Err(MapRefusal::UnknownIndex(_))
    ));
    unpark_decision(&map, "apex-wt").expect("unpark");
    assert!(matches!(
        unpark_decision(&map, "apex-wt"),
        Err(MapRefusal::NotParked(_))
    ));
}

/// Why: an operator pin moves the group's answer; a worktree is never pinned.
/// Test: this test.
#[test]
fn pin_moves_a_groups_answer_and_refuses_a_worktree() {
    let fx = Fixture::new();
    let (map_path, candidates) = (fx.map_path(), fx.candidates());
    let (pin, previous) = pin_decision(
        &map_path,
        "apex-clone",
        PinnedBy::Operator,
        &candidates,
        &disk(),
    )
    .expect("pin");
    assert_eq!(
        (pin.key.as_str(), previous.as_deref()),
        (REPO, Some("apex"))
    );
    let map = project_map_io::load(&map_path).expect("load").expect("map");
    let got = resolve_pinned(&identity(), &candidates, &disk(), &map.view()).expect("resolve");
    assert_eq!(got.index.index_id, "apex-clone");
    let err = pin_decision(
        &map_path,
        "apex-wt",
        PinnedBy::Operator,
        &candidates,
        &disk(),
    )
    .expect_err("worktree");
    assert!(matches!(err, MapRefusal::Refused(_)), "{err:?}");
}

/// Why: a group created after the migration gets a `create` pin, once.
/// Test: this test.
#[test]
fn creating_an_index_for_an_unpinned_group_pins_it() {
    let mut fx = Fixture::new();
    ensure_map(&fx.map_path(), &fx.candidates(), &disk()).expect("migrate");
    let other = fx.dir.path().join("other");
    std::fs::create_dir_all(other.join(".git")).expect("other repo");
    fx.persisted.push(PersistedIndex {
        repo_identity: Some("bobmatnyc/other".to_string()),
        ..PersistedIndex::new("other", &other)
    });
    let pinned = pin_on_create(&fx.map_path(), "other", &fx.candidates(), &disk())
        .expect("pin")
        .expect("a new group is pinned");
    assert_eq!(pinned.pinned_by, PinnedBy::Create);
    assert_eq!(pinned.key, "bobmatnyc/other");
    let again = pin_on_create(&fx.map_path(), "other", &fx.candidates(), &disk()).expect("again");
    assert_eq!(again, None, "a pinned group is left alone");
}

/// Why: a parked index is never warm-loaded, and a boot that cannot read the
/// map heals nothing.
/// Test: this test.
#[tokio::test(flavor = "multi_thread")]
async fn a_parked_index_is_skipped_on_boot() {
    let fx = Fixture::new();
    park_decision(&fx.map_path(), "apex-wt", &fx.candidates(), &disk()).expect("park");
    let state = SearchAppState::new(IndexRegistry::new()).with_registry_path(fx.registry());
    let boot = BootMap::load(&state).await;
    assert!(boot.is_readable());
    let (parked, rest) = boot.split(fx.persisted.clone());
    assert_eq!(
        parked.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
        ["apex-wt"]
    );
    assert_eq!(rest.len(), 3);

    std::fs::write(fx.map_path(), "garbage [[").expect("corrupt");
    let boot = BootMap::load(&state).await;
    assert!(!boot.is_readable());
    let gone = PersistedIndex::new("gone", fx.dir.path().join("deleted-root"));
    assert_eq!(
        boot.heal(vec![gone]).len(),
        1,
        "an unreadable map heals nothing"
    );
    assert_eq!(
        std::fs::read_to_string(fx.map_path()).expect("map"),
        "garbage [["
    );
}

/// Why: a prune must never take a parked index, and one that cannot read the
/// map prunes nothing.
/// Test: this test.
#[test]
fn retain_unparked_drops_parked_rows_and_fails_closed() {
    let fx = Fixture::new();
    park_decision(&fx.map_path(), "apex-wt", &fx.candidates(), &disk()).expect("park");
    let kept = retain_unparked(&fx.registry(), fx.persisted.clone()).expect("read");
    assert!(kept.iter().all(|e| e.id != "apex-wt"));
    assert_eq!(kept.len(), 3);
    std::fs::write(fx.map_path(), "garbage [[").expect("corrupt");
    assert!(retain_unparked(&fx.registry(), fx.persisted.clone()).is_err());
}

/// Why: the boot salvage writes registry rows, so it skips a parked entry and
/// every entry when the map was not read.
/// Test: this test.
#[test]
fn salvage_skips_parked_entries_and_everything_without_the_map() {
    let rows = || {
        vec![
            PersistedIndex::new("parked", "/gone/a"),
            PersistedIndex::new("free", "/gone/b"),
        ]
    };
    let kept = BootMap::with_parked(["parked".to_string()]).salvageable(rows());
    assert_eq!(
        kept.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
        ["free"]
    );
    assert!(BootMap::default().salvageable(rows()).is_empty());
}
