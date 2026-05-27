//! Project-root detection and palace-slug derivation (issue #88).
//!
//! Why: unbounded palace creation leads to orphaned namespaces that no longer
//! correspond to any project on disk. Anchoring palace names to a stable,
//! filesystem-derived slug ensures each project gets exactly one palace and
//! makes "which palace am I in?" predictable from the working directory alone.
//! The `personal` palace is the single sanctioned exception for non-project
//! contexts (global notes, one-off sessions).
//! What: `project_slug()` walks upward from CWD looking for canonical project
//! markers (`.git`, `Cargo.toml`, `pyproject.toml`, `package.json`) and
//! returns the slugified basename of the first ancestor that contains one.
//! Returns `None` when no project root is found (all ancestors have been
//! exhausted). The slug is deterministic, lowercase, filesystem-safe, and
//! under 64 chars.
//! Test: `project_slug_finds_git_root`, `project_slug_returns_none_without_markers`,
//! `project_slug_uses_first_ancestor_marker`,
//! `project_slug_personal_always_allowed`.

use anyhow::Result;
use std::path::{Path, PathBuf};

use crate::messaging::slugify_string;

/// Sentinel palace name that is always valid regardless of project context.
///
/// Why: users operating outside any project root (global notes, exploratory
/// sessions, personal task lists) need a stable palace that can receive
/// memories without failing the project-enforcement gate. The name `personal`
/// is the single reserved identifier for this purpose.
/// What: a `&str` constant that the enforcement logic tests against before
/// applying project-slug validation.
/// Test: `project_slug_personal_always_allowed`.
pub const PERSONAL_PALACE: &str = "personal";

/// File names that mark a directory as a project root.
///
/// Why: different ecosystems use different conventions for the project root;
/// we want a single, ordered list that every part of the codebase agrees on
/// so project detection is consistent whether invoked from CLI, MCP, or
/// tests. `.git` comes first because it is the most universal signal.
/// What: an ordered slice of filenames checked by `find_project_root`. A
/// directory is considered a project root when it contains *any* of these.
/// Test: `project_slug_uses_first_ancestor_marker`.
pub const PROJECT_MARKERS: &[&str] = &[
    ".git",
    "Cargo.toml",
    "pyproject.toml",
    "package.json",
    "go.mod",
    ".project-root",
];

/// Walk upward from `start` and return the first ancestor directory (inclusive)
/// that contains at least one project marker.
///
/// Why: keeping the filesystem walk in a dedicated helper makes both the slug
/// derivation function and the tests easier to reason about — callers get the
/// root path, not just the slug.
/// What: starts at `start`, checks for every [`PROJECT_MARKERS`] file/dir,
/// and ascends to `parent()` until a root is found or the filesystem root is
/// reached. Returns `None` when no project root is found.
/// Test: `project_slug_finds_git_root`, `project_slug_uses_first_ancestor_marker`.
pub fn find_project_root(start: &Path) -> Option<PathBuf> {
    let mut current = start.to_path_buf();
    // Canonicalize to resolve symlinks before walking (best-effort; fall back
    // to the original path if canonicalization fails, e.g. path does not exist
    // yet).
    if let Ok(canonical) = std::fs::canonicalize(&current) {
        current = canonical;
    }
    loop {
        for marker in PROJECT_MARKERS {
            if current.join(marker).exists() {
                return Some(current);
            }
        }
        // Ascend one level; stop at the filesystem root.
        match current.parent() {
            Some(parent) if parent != current => current = parent.to_path_buf(),
            _ => return None,
        }
    }
}

/// Derive a palace slug from the project root found at or above `start`.
///
/// Why: the core of issue #88 — palace names must match the canonical slug
/// of the project they belong to so a project's palace is unambiguously
/// discoverable from any subdirectory of that project.
/// What: calls `find_project_root`, then `slugify_string` on the basename.
/// Returns `None` when no project root is found (the caller should then fall
/// back to the `personal` palace or prompt the user to pass `--palace
/// personal`).
/// Test: `project_slug_finds_git_root`, `project_slug_returns_none_without_markers`.
pub fn project_slug_at(start: &Path) -> Option<String> {
    let root = find_project_root(start)?;
    let basename = root.file_name()?.to_str()?;
    let slug = slugify_string(basename);
    if slug.is_empty() {
        None
    } else {
        Some(slug)
    }
}

