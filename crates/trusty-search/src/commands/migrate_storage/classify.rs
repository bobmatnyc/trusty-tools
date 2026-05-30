//! Filesystem-based classification of index migration state.
//!
//! Why: the old migrate-storage command decided whether an index was
//! "already colocated" by checking the `colocated` flag in `indexes.toml`.
//! That flag could be set to `true` even when the actual filesystem state
//! diverged — e.g. after a prior migration that silently failed because
//! `<root>/.trusty-search` was a legacy POINTER FILE (a small text file
//! like `index = "itinerator"`) that blocked `mkdir`. Issue #491.
//!
//! What: defines [`IndexMigrationClass`] and [`classify_index`], which
//! inspect real filesystem paths (not the registry flag) to determine the
//! true migration state of each index.
//!
//! Test: `classify::tests` in this file covers each variant with tempdir
//! fixtures. The LegacyPointerFile case is the regression test for #491.

use std::path::{Path, PathBuf};

use crate::service::colocated_storage::COLOCATED_DIR_NAME;
use crate::service::persistence::data_dir;

/// Filesystem-based classification of one index's migration state.
///
/// Why: replaces the old boolean `colocated` flag check, which produced
/// false "already colocated" reports when the filesystem diverged from
/// the registry (issue #491).
/// What: each variant describes what we found on disk, independent of
/// what `indexes.toml` says.
/// Test: each variant is exercised by a dedicated test in
/// `classify::tests`.
#[derive(Debug, PartialEq, Eq, Clone)]
pub enum IndexMigrationClass {
    /// `<root>/.trusty-search/` is a populated directory containing a
    /// non-empty `index.redb`. Truly done — no action needed.
    AlreadyColocated,

    /// `index.redb` exists and is non-empty in the app-data dir, AND the
    /// colocated dir is either absent or an empty directory.
    /// Action: create the dir (if absent) and move data files.
    NeedsMigration {
        /// Legacy app-data source directory.
        src_dir: PathBuf,
        /// Target `<root>/.trusty-search/` path (may not yet exist).
        dst_dir: PathBuf,
    },

    /// `<root>/.trusty-search` exists as a FILE (the old pointer-file
    /// format, e.g. `index = "itinerator"`) instead of a directory. Data
    /// still lives in the app-data dir. The file must be removed before
    /// the directory can be created.
    /// Action: remove the pointer file, create the dir, move data files.
    LegacyPointerFile {
        /// Path to the pointer file that must be deleted.
        pointer_path: PathBuf,
        /// Legacy app-data source directory.
        src_dir: PathBuf,
        /// Target `<root>/.trusty-search/` path (after pointer removal).
        dst_dir: PathBuf,
    },

    /// `root_path` does not exist on disk. The project may have been
    /// deleted or is on an unmounted volume. Skip silently.
    SkipDeadRoot,

    /// Neither the colocated dir nor the app-data dir has a populated
    /// `index.redb`. Nothing to migrate; skip.
    SkipNoData,
}

