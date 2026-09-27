//! Per-project colocated storage: `<root_path>/.trusty-search/` layout.
//!
//! Why: The legacy layout places all index data under a single platform data
//! directory (`<data_dir>/indexes/<id>/`), which means index data is separated
//! from the project it indexes. This causes friction with git worktrees (two
//! worktrees of the same repo share a physical path but are at different
//! filesystem paths; they should have independent indexes) and makes relocating
//! a project tree break its index. Issue #403 moves each index's on-disk data
//! INSIDE the project tree at `<root_path>/.trusty-search/`, so:
//!
//! - Two git worktrees at different paths have independent `.trusty-search/` dirs.
//! - Moving the project directory can be handled by a `migrate storage` step.
//! - The index is co-located with the code it indexes for easy `find`/cleanup.
//!
//! #8499: a directory inside the work tree is deleted by `git clean -fdx`, so
//! new registrations no longer use this layout — they keep their store in the
//! data dir, outside the work tree. This layout remains for indexes that
//! already have (or were shipped with) a `.trusty-search/` artifact.
//!
//! What: this module resolves colocated storage paths and writes the
//! self-contained `.trusty-search/.gitignore` that hides the directory from
//! git. It never reads or writes the repository's own `.gitignore`.
//!
//! Test: `storage_dir_resolves_under_root`,
//! `self_ignore_is_created_once_and_never_rewritten`, and
//! `colocated_paths_distinct_for_different_roots` in the test block below.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// The directory name used for colocated index storage inside a project root.
///
/// Why: a single canonical constant prevents typos across the codebase and
/// makes grep-based audits reliable.
/// What: the literal string `.trusty-search`.
/// Test: referenced by every test that constructs an expected path.
pub const COLOCATED_DIR_NAME: &str = ".trusty-search";

/// Resolve the colocated storage directory for a given project root.
///
/// Why: centralise the path formula (`<root>/.trusty-search/`) so every
/// caller (persistence, migration, tests) agrees on the same location.
/// What: returns `<root_path>/.trusty-search/` after calling
/// `create_dir_all` to ensure it exists.
/// Test: `storage_dir_resolves_under_root`.
pub fn colocated_storage_dir(root_path: &Path) -> Result<PathBuf> {
    let dir = root_path.join(COLOCATED_DIR_NAME);
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("create colocated storage dir at {}", dir.display()))?;
    Ok(dir)
}

/// Path to the HNSW snapshot inside the colocated storage dir.
///
/// Why: mirrors `persistence::hnsw_path` but rooted at `<root>/.trusty-search/`
/// instead of the global data dir.
/// What: returns `<root_path>/.trusty-search/hnsw.usearch` (creating the dir
/// if needed).
/// Test: covered indirectly by the colocated-index integration tests.
pub fn colocated_hnsw_path(root_path: &Path) -> Result<PathBuf> {
    Ok(colocated_storage_dir(root_path)?.join("hnsw.usearch"))
}

/// Path to the redb corpus inside the colocated storage dir.
///
/// Why: mirrors `persistence::corpus_redb_path` but under the project tree.
/// What: returns `<root_path>/.trusty-search/index.redb`.
/// Test: covered indirectly by the colocated-index integration tests.
pub fn colocated_redb_path(root_path: &Path) -> Result<PathBuf> {
    Ok(colocated_storage_dir(root_path)?.join("index.redb"))
}

/// Path to the schema-version stamp file inside the colocated storage dir.
///
/// Why: mirrors `persistence::schema_version_path` but under the project tree.
/// What: returns `<root_path>/.trusty-search/schema_version.json`.
/// Test: covered indirectly by the colocated-index integration tests.
pub fn colocated_schema_version_path(root_path: &Path) -> Result<PathBuf> {
    Ok(colocated_storage_dir(root_path)?.join("schema_version.json"))
}

