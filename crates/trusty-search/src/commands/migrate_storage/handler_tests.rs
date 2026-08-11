//! Integration tests for `handle_migrate_storage` — the top-level orchestrator.
//!
//! Why: isolated in their own file to keep `mod.rs` under the 500-line cap.
//! What: drive the full `handle_migrate_storage` flow against tempdir
//! fixtures for every interesting classification case.
//! Test: each test below covers one scenario end-to-end.

use super::*;
use crate::service::persistence::{
    data_dir, indexes_toml_path, load_index_registry_at, save_index_registry,
    save_index_registry_at, PersistedIndex,
};
use serial_test::serial;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tempfile::tempdir;

/// Why: the migration must physically move files from the legacy dir to the
/// colocated dir WITHOUT requiring a re-index.
/// What: create a fake legacy index dir with sentinel files, run the
/// migration, assert files have moved and the colocated dir exists.
///
/// `#[serial]` is required because the test mutates the `TRUSTY_DATA_DIR`
/// process-level environment variable.
#[test]
#[serial]
fn migrate_storage_moves_files_and_updates_registry() {
    let data_dir_tmp = tempdir().unwrap();
    unsafe {
        std::env::set_var("TRUSTY_DATA_DIR", data_dir_tmp.path());
    }

    let project_root = tempdir().unwrap();
    let index_id = "test-migrate-idx";

    // Populate the legacy index data dir with sentinel files.
    let legacy_dir = data_dir().unwrap().join("indexes").join(index_id);
    std::fs::create_dir_all(&legacy_dir).unwrap();
    std::fs::write(legacy_dir.join("index.redb"), b"redb-sentinel").unwrap();
    std::fs::write(legacy_dir.join("hnsw.usearch"), b"hnsw-sentinel").unwrap();
    std::fs::write(legacy_dir.join("schema_version.json"), b"{}").unwrap();

    // Write an indexes.toml with the legacy entry (colocated=false).
    save_index_registry(&[PersistedIndex {
        id: index_id.to_string(),
        root_path: project_root.path().to_path_buf(),
        colocated: false,
        ..Default::default()
    }])
    .unwrap();

    // Run the full handler.
    handle_migrate_storage(false).unwrap();

    // Files must be in the colocated dir.
    let colocated = project_root.path().join(".trusty-search");
    assert!(
        colocated.exists(),
        "colocated dir must exist after migration"
    );
    assert!(
        colocated.join("index.redb").exists(),
        "index.redb must be in colocated dir"
    );
    assert!(
        colocated.join("hnsw.usearch").exists(),
        "hnsw.usearch must be in colocated dir"
    );
    assert!(
        colocated.join("schema_version.json").exists(),
        "schema_version.json must be in colocated dir"
    );

    // Legacy dir must be gone or empty.
    assert!(
        !legacy_dir.exists() || std::fs::read_dir(&legacy_dir).unwrap().next().is_none(),
        "legacy dir must be empty or deleted after migration"
    );

    // roots.toml must contain the root.
    let roots = crate::service::roots_registry::load_roots().unwrap();
    assert!(
        roots.iter().any(|r| r.path == project_root.path()),
        "root must be registered in roots.toml after migration"
    );

    // .gitignore must contain .trusty-search/.
    let gitignore_content =
        std::fs::read_to_string(project_root.path().join(".gitignore")).unwrap_or_default();
    assert!(
        gitignore_content.contains(".trusty-search/"),
        ".gitignore must contain .trusty-search/ after migration"
    );

    // Registry must now have colocated=true.
    let entries = load_index_registry().unwrap();
    assert_eq!(entries.len(), 1);
    assert!(
        entries[0].colocated,
        "registry must be updated to colocated=true after migration"
    );

    // File contents must have survived the move.
    assert_eq!(
        std::fs::read(colocated.join("index.redb")).unwrap(),
        b"redb-sentinel"
    );

    unsafe {
        std::env::remove_var("TRUSTY_DATA_DIR");
    }
}

