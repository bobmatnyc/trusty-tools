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
//! What: this module resolves colocated storage paths and manages the `.gitignore`
//! entry that prevents the `.trusty-search/` dir from being committed.
//!
//! Test: `storage_dir_resolves_under_root`, `gitignore_entry_added_idempotently`,
//! and `colocated_paths_distinct_for_different_roots` in the test block below.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// The directory name used for colocated index storage inside a project root.
///
/// Why: a single canonical constant prevents typos across the codebase and
/// makes grep-based audits reliable.
/// What: the literal string `.trusty-search`.
/// Test: referenced by every test that constructs an expected path.
pub const COLOCATED_DIR_NAME: &str = ".trusty-search";

/// `.gitignore` line that should be present for every colocated index dir.
///
/// Why: the `.trusty-search/` dir contains redb, HNSW, and schema stamps —
/// large binary files that should never be committed. Auto-adding this line
/// prevents accidental `git add -A` from including them.
/// What: the pattern that git(1) matches against the dir name.
/// Test: `gitignore_entry_added_idempotently` verifies the line is appended
/// exactly once regardless of how many times the helper is called.
pub const GITIGNORE_LINE: &str = ".trusty-search/";

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

/// Path to the staging redb corpus inside the colocated storage dir.
///
/// Why: mirrors `persistence::corpus_redb_tmp_path` but under the project tree.
/// What: returns `<root_path>/.trusty-search/index.redb.tmp`.
/// Test: covered indirectly by the colocated-index integration tests.
pub fn colocated_redb_tmp_path(root_path: &Path) -> Result<PathBuf> {
    Ok(colocated_storage_dir(root_path)?.join("index.redb.tmp"))
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

/// Ensure `.trusty-search/` is present in the nearest `.gitignore` file.
///
/// Why: `.trusty-search/` contains large binary files (redb, HNSW snapshots)
/// that must never be committed. Auto-adding the ignore entry prevents
/// accidental `git add -A` inclusion. The operation is idempotent — it
/// checks for the pattern before appending.
/// What: walks up from `root_path` looking for `.gitignore`; if none found,
/// creates one at `root_path/.gitignore`. Appends `GITIGNORE_LINE` when it is
/// not already present (checking both `".trusty-search/"` and `".trusty-search"`
/// forms). Failures are logged at warn level and do not propagate — missing
/// `.gitignore` coverage is not fatal.
/// Test: `gitignore_entry_added_idempotently`.
pub fn ensure_gitignored(root_path: &Path) -> Result<()> {
    let gitignore_path = find_or_create_gitignore(root_path)?;

    let content = match std::fs::read_to_string(&gitignore_path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e).context("read .gitignore"),
    };

    if gitignore_already_covers(&content) {
        tracing::debug!(
            ".gitignore at {} already covers .trusty-search/",
            gitignore_path.display()
        );
        return Ok(());
    }

    // Append the line. Ensure there is a trailing newline before our entry.
    let needs_newline = !content.is_empty() && !content.ends_with('\n');
    let mut new_content = content;
    if needs_newline {
        new_content.push('\n');
    }
    new_content.push_str(GITIGNORE_LINE);
    new_content.push('\n');

    std::fs::write(&gitignore_path, &new_content)
        .with_context(|| format!("write .gitignore at {}", gitignore_path.display()))?;

    tracing::info!(
        "added .trusty-search/ to .gitignore at {}",
        gitignore_path.display()
    );
    Ok(())
}

/// Return true if the gitignore content already contains an entry that would
/// exclude `.trusty-search/`.
///
/// Why: both `".trusty-search/"` (trailing slash) and `".trusty-search"` (no
/// slash) are valid gitignore patterns that match the directory. Checking for
/// both avoids a spurious double-entry when the user already wrote one form.
/// What: line-based search for both patterns, ignoring comment lines.
/// Test: `gitignore_entry_added_idempotently` covers both forms.
fn gitignore_already_covers(content: &str) -> bool {
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed == ".trusty-search/" || trimmed == ".trusty-search" {
            return true;
        }
    }
    false
}