/// Path to the legacy `chunks.json` snapshot inside the colocated storage dir
/// (#8134).
///
/// Why: the `chunks.json → index.redb` migration resolved its source through
/// `persistence::chunks_path`, which only ever names the GLOBAL data dir. A
/// colocated artifact keeps its snapshot under the project tree, so the
/// migration read a location that held nothing, called that a first boot,
/// and let the runner stamp the schema as migrated — after which the populated
/// snapshot beside `index.redb` was never read again.
/// What: returns `<root_path>/.trusty-search/chunks.json`. Unlike its siblings
/// above this does NOT create the directory: it is a probe for an artifact a
/// legacy build wrote, and a probe must never manufacture the tree it looks in.
/// Test: `colocated_chunks_path_is_a_probe_and_creates_nothing`.
pub fn colocated_chunks_path(root_path: &Path) -> PathBuf {
    root_path.join(COLOCATED_DIR_NAME).join("chunks.json")
}

/// Path to the staging redb corpus inside the colocated storage dir.
///
/// Why: mirrors `persistence::corpus_redb_tmp_path` but under the project tree.
/// What: returns `<root_path>/.trusty-search/index.redb.tmp`.
/// Test: covered indirectly by the colocated-index integration tests.
pub fn colocated_redb_tmp_path(root_path: &Path) -> Result<PathBuf> {
    Ok(colocated_storage_dir(root_path)?.join("index.redb.tmp"))
}

/// Path to the staging HNSW snapshot inside the colocated storage dir
/// (issue #3970).
///
/// Why: mirrors `persistence::hnsw_staging_path` but under the project tree —
/// see that function's doc for the full staged-write-then-swap rationale.
/// What: returns `<root_path>/.trusty-search/hnsw.reindex-staging.usearch`.
/// Test: covered indirectly by the colocated-index integration tests.
pub fn colocated_hnsw_staging_path(root_path: &Path) -> Result<PathBuf> {
    Ok(colocated_storage_dir(root_path)?.join("hnsw.reindex-staging.usearch"))
}

/// True iff a colocated storage dir exists for the given root.
///
/// Why: used by the discovery scanner to identify project roots that have
/// a `.trusty-search/` directory without triggering `create_dir_all`.
/// What: returns `<root_path>/.trusty-search/.exists() && is_dir()`.
/// Test: `colocated_dir_exists_after_create`.
pub fn has_colocated_storage(root_path: &Path) -> bool {
    let dir = root_path.join(COLOCATED_DIR_NAME);
    dir.exists() && dir.is_dir()
}

/// Name of the git exclude file trusty-search keeps inside its own directory.
pub const SELF_IGNORE_FILE: &str = ".gitignore";

/// Content of [`SELF_IGNORE_FILE`]: ignore everything, the file included.
pub const SELF_IGNORE_CONTENT: &str = "*\n";