/// #491 regression test: handle_migrate_storage must correctly migrate an
/// index whose `<root>/.trusty-search` is a LEGACY POINTER FILE (a small
/// text file, not a directory), even when `indexes.toml` has `colocated=true`.
///
/// This is THE regression test for issue #491. Previously the command
/// would check `entry.colocated` (which was true), report "already
/// colocated", and skip — leaving data unmigrated in app-data.
#[test]
#[serial]
fn handle_legacy_pointer_file_migrates_correctly() {
    let data_dir_tmp = tempdir().unwrap();
    unsafe {
        std::env::set_var("TRUSTY_DATA_DIR", data_dir_tmp.path());
    }

    let project_root = tempdir().unwrap();
    let index_id = "ptr-regress";

    // Create the legacy POINTER FILE at <root>/.trusty-search.
    let pointer = project_root.path().join(".trusty-search");
    std::fs::write(&pointer, b"index = \"ptr-regress\"").unwrap();
    assert!(pointer.is_file(), "pointer must be a file before migration");

    // Create real data in app-data.
    let src = data_dir().unwrap().join("indexes").join(index_id);
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("index.redb"), b"ptr-redb-data").unwrap();
    std::fs::write(src.join("hnsw.usearch"), b"ptr-hnsw-data").unwrap();

    // Write indexes.toml with colocated=true (the misleading pre-#491 state).
    save_index_registry(&[PersistedIndex {
        id: index_id.to_string(),
        root_path: project_root.path().to_path_buf(),
        colocated: true, // BUG: was true but filesystem had pointer file!
        ..Default::default()
    }])
    .unwrap();

    // Run the handler — must NOT skip due to colocated=true flag.
    handle_migrate_storage(false).unwrap();

    // The pointer FILE must be gone.
    assert!(
        !pointer.is_file(),
        "legacy pointer file must be removed by migration"
    );

    // .trusty-search must now be a DIRECTORY with the data.
    let colocated = project_root.path().join(".trusty-search");
    assert!(
        colocated.is_dir(),
        ".trusty-search must be a directory post-migration"
    );
    assert!(
        colocated.join("index.redb").exists(),
        "index.redb must be present in colocated dir"
    );
    assert_eq!(
        std::fs::read(colocated.join("index.redb")).unwrap(),
        b"ptr-redb-data",
        "index.redb content must survive migration"
    );

    unsafe {
        std::env::remove_var("TRUSTY_DATA_DIR");
    }
}

/// Why: migrating an index whose root_path no longer exists must be a
/// non-error skip rather than a panic.
#[test]
#[serial]
fn migrate_storage_skips_missing_root() {
    let data_dir_tmp = tempdir().unwrap();
    unsafe {
        std::env::set_var("TRUSTY_DATA_DIR", data_dir_tmp.path());
    }

    let nonexistent_root = PathBuf::from("/tmp/trusty-test-missing-root-12345");
    save_index_registry(&[PersistedIndex {
        id: "gone-index".to_string(),
        root_path: nonexistent_root.clone(),
        colocated: false,
        ..Default::default()
    }])
    .unwrap();

    handle_migrate_storage(false).unwrap();

    let entries = load_index_registry().unwrap();
    assert_eq!(entries.len(), 1);
    assert!(
        !entries[0].colocated,
        "missing-root index must not be marked colocated"
    );

    unsafe {
        std::env::remove_var("TRUSTY_DATA_DIR");
    }
}

/// Why: running the command on an index whose filesystem state is truly
/// colocated (dir + populated index.redb) must be a no-op.
#[test]
#[serial]
fn migrate_storage_idempotent_for_colocated() {
    let data_dir_tmp = tempdir().unwrap();
    unsafe {
        std::env::set_var("TRUSTY_DATA_DIR", data_dir_tmp.path());
    }

    let project_root = tempdir().unwrap();

    // Create a real .trusty-search/ dir with a populated index.redb.
    let colocated_dir = project_root.path().join(".trusty-search");
    std::fs::create_dir_all(&colocated_dir).unwrap();
    std::fs::write(colocated_dir.join("index.redb"), b"already-done").unwrap();

    save_index_registry(&[PersistedIndex {
        id: "col-index".to_string(),
        root_path: project_root.path().to_path_buf(),
        colocated: true,
        ..Default::default()
    }])
    .unwrap();

    handle_migrate_storage(false).unwrap();

    // File must still be intact.
    assert_eq!(
        std::fs::read(colocated_dir.join("index.redb")).unwrap(),
        b"already-done"
    );

    let entries = load_index_registry().unwrap();
    assert_eq!(entries.len(), 1);
    assert!(
        entries[0].colocated,
        "already-colocated must remain colocated"
    );

    unsafe {
        std::env::remove_var("TRUSTY_DATA_DIR");
    }
}

