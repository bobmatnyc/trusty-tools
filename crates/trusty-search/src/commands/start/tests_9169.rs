//! The boot salvage and the project map (#9169 PR-B1).
//!
//! Why: the boot salvage writes `indexes.toml` — it relinks a moved
//! colocated root and stamps, then after a grace period removes, an
//! ambiguous one. Those are the registry writes the boot heal is barred from
//! when the map cannot say what is parked, and never apply to a parked index.
//! What: a real `restore_indexes` over a missing-root colocated row with two
//! relocation candidates, so the salvage would stamp it ambiguous; the row
//! must come out of the boot unchanged.
//! The same-root dedup prune is barred the same way without the map.
//! Test: the three tests below.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::core::registry::IndexRegistry;
use crate::core::Embedder;
use crate::service::persistence::PersistedIndex;
use crate::service::SearchAppState;
use trusty_common::embedder::MockEmbedder;

const DEAD: &str = "moved-9169";

/// A root that survives the strict relocation denylist (`$HOME`, not `$TMPDIR`).
fn home_anchored(prefix: &str) -> tempfile::TempDir {
    let base = dirs::home_dir()
        .expect("HOME must be set to run trusty-search tests")
        .join(".trusty-search-test-roots");
    std::fs::create_dir_all(&base).expect("create test-roots base");
    tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in(&base)
        .expect("home-anchored root")
}

/// An allowlist under `dir` approving `roots`.
fn approving(dir: &Path, roots: &[&Path]) -> crate::allowlist::AllowlistPaths {
    let paths = crate::allowlist::AllowlistPaths::default()
        .with_allowlist(dir.join("allowlist.toml"))
        .with_project_paths(dir.join("no-projects.json"));
    let cfg = crate::allowlist::AllowlistConfig {
        entries: roots
            .iter()
            .map(|p| crate::allowlist::AllowlistEntry {
                path: p.to_path_buf(),
                name: None,
                exclude: Vec::new(),
                extensions: Vec::new(),
                skip_kg: false,
            })
            .collect(),
    };
    cfg.save_to(&paths.allowlist_file()).expect("allowlist");
    paths
}

/// Boot once over one dead colocated row with two relocation candidates,
/// `map` written beside `indexes.toml` first; returns the row afterwards.
async fn boot_with_map(map: &str) -> PersistedIndex {
    let data = tempfile::tempdir().expect("data dir");
    let work = home_anchored("ts-9169-salvage-");
    // SAFETY: `#[serial]` with every other env mutator.
    unsafe {
        std::env::set_var("TRUSTY_DATA_DIR", data.path());
        std::env::set_var("TRUSTY_DISABLE_WATCHER", "1");
        std::env::set_var(crate::service::lazy_loader::WARMBOOT_MAX_AGE_HOURS_ENV, "0");
    }
    let tracked = work.path().join("tracked");
    let candidates: Vec<PathBuf> = ["a", "b"].iter().map(|n| tracked.join(n)).collect();
    for c in &candidates {
        let dir = c.join(crate::service::colocated_storage::COLOCATED_DIR_NAME);
        std::fs::create_dir_all(&dir).expect("candidate");
        std::fs::write(dir.join("index.redb"), b"x").expect("non-empty redb");
    }
    crate::service::roots_registry::upsert_root(tracked.clone()).expect("track root");
    let gone = work.path().join("gone");
    let mut row = PersistedIndex::new(DEAD.to_string(), gone.clone());
    row.colocated = true;
    crate::service::persistence::save_index_registry(&[row]).expect("seed indexes.toml");
    std::fs::write(data.path().join("project-map.toml"), map).expect("map");

    let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::new(16));
    let allowed: Vec<&Path> = std::iter::once(gone.as_path())
        .chain(candidates.iter().map(PathBuf::as_path))
        .collect();
    let state = SearchAppState::new(IndexRegistry::new())
        .with_allowlist_paths(approving(data.path(), &allowed));
    super::restore::restore_indexes(&state, &embedder, true).await;
    let after = crate::service::persistence::find_index_registry_entry(DEAD)
        .expect("indexes.toml reads")
        .expect("the row survives the boot");
    unsafe {
        std::env::remove_var("TRUSTY_DATA_DIR");
        std::env::remove_var("TRUSTY_DISABLE_WATCHER");
        std::env::remove_var(crate::service::lazy_loader::WARMBOOT_MAX_AGE_HOURS_ENV);
    }
    assert_eq!(after.root_path, gone, "the root was never rewritten");
    after
}