/// Hide a colocated storage directory from git without touching a tracked file.
///
/// Why (#8499): the daemon used to append `.trusty-search/` to the repo's own
/// tracked `.gitignore` and leave the edit uncommitted. `git reset --hard`
/// reverted it, and `git clean -fd` then deleted the index under a live mmap.
/// A `.gitignore` inside the directory is untracked by design: `reset --hard`
/// and `checkout` never touch it, `git status` stops reporting the directory,
/// and `git clean -fd` (no `-x`) leaves it in place. `git clean -fdx` still
/// removes it, which is why new indexes live outside the work tree
/// ([`crate::service::storage_layout::StorageLayout::for_new_registration`]).
/// What: creates `<dir>/.gitignore` holding `*` with `create_new`, so an
/// existing file — whatever it holds — is never read or rewritten. Nothing
/// outside `dir` is touched.
/// Test: `self_ignore_is_created_once_and_never_rewritten`.
pub fn ensure_self_ignored(dir: &Path) -> Result<()> {
    let path = dir.join(SELF_IGNORE_FILE);
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
    {
        Ok(mut file) => {
            use std::io::Write as _;
            file.write_all(SELF_IGNORE_CONTENT.as_bytes())
                .with_context(|| format!("write {}", path.display()))
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(e).with_context(|| format!("create {}", path.display())),
    }
}

/// Former `.gitignore` line; kept only so the public API does not break.
#[deprecated(note = "#8499: trusty-search no longer edits the repository's .gitignore")]
pub const GITIGNORE_LINE: &str = ".trusty-search/";

/// Hide `<root_path>/.trusty-search/` from git, if it exists.
///
/// Why (#8499): this used to append `.trusty-search/` to the repository's own
/// tracked `.gitignore`, leaving an uncommitted edit that `git reset --hard`
/// reverted. It is kept for API compatibility and now edits nothing the
/// repository tracks.
/// What: delegates to [`ensure_self_ignored`] when the colocated directory
/// exists; otherwise does nothing. The root `.gitignore` is never read or
/// written, and the directory is never created.
/// Test: `deprecated_ensure_gitignored_never_touches_the_root_gitignore`.
#[deprecated(note = "#8499: use the data-dir layout; the colocated dir hides itself")]
pub fn ensure_gitignored(root_path: &Path) -> Result<()> {
    let dir = root_path.join(COLOCATED_DIR_NAME);
    if dir.is_dir() {
        ensure_self_ignored(&dir)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn storage_dir_resolves_under_root() {
        // Why: the colocated dir must land inside the project root, not the
        // global data dir, so two worktrees at different paths have independent
        // storage.
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        let dir = colocated_storage_dir(root).unwrap();
        assert!(dir.starts_with(root), "dir must be inside root");
        assert_eq!(dir.file_name().unwrap(), ".trusty-search");
        assert!(dir.exists() && dir.is_dir());
    }

    /// Why (#8134): the migration probes this path to decide whether a legacy
    /// snapshot exists. A probe that created `.trusty-search/` would leave the
    /// directory behind on every index that never had one.
    /// What: asserts the path is under the root and that nothing was created.
    /// Test: this test.
    #[test]
    fn colocated_chunks_path_is_a_probe_and_creates_nothing() {
        let tmp = tempdir().unwrap();
        let root = tmp.path().join("never-touched");
        let path = colocated_chunks_path(&root);
        assert_eq!(path, root.join(".trusty-search").join("chunks.json"));
        assert!(
            !root.exists(),
            "probing for a legacy snapshot must not create the root or its storage dir"
        );
    }

    #[test]
    fn colocated_paths_distinct_for_different_roots() {
        // Why: two projects at different paths must have INDEPENDENT storage;
        // this regression test guards against accidentally sharing a global dir.
        let tmp1 = tempdir().unwrap();
        let tmp2 = tempdir().unwrap();
        let path1 = colocated_redb_path(tmp1.path()).unwrap();
        let path2 = colocated_redb_path(tmp2.path()).unwrap();
        assert_ne!(path1, path2, "colocated paths must differ per root");
        assert!(path1.starts_with(tmp1.path()));
        assert!(path2.starts_with(tmp2.path()));
    }

    /// Why (#8499): the self-ignore file must be created when absent and never
    /// rewritten when present — an existing file may be the user's.
    /// Test: this test.
    #[test]
    fn self_ignore_is_created_once_and_never_rewritten() {
        let tmp = tempdir().unwrap();
        ensure_self_ignored(tmp.path()).unwrap();
        let path = tmp.path().join(SELF_IGNORE_FILE);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), SELF_IGNORE_CONTENT);

        std::fs::write(&path, "user content\n").unwrap();
        ensure_self_ignored(tmp.path()).unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "user content\n",
            "an existing file must be left byte for byte"
        );
    }

    /// Why (#8499): the compatibility shim must never edit the root
    /// `.gitignore` nor create the colocated directory.
    /// Test: this test.
    #[test]
    #[allow(deprecated)]
    fn deprecated_ensure_gitignored_never_touches_the_root_gitignore() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(root.join(".gitignore"), "target/").unwrap();
        ensure_gitignored(root).unwrap();
        assert!(!has_colocated_storage(root), "must not create the dir");
        colocated_storage_dir(root).unwrap();
        ensure_gitignored(root).unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join(".gitignore")).unwrap(),
            "target/"
        );
        let inner = root.join(COLOCATED_DIR_NAME).join(SELF_IGNORE_FILE);
        assert_eq!(std::fs::read_to_string(inner).unwrap(), SELF_IGNORE_CONTENT);
    }

    #[test]
    fn has_colocated_storage_reflects_dir_presence() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        assert!(
            !has_colocated_storage(root),
            "should be false before create"
        );
        colocated_storage_dir(root).unwrap();
        assert!(has_colocated_storage(root), "should be true after create");
    }
}
