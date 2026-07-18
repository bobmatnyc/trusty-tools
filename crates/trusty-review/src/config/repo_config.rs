//! Repo-scoped `.trusty-review.toml` discovery (issue #2995).
//!
//! Why: voice-package and review-template selection was previously global-only
//! (env var or the fixed XDG config path) — there was no way for a project to
//! commit its own review standards alongside its source so every contributor
//! (and CI) automatically picks them up.  This module locates an optional
//! `.trusty-review.toml` at the git root of the code under review, using the
//! same git-root-walk convention `config::index_resolver` already uses to
//! auto-derive the trusty-search index (issue #661), so "the repo" means the
//! same thing everywhere in this crate.
//!
//! What: [`find_repo_config_path`] is the impure CWD-based entry point used by
//! `ReviewConfig::from_env_and_file`; [`find_repo_config_path_from`] is the
//! pure, unit-testable core that takes an explicit starting directory.  Both
//! return `None` when no `.trusty-review.toml` file exists at the discovered
//! git root — absence is not an error (fail-open, matching `load_toml_file`).
//!
//! Test: `finds_repo_config_at_git_root`, `finds_repo_config_from_nested_cwd`,
//! `no_repo_config_returns_none`,
//! `no_git_root_returns_none_unless_start_itself_has_file`.

use std::path::{Path, PathBuf};

use super::index_resolver::find_git_root;

/// Filename for the per-project review config, discovered at the repo root.
pub const REPO_CONFIG_FILENAME: &str = ".trusty-review.toml";

/// Locate `.trusty-review.toml` at the git root of an explicit starting
/// directory.
///
/// Why: separated from [`find_repo_config_path`] so tests can exercise the
/// discovery logic against a tempdir fixture without mutating the real
/// process CWD (a process-global resource unsafe to touch from parallel
/// tests) — mirrors the pure/impure split `index_resolver::find_git_root` /
/// `repo_root_from_cwd` already uses.
/// What: walks up from `start` to the nearest `.git` entry (via
/// `find_git_root`), then checks for a `.trusty-review.toml` file there.
/// Returns `None` when the file does not exist (or is not a regular file).
/// Test: `finds_repo_config_at_git_root`, `finds_repo_config_from_nested_cwd`,
/// `no_repo_config_returns_none`.
pub fn find_repo_config_path_from(start: &Path) -> Option<PathBuf> {
    let root = find_git_root(start);
    let candidate = root.join(REPO_CONFIG_FILENAME);
    candidate.is_file().then_some(candidate)
}

/// Locate `.trusty-review.toml` at the git root of the current working
/// directory.
///
/// Why: the production call site (`ReviewConfig::from_env_and_file`) has no
/// explicit directory to hand in — the process CWD is the repo under review,
/// consistent with how `index_resolver::repo_root_from_cwd` already works.
/// What: reads `std::env::current_dir()` and delegates to
/// [`find_repo_config_path_from`].  Returns `None` on any I/O failure or
/// when no `.trusty-review.toml` exists — discovery is fail-open, never a
/// hard error.
/// Test: exercised indirectly by `ReviewConfig::from_env_and_file` callers;
/// the pure logic is covered directly via `find_repo_config_path_from`.
pub fn find_repo_config_path() -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    find_repo_config_path_from(&cwd)
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_repo_config_at_git_root() {
        let root = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir(root.path().join(".git")).expect("mkdir .git");
        std::fs::write(root.path().join(".trusty-review.toml"), "[voice]\n").expect("write");

        let found = find_repo_config_path_from(root.path()).expect("must find repo config");
        assert_eq!(found, root.path().join(".trusty-review.toml"));
    }

    #[test]
    fn finds_repo_config_from_nested_cwd() {
        let root = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir(root.path().join(".git")).expect("mkdir .git");
        std::fs::write(root.path().join(".trusty-review.toml"), "[voice]\n").expect("write");
        let nested = root.path().join("src").join("deep");
        std::fs::create_dir_all(&nested).expect("mkdir nested");

        let found =
            find_repo_config_path_from(&nested).expect("must find repo config from nested dir");
        assert_eq!(found, root.path().join(".trusty-review.toml"));
    }

    #[test]
    fn no_repo_config_returns_none() {
        let root = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir(root.path().join(".git")).expect("mkdir .git");
        // No .trusty-review.toml written.
        assert!(find_repo_config_path_from(root.path()).is_none());
    }

    #[test]
    fn no_git_root_returns_none_unless_start_itself_has_file() {
        // Without a `.git` anchor, `find_git_root` degrades to `start` itself
        // (see `index_resolver::find_git_root`) — so a `.trusty-review.toml`
        // sitting directly in `start` is still discovered even without a
        // `.git` directory. This documents that fallback rather than asserting
        // a stricter (unimplemented) git-required behaviour.
        let root = tempfile::tempdir().expect("tempdir");
        assert!(find_repo_config_path_from(root.path()).is_none());

        std::fs::write(root.path().join(".trusty-review.toml"), "[voice]\n").expect("write");
        assert!(find_repo_config_path_from(root.path()).is_some());
    }
}
