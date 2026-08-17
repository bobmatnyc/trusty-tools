//! Project-root detection and the lazy-write safety guard.
//!
//! Why: project-root detection is asked by five crates, so the walk itself
//! moved to `trusty_common::palace_resolve` (#5811) and this module keeps only
//! trusty-memory's own concerns — the `personal` sentinel and the guard that
//! decides where a pin file may be created.
//! What: re-exports `find_project_root`, `PROJECT_MARKERS` and
//! `TRUSTY_TOOLS_DIR` from the shared resolver; defines `PERSONAL_PALACE` and
//! `is_unsafe_pin_location`.
//! Test: `project_slug_finds_git_root`, `project_slug_returns_none_without_markers`,
//! `project_slug_uses_first_ancestor_marker`, `trusty_tools_dir_is_project_marker`.

use std::path::Path;

// #5811: the walk and the marker list are shared — trusty-mpm, trusty-code,
// trusty-agents and trusty-common's catch-up all need the same answer for
// "where does this project begin?", so a second copy here would be a second
// answer.
pub use trusty_common::palace_resolve::{find_project_root, PROJECT_MARKERS, TRUSTY_TOOLS_DIR};

/// Sentinel palace name that is always valid regardless of project context.
///
/// Why: users operating outside any project root (global notes, exploratory
/// sessions, personal task lists) need a stable palace that can receive
/// memories without failing the project-enforcement gate. The name `personal`
/// is the single reserved identifier for this purpose.
/// What: a `&str` constant that the enforcement logic tests against before
/// applying project-slug validation.
/// Test: `validate_palace_name_accepts_personal`.
pub const PERSONAL_PALACE: &str = "personal";

/// Return `true` when `root` is an unsafe location for a lazily-written pin.
///
/// Why (product guard): when the walk finds no real project marker it can fall
/// through to the system temp dir, the user's home directory, or the filesystem
/// root. Writing a pin file there silently poisons every future invocation from
/// any subdirectory of that path — including every `tempfile::tempdir()` in the
/// test suite. The guard intercepts this before the write so only genuine
/// project roots ever receive a pin file.
/// What: canonicalises `root` and compares it against `std::env::temp_dir()`,
/// `dirs::home_dir()`, and `/`. A path that cannot be canonicalised is treated
/// as unsafe.
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