/// Why: --dry-run must report what WOULD happen using filesystem
/// classification, without touching any files.
#[test]
#[serial]
fn migrate_storage_dry_run_no_changes() {
    let data_dir_tmp = tempdir().unwrap();
    unsafe {
        std::env::set_var("TRUSTY_DATA_DIR", data_dir_tmp.path());
    }

    let project_root = tempdir().unwrap();
    let index_id = "dry-run-test";

    let legacy_dir = data_dir().unwrap().join("indexes").join(index_id);
    std::fs::create_dir_all(&legacy_dir).unwrap();
    std::fs::write(legacy_dir.join("index.redb"), b"dry-data").unwrap();

    save_index_registry(&[PersistedIndex {
        id: index_id.to_string(),
        root_path: project_root.path().to_path_buf(),
        colocated: false,
        ..Default::default()
    }])
    .unwrap();

    // Run dry-run — must not move any files.
    handle_migrate_storage(true).unwrap();

    // The source must still exist.
    assert!(
        legacy_dir.join("index.redb").exists(),
        "dry-run must not move source files"
    );

    // The colocated dir must NOT have been created.
    assert!(
        !project_root.path().join(".trusty-search").exists(),
        "dry-run must not create colocated dir"
    );

    // Registry must still have colocated=false.
    let entries = load_index_registry().unwrap();
    assert!(!entries[0].colocated, "dry-run must not update registry");

    unsafe {
        std::env::remove_var("TRUSTY_DATA_DIR");
    }
}

// ── #5344: cross-process registry-write integrity ────────────────────────────

/// Why: `handle_migrate_storage` used to persist by republishing the whole
/// `entries` snapshot it loaded at the very top of the function
/// (`save_index_registry(&entries)`), long before the classify/move loop —
/// which walks and copies data directories and can run for minutes — had
/// finished. A registration landing anywhere in that window (the way another
/// process's `POST /indexes` would) was silently erased by that final
/// whole-file overwrite even though the migration itself reported success.
/// Same mass-deregistration signature #4317/#5344 recorded elsewhere, arriving
/// from the `migrate storage` CLI path this time.
/// What: seeds many legacy indexes so the classify/move loop has real,
/// observable wall-clock duration, waits (condition-based, not a blind sleep)
/// for the FIRST entry's file move to land as proof the loop is underway, then
/// writes a `newcomer` entry directly to `indexes.toml` — standing in for a
/// concurrent process's registration — while ~39 more entries still remain to
/// be processed before the post-loop persist step runs. Asserts the newcomer,
/// and every legacy entry, survive.
/// Test: this test. Fails on the pre-fix commit, where `newcomer` is gone
/// because the final `save_index_registry(&entries)` republishes the stale
/// snapshot taken before `newcomer` was registered.
#[test]
#[serial]
fn migrate_storage_preserves_a_registration_made_during_the_migration_window() {
    let data_dir_tmp = tempdir().unwrap();
    unsafe {
        std::env::set_var("TRUSTY_DATA_DIR", data_dir_tmp.path());
    }

    const N: usize = 40;
    let mut project_roots: Vec<tempfile::TempDir> = Vec::with_capacity(N);
    let mut entries: Vec<PersistedIndex> = Vec::with_capacity(N);
    for i in 0..N {
        let root = tempdir().unwrap();
        let legacy_dir = data_dir()
            .unwrap()
            .join("indexes")
            .join(format!("legacy-{i}"));
        std::fs::create_dir_all(&legacy_dir).unwrap();
        std::fs::write(legacy_dir.join("index.redb"), b"sentinel").unwrap();
        entries.push(PersistedIndex {
            id: format!("legacy-{i}"),
            root_path: root.path().to_path_buf(),
            colocated: false,
            ..Default::default()
        });
        project_roots.push(root);
    }
    save_index_registry(&entries).unwrap();

    let worker = std::thread::spawn(|| {
        handle_migrate_storage(false).unwrap();
    });

    // Condition-based wait: block until the FIRST entry's data has actually
    // moved into its colocated dir — proof the classify/move loop is
    // underway, i.e. strictly after the top-level snapshot load this run
    // took. With 39 more entries still to process, the post-loop persist
    // step is comfortably still ahead.
    let first_marker = project_roots[0]
        .path()
        .join(".trusty-search")
        .join("index.redb");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !first_marker.exists() {
        assert!(
            Instant::now() < deadline,
            "migrate storage did not start moving files within the deadline"
        );
        std::thread::sleep(Duration::from_micros(200));
    }

    // Stands in for another process registering an index while migrate
    // storage is still walking/copying the remaining data directories.
    let toml_path = indexes_toml_path().unwrap();
    let mut current = load_index_registry_at(&toml_path).unwrap();
    current.push(PersistedIndex {
        id: "newcomer".to_string(),
        root_path: PathBuf::from("/tmp/migrate-storage-newcomer"),
        colocated: false,
        ..Default::default()
    });
    save_index_registry_at(&toml_path, &current).unwrap();

    worker.join().unwrap();

    let after = load_index_registry_at(&toml_path).unwrap();
    let ids: Vec<&str> = after.iter().map(|e| e.id.as_str()).collect();
    assert!(
        ids.contains(&"newcomer"),
        "a registration made during migrate storage's classify/move window \
         must survive the post-migration patch (#5344); registry now holds \
         {ids:?}"
    );
    assert_eq!(
        after.len(),
        N + 1,
        "no legacy entry may be lost either: {ids:?}"
    );

    unsafe {
        std::env::remove_var("TRUSTY_DATA_DIR");
    }
}