/// Derive a palace slug for the current working directory.
///
/// Why: convenience wrapper over `project_slug_at` for callers that want
/// the "natural" project slug (CLI commands, MCP handlers, tests running
/// inside a repo).
/// What: calls `std::env::current_dir()`, propagates the error if the syscall
/// fails, then delegates to [`project_slug_at`].
/// Test: `project_slug_finds_git_root` (run from inside the trusty-tools repo
/// which is a git checkout).
pub fn project_slug() -> Result<Option<String>> {
    let cwd = std::env::current_dir().map_err(|e| anyhow::anyhow!("read cwd: {e}"))?;
    Ok(project_slug_at(&cwd))
}

/// Validate a proposed palace name against project-slug enforcement rules.
///
/// Why: palace creation in MCP tool calls and HTTP handlers must apply the
/// same enforcement logic. Centralising the check here keeps the rule in one
/// place and makes it easy to write exhaustive unit tests.
/// What: returns `Ok(())` when the name is valid; returns `Err` with a
/// human-readable message when it is not. The rules are:
///   1. `personal` is always valid (the escape hatch for non-project
///      contexts).
///   2. When a project root is detectable from `cwd`, the name must equal
///      the derived slug.
///   3. When no project root is detectable, only `personal` is allowed.
///
/// Existing palaces are **not** affected by this check; it applies only to
/// *new* palace creation requests.
/// Test: `validate_palace_name_accepts_personal`,
/// `validate_palace_name_accepts_matching_slug`,
/// `validate_palace_name_rejects_mismatch`,
/// `validate_palace_name_rejects_non_personal_without_project`.
pub fn validate_palace_name(name: &str, cwd: &Path) -> Result<()> {
    // The `personal` palace is always a valid creation target.
    if name == PERSONAL_PALACE {
        return Ok(());
    }

    match project_slug_at(cwd) {
        Some(expected) => {
            if name == expected {
                Ok(())
            } else {
                Err(anyhow::anyhow!(
                    "palace name '{name}' does not match the project slug '{expected}' \
                     (derived from {cwd}). \
                     Either use '{expected}' or use 'personal' for non-project memories.",
                    cwd = cwd.display(),
                ))
            }
        }
        None => Err(anyhow::anyhow!(
            "no project root found at or above '{cwd}'. \
             Use 'personal' for memories not tied to a project, \
             or run from inside a project directory that contains \
             a .git file, Cargo.toml, pyproject.toml, or package.json.",
            cwd = cwd.display(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    // -----------------------------------------------------------------------
    // find_project_root
    // -----------------------------------------------------------------------

    /// Why: the primary use-case — a nested directory inside a git repo must
    /// resolve to the repo root, not just the immediate parent.
    /// What: create a temp dir with a `.git` subdir, nest a subdirectory
    /// inside it, and assert that `find_project_root` from the subdirectory
    /// returns the outer root (the one with `.git`).
    /// Test: itself.
    #[test]
    fn project_slug_finds_git_root() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().to_path_buf();
        // Create a .git marker at the root level.
        fs::create_dir_all(root.join(".git")).unwrap();
        // Create a nested subdirectory.
        let nested = root.join("crates").join("foo");
        fs::create_dir_all(&nested).unwrap();

        let found = find_project_root(&nested);
        assert!(found.is_some(), "should find project root");
        // Canonicalize both sides so macOS /var vs /private/var symlinks
        // do not cause false mismatches.
        let found_canonical = fs::canonicalize(found.unwrap()).unwrap();
        let root_canonical = fs::canonicalize(&root).unwrap();
        assert_eq!(found_canonical, root_canonical);
    }

    /// Why: when the CWD is not inside any project, `find_project_root` must
    /// return `None` so the caller can fall through to the `personal` palace.
    /// What: create a temp dir with *no* marker files and assert the result
    /// is `None`.
    /// Test: itself.
    #[test]
    fn project_slug_returns_none_without_markers() {
        let tmp = tempfile::tempdir().expect("tempdir");
        // Bare directory — no .git, Cargo.toml, etc.
        let found = find_project_root(tmp.path());
        assert!(
            found.is_none(),
            "bare tempdir should not resolve to a project root"
        );
    }

    /// Why: `Cargo.toml` is also a valid project marker; not every project
    /// uses git.
    /// What: create a temp dir with a `Cargo.toml` file and assert it is
    /// detected as the project root from a subdirectory.
    /// Test: itself.
    #[test]
    fn project_slug_uses_first_ancestor_marker() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().to_path_buf();
        fs::write(root.join("Cargo.toml"), "[package]").unwrap();
        let sub = root.join("src");
        fs::create_dir_all(&sub).unwrap();

        let found = find_project_root(&sub);
        assert!(found.is_some());
        // Canonicalize both sides so macOS /var vs /private/var symlinks
        // do not cause false mismatches.
        let found_canonical = fs::canonicalize(found.unwrap()).unwrap();
        let root_canonical = fs::canonicalize(&root).unwrap();
        assert_eq!(found_canonical, root_canonical);
    }

    // -----------------------------------------------------------------------
    // project_slug_at
    // -----------------------------------------------------------------------

    /// Why: the slug must be the slugified basename of the project root, not
    /// the subdirectory we started from.
    /// What: create a root named `my-project` with a `.git` marker; start
    /// from a nested subdirectory; assert the slug is `my-project`.
    /// Test: itself.
    #[test]
    fn project_slug_at_returns_root_basename_slug() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("my-project");
        fs::create_dir_all(root.join(".git")).unwrap();
        let src = root.join("src");
        fs::create_dir_all(&src).unwrap();

        let slug = project_slug_at(&src).expect("should return slug");
        assert_eq!(slug, "my-project");
    }

    /// Why: uppercase and underscores must be normalised by the slug derivation
    /// so that `My_Project` and `my-project` resolve to the same palace.
    /// What: create a root named `My_Project`; assert the derived slug is
    /// `my-project`.
    /// Test: itself.
    #[test]
    fn project_slug_at_normalises_case_and_underscores() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("My_Project");
        fs::create_dir_all(root.join(".git")).unwrap();

        let slug = project_slug_at(&root).expect("should return slug");
        assert_eq!(slug, "my-project");
    }

    /// Why: when no project root is found, `project_slug_at` must return
    /// `None` so the caller knows to use `personal`.
    /// Test: itself.
    #[test]
    fn project_slug_at_returns_none_without_markers() {
        let tmp = tempfile::tempdir().expect("tempdir");
        assert!(project_slug_at(tmp.path()).is_none());
    }

    // -----------------------------------------------------------------------
    // validate_palace_name
    // -----------------------------------------------------------------------

    /// Why: `personal` is the sanctioned escape hatch; it must always be
    /// accepted regardless of whether a project root is found.
    /// What: run `validate_palace_name("personal", …)` from a plain temp
    /// dir (no project markers); assert `Ok(())`.
    /// Test: itself.
    #[test]
    fn validate_palace_name_accepts_personal() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let result = validate_palace_name(PERSONAL_PALACE, tmp.path());
        assert!(
            result.is_ok(),
            "personal must always be accepted; got {result:?}"
        );
    }

    /// Why: when the name exactly matches the derived slug the creation must
    /// succeed.
    /// What: create a project root named `cool-app`; assert that
    /// `validate_palace_name("cool-app", subdir)` returns `Ok(())`.
    /// Test: itself.
    #[test]
    fn validate_palace_name_accepts_matching_slug() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("cool-app");
        fs::create_dir_all(root.join(".git")).unwrap();
        let sub = root.join("src");
        fs::create_dir_all(&sub).unwrap();

        let result = validate_palace_name("cool-app", &sub);
        assert!(result.is_ok(), "matching slug must be accepted: {result:?}");
    }

    /// Why: a mismatched name must be rejected with an actionable error that
    /// tells the user which slug is expected.
    /// What: create a project root named `cool-app`; assert that
    /// `validate_palace_name("wrong-name", subdir)` returns `Err` and the
    /// error message mentions `cool-app`.
    /// Test: itself.
    #[test]
    fn validate_palace_name_rejects_mismatch() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("cool-app");
        fs::create_dir_all(root.join(".git")).unwrap();
        let sub = root.join("src");
        fs::create_dir_all(&sub).unwrap();

        let result = validate_palace_name("wrong-name", &sub);
        assert!(result.is_err(), "mismatched name must be rejected");
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("cool-app"),
            "error must mention the expected slug; got: {msg}"
        );
    }

    /// Why: outside a project directory, only `personal` is allowed; any
    /// other name must be rejected.
    /// What: use a plain tempdir (no markers); assert that any non-`personal`
    /// name returns `Err`.
    /// Test: itself.
    #[test]
    fn validate_palace_name_rejects_non_personal_without_project() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let result = validate_palace_name("my-notes", tmp.path());
        assert!(
            result.is_err(),
            "non-personal name outside a project must be rejected"
        );
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("personal"),
            "error must mention 'personal'; got: {msg}"
        );
    }
}