/// Probe filesystem paths to classify one registered index.
///
/// Why: the classification must be driven by actual filesystem state, not
/// the `colocated` flag in `indexes.toml`, which may be stale after a
/// partial migration. See issue #491.
///
/// What: checks (in order):
///   1. `root_path` existence — returns `SkipDeadRoot` if absent.
///   2. `<root>/.trusty-search/` being a dir with a populated `index.redb`
///      → `AlreadyColocated`.
///   3. `<root>/.trusty-search` being a FILE (legacy pointer) → `LegacyPointerFile`.
///   4. App-data `<data_dir>/indexes/<id>/index.redb` populated →
///      `NeedsMigration`.
///   5. No populated data found anywhere → `SkipNoData`.
///
/// The `colocated` registry flag is intentionally ignored — it serves
/// only as a hint in the caller after classification succeeds.
///
/// Test: `classify::tests::*` in this file.
pub fn classify_index(index_id: &str, root_path: &Path) -> IndexMigrationClass {
    // 1. Dead root — can't do anything without it.
    if !root_path.exists() || !root_path.is_dir() {
        return IndexMigrationClass::SkipDeadRoot;
    }

    let colocated_candidate = root_path.join(COLOCATED_DIR_NAME);
    let dst_dir = colocated_candidate.clone();

    // 2. Already colocated: dir exists AND has a non-empty index.redb.
    if colocated_candidate.is_dir() {
        let redb_in_colocated = colocated_candidate.join("index.redb");
        if is_populated_file(&redb_in_colocated) {
            return IndexMigrationClass::AlreadyColocated;
        }
        // Dir exists but is empty / has no valid redb — fall through to check
        // app-data. We treat this as NeedsMigration (may be a partial write).
    } else if colocated_candidate.exists() {
        // 3. Legacy pointer FILE (not a directory).
        // Resolve the app-data src dir; if we can't (e.g. data_dir() fails)
        // we fall through to SkipNoData with the info we have.
        if let Ok(src_dir) = app_data_index_dir(index_id) {
            return IndexMigrationClass::LegacyPointerFile {
                pointer_path: colocated_candidate,
                src_dir,
                dst_dir,
            };
        }
        // data_dir() failed — treat as SkipNoData.
        return IndexMigrationClass::SkipNoData;
    }

    // 4. App-data has a populated index.redb → NeedsMigration.
    if let Ok(src_dir) = app_data_index_dir(index_id) {
        let redb_in_src = src_dir.join("index.redb");
        if is_populated_file(&redb_in_src) {
            return IndexMigrationClass::NeedsMigration { src_dir, dst_dir };
        }
    }

    // 5. Nothing to migrate.
    IndexMigrationClass::SkipNoData
}

/// Resolve the legacy app-data index directory for the given id.
///
/// Why: isolated so tests can verify directory resolution without
/// triggering the `create_dir_all` side-effect of `index_data_dir`.
/// What: returns `<data_dir>/indexes/<sanitized_id>` without creating it.
/// Test: indirectly via `classify_index` in `classify::tests`.
pub(super) fn app_data_index_dir(index_id: &str) -> anyhow::Result<PathBuf> {
    // Inline the sanitize logic from persistence.rs to avoid a create_dir_all.
    let sanitized: String = index_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    Ok(data_dir()?.join("indexes").join(sanitized))
}