/// Why: with an unreadable map the boot heal is skipped; the salvage's
/// ambiguity stamp (and the reap it starts) must be skipped too.
/// Test: this test.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial]
async fn the_boot_salvage_touches_no_row_without_the_map() {
    let row = boot_with_map("garbage [[").await;
    assert_eq!(
        row.ambiguous_root_since_unix, None,
        "#9169: an unreadable map stamped an ambiguous-root reap clock"
    );
}

/// Why: a parked index is never relocated or reaped at boot.
/// Test: this test.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial]
async fn the_boot_salvage_never_touches_a_parked_row() {
    let map = format!(
        "schema = 1\n\n[[parked]]\nindex_id = \"{DEAD}\"\nkey = \"root:/gone\"\n\
         root_path = \"/gone\"\nreason = \"orphaned\"\nparked_unix = 1\n"
    );
    let row = boot_with_map(&map).await;
    assert_eq!(
        row.ambiguous_root_since_unix, None,
        "#9169: a parked row was stamped for an ambiguous-root reap"
    );
}

/// Why: an unreadable map cannot say which same-root sibling is parked, so
/// the warm-boot dedup must not prune the losing sibling's registry row.
/// What: two colocated rows at one root and a garbage map; after one boot
/// both rows are still in `indexes.toml`.
/// Test: this test.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial]
async fn the_boot_dedup_prunes_no_row_without_the_map() {
    let data = tempfile::tempdir().expect("data dir");
    let work = home_anchored("ts-9169-dedup-");
    // SAFETY: `#[serial]` with every other env mutator.
    unsafe {
        std::env::set_var("TRUSTY_DATA_DIR", data.path());
        std::env::set_var("TRUSTY_DISABLE_WATCHER", "1");
        std::env::set_var(crate::service::lazy_loader::WARMBOOT_MAX_AGE_HOURS_ENV, "0");
    }
    let root = work.path().join("shared");
    std::fs::create_dir_all(root.join(crate::service::colocated_storage::COLOCATED_DIR_NAME))
        .expect("shared root");
    let rows: Vec<PersistedIndex> = ["sibling-a-9169", "sibling-b-9169"]
        .iter()
        .map(|id| {
            let mut row = PersistedIndex::new((*id).to_string(), root.clone());
            row.colocated = true;
            row
        })
        .collect();
    crate::service::persistence::save_index_registry(&rows).expect("seed indexes.toml");
    std::fs::write(data.path().join("project-map.toml"), "garbage [[").expect("map");

    let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::new(16));
    let state = SearchAppState::new(IndexRegistry::new())
        .with_allowlist_paths(approving(data.path(), &[root.as_path()]));
    super::restore::restore_indexes(&state, &embedder, true).await;
    let mut after: Vec<String> = crate::service::persistence::load_index_registry()
        .expect("indexes.toml reads")
        .into_iter()
        .map(|r| r.id)
        .collect();
    after.sort();
    unsafe {
        std::env::remove_var("TRUSTY_DATA_DIR");
        std::env::remove_var("TRUSTY_DISABLE_WATCHER");
        std::env::remove_var(crate::service::lazy_loader::WARMBOOT_MAX_AGE_HOURS_ENV);
    }
    assert_eq!(
        after,
        ["sibling-a-9169", "sibling-b-9169"],
        "#9169: an unreadable map let the boot dedup prune a sibling's row"
    );
}