/// Why: fail-closed. `migrate storage`'s registry persist step must block on
/// the cross-process advisory lock the same way every other registry writer
/// does; a writer that ignores it is the #5344 defect itself. A second
/// descriptor on the sidecar is exactly the conflict another PROCESS produces.
/// What: holds the lock, runs `handle_migrate_storage` on a worker thread, and
/// proves the `colocated` flag is still unset (the persist step is blocked)
/// while the lock is held even though the file move itself — unlocked — has
/// already completed — then that the persist completes once the lock is
/// released.
/// Test: this test. On the pre-fix commit the persist step (a plain
/// `save_index_registry`) takes no lock at all, so `colocated` flips to
/// `true` before the release regardless.
#[test]
#[serial]
fn migrate_storage_blocks_on_the_cross_process_registry_lock() {
    use std::sync::mpsc;

    let data_dir_tmp = tempdir().unwrap();
    unsafe {
        std::env::set_var("TRUSTY_DATA_DIR", data_dir_tmp.path());
    }

    let project_root = tempdir().unwrap();
    let index_id = "lock-blocks";
    let legacy_dir = data_dir().unwrap().join("indexes").join(index_id);
    std::fs::create_dir_all(&legacy_dir).unwrap();
    std::fs::write(legacy_dir.join("index.redb"), b"sentinel").unwrap();
    save_index_registry(&[PersistedIndex {
        id: index_id.to_string(),
        root_path: project_root.path().to_path_buf(),
        colocated: false,
        ..Default::default()
    }])
    .unwrap();

    let toml_path = indexes_toml_path().unwrap();
    let (done_tx, done_rx) = mpsc::channel::<()>();

    let worker = trusty_common::file_lock::with_exclusive_lock(&toml_path, || {
        let worker = std::thread::spawn(move || {
            handle_migrate_storage(false).unwrap();
            let _ = done_tx.send(());
        });
        // The file move itself is unlocked, so give it ample time to finish;
        // only the registry persist afterward must still be blocked.
        std::thread::sleep(Duration::from_millis(1500));

        let colocated_dir = project_root.path().join(".trusty-search");
        assert!(
            colocated_dir.join("index.redb").exists(),
            "the unlocked file move should have completed while the lock was held"
        );

        let during = load_index_registry_at(&toml_path).unwrap();
        assert!(
            !during[0].colocated,
            "migrate storage recorded colocated=true while another process \
             held the cross-process registry lock (#5344)"
        );
        assert!(
            done_rx.try_recv().is_err(),
            "migrate storage completed while the cross-process lock was held (#5344)"
        );
        worker
    })
    .expect("lock acquisition");

    worker.join().expect("migrate storage thread");
    let after = load_index_registry_at(&toml_path).unwrap();
    assert!(
        after[0].colocated,
        "once the lock is released migrate storage must complete normally"
    );

    unsafe {
        std::env::remove_var("TRUSTY_DATA_DIR");
    }
}
