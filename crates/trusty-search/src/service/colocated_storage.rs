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

/// Ensure `.trusty-search/` is present in the `.gitignore` at `root_path`.
///
/// Why: `.trusty-search/` contains large binary files (redb, HNSW snapshots)
/// that must never be committed. Auto-adding the ignore entry prevents
/// accidental `git add -A` inclusion. The operation is idempotent — it
/// checks for the pattern before appending.
/// What: looks for `root_path/.gitignore`; if none found, creates one there.
/// Appends `GITIGNORE_LINE` when it is not already present (checking both
/// `".trusty-search/"` and `".trusty-search"` forms). Resolution is bounded
/// strictly to `root_path` — no ancestor directory is ever read or written
/// (issue #3564: an earlier version walked upward with no boundary and could
/// find/write a `.gitignore` outside the caller's intended root, e.g. the
/// shared `$TMPDIR` root under test). Failures are logged at warn level and
/// do not propagate — missing `.gitignore` coverage is not fatal.
/// Test: `gitignore_entry_added_idempotently`, `gitignore_stays_within_root_boundary`.
pub fn ensure_gitignored(root_path: &Path) -> Result<()> {
    // The boundary equals `root_path` itself: this is the intended project
    // root the caller pointed the tool at (see the two production callers,
    // `migrate_storage::migrate` and `service::server::indexes::create_index`,
    // both of which pass the user-supplied index root). Injecting the same
    // value as both the start point and the hard upper bound means the walk
    // can never escape it, in tests or production alike — there is no
    // environment-dependent branch (e.g. "does `.git` exist above here?") for
    // behavior to diverge on.
    let gitignore_path = find_or_create_gitignore(root_path, root_path)?;

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

/// Find the closest `.gitignore` at or above `root_path`, never reading or
/// creating anything above `boundary`.
///
/// Why (issue #3564): the previous version inferred its stop condition by
/// checking for a `.git` directory during the walk, falling all the way to
/// the filesystem root when none was ever found. That fallback is the bug:
/// a `tempdir()` under test (or any production root not itself inside a git
/// repo) has no `.git` anywhere above it, so the walk continued past the
/// caller's intended root — in tests, into the shared `$TMPDIR` root, reading
/// and potentially *writing* a stray `.gitignore` there instead of inside the
/// project. In production the identical code path could locate and mutate a
/// `.gitignore` outside the directory the user pointed the tool at. Inferring
/// the boundary from `.git` presence also meant the walk behaved differently
/// in tests (rarely inside a real repo) than in production (usually inside
/// one) — an environment-dependent code path is exactly how this hid for so
/// long. The fix takes `boundary` as an explicit, caller-injected parameter
/// instead: the same hard limit applies identically regardless of what `.git`
/// markers happen to exist on disk.
/// What: walks upward from `root_path` (inclusive) toward `boundary`
/// (inclusive); `boundary` must be `root_path` or one of its ancestors
/// (debug-asserted). Returns the first existing `.gitignore` found at or
/// between them. If none exists by the time the walk reaches `boundary`,
/// returns `boundary/.gitignore` as the create target (which may not yet
/// exist — the caller creates it by writing to the returned path). Never
/// returns, reads, or writes a path outside `boundary`.
/// Test: `gitignore_entry_added_idempotently` (root-level create case) and
/// `gitignore_stays_within_root_boundary` (escape regression — proves a
/// `.gitignore` outside the boundary is neither found nor mutated).
fn find_or_create_gitignore(root_path: &Path, boundary: &Path) -> Result<PathBuf> {
    debug_assert!(
        root_path.starts_with(boundary),
        "boundary must be root_path or an ancestor of root_path"
    );
    let mut current = root_path;
    loop {
        let candidate = current.join(".gitignore");
        if candidate.exists() {
            return Ok(candidate);
        }
        if current == boundary {
            return Ok(boundary.join(".gitignore"));
        }
        // `starts_with(boundary)` (asserted above) guarantees `parent()`
        // eventually equals `boundary` before ever returning `None`; the
        // `unwrap_or(boundary)` is a defensive fallback only.
        current = current.parent().unwrap_or(boundary);
    }
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
    fn gitignore_stays_within_root_boundary() {
        // Why (issue #3564): this is the exact shape of the production bug —
        // an ancestor directory (standing in for the shared `$TMPDIR` root
        // seen in the wild) has its OWN `.gitignore` and even a `.git`
        // marker, one level (in fact two) above a nested project root that
        // has neither. The pre-fix `find_or_create_gitignore` walked upward
        // with no boundary, found the ancestor `.gitignore` first, and wrote
        // the entry there — mutating a file outside the directory the caller
        // pointed the tool at, and leaving `project/.gitignore` absent. This
        // test fails against pre-fix code for exactly that reason: the
        // ancestor-untouched assertion below would fail (pre-fix, the
        // ancestor gains `GITIGNORE_LINE`) and the project-gitignore-exists
        // assertion would also fail (pre-fix, that file is never created).
        let tmp = tempdir().unwrap();
        let ancestor_gitignore = tmp.path().join(".gitignore");
        std::fs::write(&ancestor_gitignore, "target/\n").unwrap();
        std::fs::create_dir_all(tmp.path().join(".git")).unwrap();

        // Nested two levels down: `<tmp>/nested/project`. Neither
        // intermediate dir has its own `.gitignore` or `.git`.
        let project = tmp.path().join("nested").join("project");
        std::fs::create_dir_all(&project).unwrap();

        ensure_gitignored(&project).unwrap();

        // Post-fix: the entry must land INSIDE the boundary (project root).
        let project_gitignore = project.join(".gitignore");
        assert!(
            project_gitignore.exists(),
            "gitignore must be created inside the boundary root, not escape it"
        );
        let project_content = std::fs::read_to_string(&project_gitignore).unwrap();
        assert!(
            project_content.contains(GITIGNORE_LINE),
            "the created gitignore must contain the entry"
        );

        // Post-fix: the ancestor .gitignore (outside the boundary) must be
        // completely untouched — this is the assertion that fails against
        // pre-fix code (pre-fix, this file gains GITIGNORE_LINE instead).
        let ancestor_content = std::fs::read_to_string(&ancestor_gitignore).unwrap();
        assert_eq!(
            ancestor_content, "target/\n",
            "ancestor .gitignore outside the boundary must never be read or mutated"
        );
    }

    #[test]
    fn find_or_create_gitignore_never_reads_above_explicit_boundary() {
        // Why: directly exercises the private helper's boundary contract with
        // a boundary that is a strict ancestor of `root_path` (not merely
        // `root_path == boundary`, which `ensure_gitignored` always uses in
        // production). Proves the general walk-with-boundary mechanism is
        // correct on its own terms, independent of how the public API happens
        // to invoke it today.
        let tmp = tempdir().unwrap();
        let boundary = tmp.path().join("boundary");
        std::fs::create_dir_all(&boundary).unwrap();
        let nested = boundary.join("a").join("b");
        std::fs::create_dir_all(&nested).unwrap();

        // A `.gitignore` above `boundary` must never be found.
        let outside_gitignore = tmp.path().join(".gitignore");
        std::fs::write(&outside_gitignore, "should-not-be-found\n").unwrap();

        let found = find_or_create_gitignore(&nested, &boundary).unwrap();
        assert_eq!(
            found,
            boundary.join(".gitignore"),
            "with none present within the boundary, target must be boundary/.gitignore"
        );
        assert!(
            !found.exists(),
            "find_or_create_gitignore only resolves the path; it does not create the file"
        );

        // Now place a `.gitignore` at an intermediate level WITHIN the
        // boundary — it must be found instead of falling through to boundary.
        let mid_gitignore = boundary.join("a").join(".gitignore");
        std::fs::write(&mid_gitignore, "mid\n").unwrap();
        let found2 = find_or_create_gitignore(&nested, &boundary).unwrap();
        assert_eq!(
            found2, mid_gitignore,
            "an existing .gitignore within the boundary must be preferred"
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