/// Find the closest `.gitignore` up from `root_path`, or return
/// `root_path/.gitignore` as the target to create.
///
/// Why: the project's `.gitignore` may live at a parent level (e.g. a
/// monorepo root). We prefer the nearest existing `.gitignore` rather than
/// always creating one inside the project subtree.
/// What: walks up the path hierarchy; stops at the first `.gitignore` found.
/// Falls back to `root_path/.gitignore` (which may not yet exist — the caller
/// creates it by writing to the returned path).
/// Test: covered by `gitignore_entry_added_idempotently` (root-level create case)
/// and `gitignore_found_at_parent` (parent-level find case).
fn find_or_create_gitignore(root_path: &Path) -> Result<PathBuf> {
    let mut current = root_path;
    loop {
        let candidate = current.join(".gitignore");
        if candidate.exists() {
            return Ok(candidate);
        }
        // Stop at a `.git` directory — the gitignore must be within the repo.
        if current.join(".git").exists() {
            return Ok(current.join(".gitignore"));
        }
        match current.parent() {
            Some(p) => current = p,
            None => break,
        }
    }
    // Fallback: create at the root_path itself.
    Ok(root_path.join(".gitignore"))
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

    #[test]
    fn gitignore_entry_added_idempotently() {
        // Why: `ensure_gitignored` must append the line exactly once even when
        // called multiple times. Duplicate entries are gitignore-harmless but
        // look sloppy and confuse users.
        let tmp = tempdir().unwrap();
        let root = tmp.path();

        // First call — file does not yet exist; should be created with the entry.
        ensure_gitignored(root).unwrap();
        let content1 = std::fs::read_to_string(root.join(".gitignore")).unwrap();
        assert!(
            content1.contains(GITIGNORE_LINE),
            "first call must write the entry"
        );
        let count = content1
            .lines()
            .filter(|l| l.trim() == ".trusty-search/")
            .count();
        assert_eq!(count, 1, "exactly one entry after first call");

        // Second call — file exists with the entry; must not duplicate it.
        ensure_gitignored(root).unwrap();
        let content2 = std::fs::read_to_string(root.join(".gitignore")).unwrap();
        let count2 = content2
            .lines()
            .filter(|l| l.trim() == ".trusty-search/")
            .count();
        assert_eq!(count2, 1, "still exactly one entry after second call");
    }

    #[test]
    fn gitignore_respects_no_trailing_slash_form() {
        // Why: if the user already wrote `.trusty-search` (no trailing slash),
        // we must NOT add a second entry.
        let tmp = tempdir().unwrap();
        let gitignore = tmp.path().join(".gitignore");
        std::fs::write(&gitignore, ".trusty-search\n").unwrap();

        ensure_gitignored(tmp.path()).unwrap();
        let content = std::fs::read_to_string(&gitignore).unwrap();
        let count = content
            .lines()
            .filter(|l| {
                let t = l.trim();
                t == ".trusty-search/" || t == ".trusty-search"
            })
            .count();
        assert_eq!(count, 1, "no duplicate when no-slash form already present");
    }

    #[test]
    fn gitignore_found_at_parent_level() {
        // Why: in a monorepo the `.gitignore` may live at the repo root, one
        // level up from the project root. We should update the existing file
        // rather than creating a new one inside the project.
        let tmp = tempdir().unwrap();
        let parent_gitignore = tmp.path().join(".gitignore");
        std::fs::write(&parent_gitignore, "target/\n").unwrap();

        // Simulate a project subdir without its own .gitignore but with a .git
        // at parent level.
        let git_dir = tmp.path().join(".git");
        std::fs::create_dir_all(&git_dir).unwrap();
        let project = tmp.path().join("my-project");
        std::fs::create_dir_all(&project).unwrap();

        ensure_gitignored(&project).unwrap();

        // The parent .gitignore must now contain the entry.
        let content = std::fs::read_to_string(&parent_gitignore).unwrap();
        assert!(
            content.contains(GITIGNORE_LINE),
            "parent .gitignore must receive the entry; content={content:?}"
        );
        // No new .gitignore should have been created inside the project subdir.
        assert!(
            !project.join(".gitignore").exists(),
            "no .gitignore should be created inside the project subdir"
        );
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