/// True iff `path` points to a regular file with non-zero size.
///
/// Why: an empty or missing `index.redb` means the index was never written
/// or was cleared — we should not report AlreadyColocated for it.
/// What: calls `std::fs::metadata` and checks `is_file() && len > 0`.
/// Test: used in all classifier branches; exercised by classify::tests.
pub(super) fn is_populated_file(path: &Path) -> bool {
    match std::fs::metadata(path) {
        Ok(m) => m.is_file() && m.len() > 0,
        Err(_) => false,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::service::persistence::data_dir;
    use serial_test::serial;
    use tempfile::tempdir;

    /// Why: AlreadyColocated must be returned when `.trusty-search/` is a
    /// dir containing a populated `index.redb`, regardless of what
    /// `indexes.toml` says.
    /// Test: `classify_already_colocated`.
    #[test]
    #[serial]
    fn classify_already_colocated() {
        let data_tmp = tempdir().unwrap();
        unsafe { std::env::set_var("TRUSTY_DATA_DIR", data_tmp.path()) };

        let root = tempdir().unwrap();
        let ts_dir = root.path().join(".trusty-search");
        std::fs::create_dir_all(&ts_dir).unwrap();
        std::fs::write(ts_dir.join("index.redb"), b"notempty").unwrap();

        let result = classify_index("test-idx", root.path());
        assert_eq!(result, IndexMigrationClass::AlreadyColocated);

        unsafe { std::env::remove_var("TRUSTY_DATA_DIR") };
    }

    /// Why: when app-data has a populated `index.redb` and the colocated dir
    /// is absent, classify_index must return NeedsMigration.
    /// Test: `classify_needs_migration_no_colocated_dir`.
    #[test]
    #[serial]
    fn classify_needs_migration_no_colocated_dir() {
        let data_tmp = tempdir().unwrap();
        unsafe { std::env::set_var("TRUSTY_DATA_DIR", data_tmp.path()) };

        let root = tempdir().unwrap();

        // Create populated index.redb in app-data.
        let src = data_dir().unwrap().join("indexes").join("myidx");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("index.redb"), b"data").unwrap();

        let result = classify_index("myidx", root.path());
        match result {
            IndexMigrationClass::NeedsMigration { src_dir, dst_dir } => {
                assert!(src_dir.ends_with("indexes/myidx"));
                assert!(dst_dir.ends_with(".trusty-search"));
            }
            other => panic!("expected NeedsMigration, got {other:?}"),
        }

        unsafe { std::env::remove_var("TRUSTY_DATA_DIR") };
    }

    /// #491 regression: when `<root>/.trusty-search` is a FILE (legacy
    /// pointer format) and the real data is in app-data, classify_index
    /// must return LegacyPointerFile — NOT AlreadyColocated.
    ///
    /// This was the case that caused all 15 user indexes to be falsely
    /// reported as "already colocated" and skipped.
    #[test]
    #[serial]
    fn classify_legacy_pointer_file_is_not_already_colocated() {
        let data_tmp = tempdir().unwrap();
        unsafe { std::env::set_var("TRUSTY_DATA_DIR", data_tmp.path()) };

        let root = tempdir().unwrap();

        // Create the legacy pointer FILE at <root>/.trusty-search.
        let pointer = root.path().join(".trusty-search");
        std::fs::write(&pointer, b"index = \"my-project\"").unwrap();
        assert!(pointer.exists() && pointer.is_file());

        // Create populated index.redb in app-data.
        let src = data_dir().unwrap().join("indexes").join("ptr-test");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("index.redb"), b"data").unwrap();

        let result = classify_index("ptr-test", root.path());
        match &result {
            IndexMigrationClass::LegacyPointerFile {
                pointer_path,
                src_dir,
                dst_dir,
            } => {
                assert_eq!(pointer_path, &pointer);
                assert!(src_dir.ends_with("indexes/ptr-test"));
                assert!(dst_dir.ends_with(".trusty-search"));
            }
            other => panic!("expected LegacyPointerFile (not AlreadyColocated), got {other:?}"),
        }

        unsafe { std::env::remove_var("TRUSTY_DATA_DIR") };
    }

    /// Why: a non-existent root_path must produce SkipDeadRoot so the
    /// migration loop doesn't abort on deleted/unmounted projects.
    /// Test: `classify_dead_root`.
    #[test]
    #[serial]
    fn classify_dead_root() {
        let data_tmp = tempdir().unwrap();
        unsafe { std::env::set_var("TRUSTY_DATA_DIR", data_tmp.path()) };

        let missing = PathBuf::from("/tmp/trusty-classify-dead-root-12345");
        let result = classify_index("ghost", &missing);
        assert_eq!(result, IndexMigrationClass::SkipDeadRoot);

        unsafe { std::env::remove_var("TRUSTY_DATA_DIR") };
    }

    /// Why: when neither the colocated dir nor app-data has a populated
    /// index.redb, classify_index must return SkipNoData.
    /// Test: `classify_no_data`.
    #[test]
    #[serial]
    fn classify_no_data() {
        let data_tmp = tempdir().unwrap();
        unsafe { std::env::set_var("TRUSTY_DATA_DIR", data_tmp.path()) };

        let root = tempdir().unwrap();
        // No .trusty-search dir or file, no app-data index.redb.
        let result = classify_index("empty-idx", root.path());
        assert_eq!(result, IndexMigrationClass::SkipNoData);

        unsafe { std::env::remove_var("TRUSTY_DATA_DIR") };
    }
}
