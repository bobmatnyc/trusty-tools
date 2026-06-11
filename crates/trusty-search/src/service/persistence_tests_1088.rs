//! Regression tests for persistence-layer data-integrity fixes (issues #1088,
//! #1089, #1090).
//!
//! Why: extracted from `persistence.rs` (inline tests) to keep that file under
//! its allowlist line budget. The two tests here pin the persistence-layer
//! invariants that the issue fixes depend on so future refactors cannot silently
//! break them.
//! What: `upsert_does_not_clobber_colocated_flag_of_other_entries` (#1088/#1089)
//! and `remove_entry_and_root_independent_regression` (#1090).
//! Test: run with `cargo test -p trusty-search -- persistence_tests_1088`.

use std::path::PathBuf;

use crate::service::persistence::{
    load_index_registry_at, remove_index_registry_entry_at, upsert_index_registry_entry_at,
    PersistedIndex,
};
use crate::service::roots_registry::{load_roots_at, remove_root_at, upsert_root_at};

/// Regression test for issues #1088 and #1089.
///
/// Why: `PATCH /indexes/:id` (relocate handler) previously hardcoded
/// `colocated: true` in the `PersistedIndex` it wrote to disk.  This had
/// two consequences:
///   1. It silently toggled `colocated = false` entries to `true`, routing
///      the indexer to a new `.trusty-search/` directory and destroying the
///      central-store data at `<data_dir>/indexes/<id>/`. (#1088)
///
///   2. Other indexes whose `colocated` flag was manually set to `false`
///      in `indexes.toml` were clobbered by subsequent upserts. (#1089)
///
/// Fix: `upsert_index_registry_entry_at` is a targeted merge — it overwrites
/// only the matched id and preserves all other entries unchanged, including
/// their `colocated` flags.
///
/// What: creates two indexes — one colocated=true, one colocated=false.
/// Upserts only the first with a new root_path.  Reloads and asserts:
///   (a) the patched index has the new root_path;
///   (b) the other index's colocated flag is untouched.
///
/// Test: this test.
#[test]
fn upsert_does_not_clobber_colocated_flag_of_other_entries() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();

    // Register one colocated=true and one colocated=false entry.
    upsert_index_registry_entry_at(
        &path,
        PersistedIndex {
            id: "central-store-idx".into(),
            root_path: PathBuf::from("/repos/central"),
            colocated: false, // legacy central-store layout
            ..Default::default()
        },
    )
    .unwrap();
    upsert_index_registry_entry_at(
        &path,
        PersistedIndex {
            id: "colocated-idx".into(),
            root_path: PathBuf::from("/repos/colocated"),
            colocated: true,
            ..Default::default()
        },
    )
    .unwrap();

    // Now upsert only `colocated-idx` with a new root_path (simulating PATCH).
    // The critical invariant: `central-store-idx` must keep colocated=false.
    upsert_index_registry_entry_at(
        &path,
        PersistedIndex {
            id: "colocated-idx".into(),
            root_path: PathBuf::from("/repos/colocated-moved"),
            colocated: true,
            ..Default::default()
        },
    )
    .unwrap();

    let entries = load_index_registry_at(&path).unwrap();
    assert_eq!(entries.len(), 2, "no entries should be added or removed");

    let central = entries
        .iter()
        .find(|e| e.id == "central-store-idx")
        .unwrap();
    assert!(
        !central.colocated,
        "central-store-idx must keep colocated=false after patching a different index \
         (issue #1088 / #1089)"
    );
    assert_eq!(central.root_path, PathBuf::from("/repos/central"));

    let col = entries.iter().find(|e| e.id == "colocated-idx").unwrap();
    assert!(col.colocated, "colocated-idx must keep colocated=true");
    assert_eq!(
        col.root_path,
        PathBuf::from("/repos/colocated-moved"),
        "colocated-idx root_path must be updated"
    );
}

/// Regression test for issue #1090.
///
/// Why: `index remove` (via `DELETE /indexes/:id`) removed the entry from
/// `indexes.toml` but did NOT remove the root from `roots.toml`.  On the
/// next daemon restart, `collect_colocated_entries` scanned the root, found
/// the `.trusty-search/` directory, and re-registered the index — resurrecting
/// up to ~100 pruned entries.
///
/// What: pins the behaviour of `remove_index_registry_entry_at` (persistence
/// layer) and `remove_root_at` (roots layer) independently to confirm they both
/// do what the `delete_index_handler` fix now calls.  The actual HTTP handler
/// integration is covered by manual QA (see PR description).
///
/// Test: this test.
#[test]
fn remove_entry_and_root_independent_regression() {
    let idx_tmp = tempfile::NamedTempFile::new().unwrap();
    let roots_tmp = tempfile::NamedTempFile::new().unwrap();

    // Set up: two indexes registered and two roots tracked.
    upsert_index_registry_entry_at(
        idx_tmp.path(),
        PersistedIndex {
            id: "keep-idx".into(),
            root_path: PathBuf::from("/repos/keep"),
            colocated: true,
            ..Default::default()
        },
    )
    .unwrap();
    upsert_index_registry_entry_at(
        idx_tmp.path(),
        PersistedIndex {
            id: "drop-idx".into(),
            root_path: PathBuf::from("/repos/drop"),
            colocated: true,
            ..Default::default()
        },
    )
    .unwrap();
    upsert_root_at(roots_tmp.path(), PathBuf::from("/repos/keep")).unwrap();
    upsert_root_at(roots_tmp.path(), PathBuf::from("/repos/drop")).unwrap();

    assert_eq!(load_index_registry_at(idx_tmp.path()).unwrap().len(), 2);
    assert_eq!(load_roots_at(roots_tmp.path()).unwrap().len(), 2);

    // Simulate delete_index_handler: remove from indexes.toml AND roots.toml.
    remove_index_registry_entry_at(idx_tmp.path(), "drop-idx").unwrap();
    remove_root_at(roots_tmp.path(), std::path::Path::new("/repos/drop")).unwrap();

    // After removal both files must reflect only the surviving entry.
    let idx_after = load_index_registry_at(idx_tmp.path()).unwrap();
    assert_eq!(idx_after.len(), 1, "one index must remain in indexes.toml");
    assert_eq!(idx_after[0].id, "keep-idx");

    let roots_after = load_roots_at(roots_tmp.path()).unwrap();
    assert_eq!(roots_after.len(), 1, "one root must remain in roots.toml");
    assert_eq!(roots_after[0].path, PathBuf::from("/repos/keep"));

    // Idempotent second delete — must not panic.
    remove_index_registry_entry_at(idx_tmp.path(), "drop-idx").unwrap();
    remove_root_at(roots_tmp.path(), std::path::Path::new("/repos/drop")).unwrap();
    assert_eq!(
        load_index_registry_at(idx_tmp.path()).unwrap().len(),
        1,
        "double-delete must be idempotent"
    );
}
