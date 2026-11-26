//! Project-root detection and the `personal` sentinel.
//!
//! Why: project-root detection is asked by five crates, so the walk itself
//! moved to `trusty_common::palace_resolve` (#5811) and this module keeps only
//! trusty-memory's own concern — the `personal` sentinel. #11823 removed the
//! lazy pin write and with it the guard that decided where one may land.
//! What: re-exports `find_project_root`, `PROJECT_MARKERS` and
//! `TRUSTY_TOOLS_DIR` from the shared resolver; defines `PERSONAL_PALACE`.
//! Test: `project_slug_finds_git_root`, `project_slug_returns_none_without_markers`,
//! `project_slug_uses_first_ancestor_marker`, `trusty_tools_dir_is_project_marker`.

// #5811: the walk and the marker list are shared — trusty-mpm, trusty-code,
// trusty-assistants and trusty-common's catch-up all need the same answer for
// "where does this project begin?", so a second copy here would be a second
// answer.
pub use trusty_common::palace_resolve::{PROJECT_MARKERS, TRUSTY_TOOLS_DIR, find_project_root};

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
