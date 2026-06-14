//! Project-root detection and filesystem walking.
//!
//! Why: Isolating the detection walk from the pin-file I/O keeps each file
//! under the 500-SLOC cap and makes the walk independently testable.
//! What: `find_project_root`, `find_project_root_with_marker`,
//! `is_unsafe_pin_location`, `PROJECT_MARKERS`, `TRUSTY_TOOLS_DIR`,
//! `PERSONAL_PALACE`.
//! Test: `project_slug_finds_git_root`, `project_slug_returns_none_without_markers`,
//! `project_slug_uses_first_ancestor_marker`, `trusty_tools_dir_is_project_marker`.

use std::path::{Path, PathBuf};

/// The `.trusty-tools/` directory name (used as a project marker).
///
/// Why: a project that already contains `.trusty-tools/trusty-memory.yaml`
/// should be recognised as a project root even if it has no `.git` or
/// `Cargo.toml`. Adding the directory itself to `PROJECT_MARKERS` (decision
/// D5) lets `find_project_root` detect this case without special-casing.
/// What: `".trusty-tools"`.
/// Test: `trusty_tools_dir_is_project_marker`.
pub const TRUSTY_TOOLS_DIR: &str = ".trusty-tools";

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
/// `.trusty-tools` is included (decision D5) so a directory that already
/// carries a pin file is recognised even without a `.git` or build manifest.
/// What: an ordered slice of filenames checked by `find_project_root`. A
/// directory is considered a project root when it contains *any* of these.
/// Test: `project_slug_uses_first_ancestor_marker`,
///       `trusty_tools_dir_is_project_marker`.
pub const PROJECT_MARKERS: &[&str] = &[
    ".git",
    "Cargo.toml",
    "pyproject.toml",
    "package.json",
    "go.mod",
    ".project-root",
    TRUSTY_TOOLS_DIR,
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
    find_project_root_with_marker(start).map(|(root, _)| root)
}

/// Walk upward from `start` and return the first ancestor directory (inclusive)
/// that contains at least one project marker, together with the name of the
/// marker that was found.
///
/// Why: the lazy-write path in `project_slug_at` needs to know whether the
/// detected root was anchored by a real marker (`.git`, `Cargo.toml`, etc.)
/// or whether no marker was found at all (in which case `find_project_root`
/// would already have returned `None` — this function's second return value
/// is always `Some` when the `PathBuf` is `Some`). The guard in
/// `project_slug_at` uses the marker name to distinguish a real project root
/// from a directory that only became the root by coincidence (e.g. a stale
/// pin file in `/tmp`).
/// What: same walk as `find_project_root`; returns `Some((root, marker))` on
/// success where `marker` is one of the strings from [`PROJECT_MARKERS`].
/// Returns `None` when no project root is found.
/// Test: indirectly via `find_project_root`, `project_slug_at`, and the
/// guard tests `lazy_write_skipped_for_temp_dir_root`.
pub(super) fn find_project_root_with_marker(start: &Path) -> Option<(PathBuf, &'static str)> {
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
                return Some((current, marker));
            }
        }
        // Ascend one level; stop at the filesystem root.
        match current.parent() {
            Some(parent) if parent != current => current = parent.to_path_buf(),
            _ => return None,
        }
    }
}

/// Return `true` when `root` is an unsafe location where we must not
/// lazily write a palace pin file.
///
/// Why (product guard): when `find_project_root` walks up from a temp or
/// scratch directory and finds no real project marker, it can fall through
/// to a fallback root such as the system temp dir, the user's home
/// directory, or the filesystem root. Writing a pin file there silently
/// poisons every future invocation from any subdirectory of that path —
/// including every `tempfile::tempdir()` in the test suite (which resolves
/// to a child of `/tmp`). The guard intercepts this before the write so
/// only genuine project roots ever receive a pin file.
/// What: canonicalises `root` and compares it against `std::env::temp_dir()`
/// (canonicalised), `dirs::home_dir()` (canonicalised, best-effort), and the
/// filesystem root `/`. Returns `true` when any comparison matches.
/// Test: `lazy_write_skipped_for_temp_dir_root`.
pub(super) fn is_unsafe_pin_location(root: &Path) -> bool {
    let canonical = match std::fs::canonicalize(root) {
        Ok(c) => c,
        // If we can't canonicalise, treat as unsafe to be conservative.
        Err(_) => return true,
    };

    // System temp dir (handles /tmp → /private/tmp on macOS).
    let temp = std::fs::canonicalize(std::env::temp_dir()).unwrap_or_else(|_| std::env::temp_dir());
    if canonical == temp {
        return true;
    }

    // User home directory.
    if let Some(home) = dirs::home_dir() {
        let home_canon = std::fs::canonicalize(&home).unwrap_or(home);
        if canonical == home_canon {
            return true;
        }
    }

    // Filesystem root.
    if canonical == std::path::Path::new("/") {
        return true;
    }

    false
}
