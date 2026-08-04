//! Where a project's `.trusty-mpm/` harness state lives (#4832).
//!
//! Why: the owner ruling (2026-08-04) is that a worktree holds the mutable,
//! tracked working set — code, config, docs — and nothing else. Harness state
//! belongs to the PROJECT, not to the branch that happens to be checked out.
//! Every writer used to join `.trusty-mpm` onto whatever directory it was
//! handed, so one project accumulated a `.trusty-mpm/` per worktree, a bare `tm`
//! run from a subdirectory seeded one there, and a launch outside any repository
//! scattered one wherever the operator's shell happened to be standing.
//! What: [`harness_root_for`] answers "which checkout owns this project's
//! harness state" from git itself, then [`harness_dir`], [`framework_dir`] and
//! [`session_dir`] compose the three directories the ruling defines —
//! `framework/` (project-stable config), `sessions/<id>/` (per-session output),
//! and `logs/`. [`session_scope`] resolves the `<id>` segment.
//! Test: `harness_root_is_the_main_checkout_for_a_worktree`,
//! `harness_root_maps_a_base_clone_back_to_the_project`,
//! `harness_root_for_is_none_outside_a_git_repo`,
//! `session_dir_is_per_session_under_the_harness_root`.

use std::path::{Path, PathBuf};

use crate::session_manager::worktree_safety::git_command;

/// The project-local harness directory name.
///
/// Why: one literal, so a writer cannot spell it differently from a reader.
/// What: `.trusty-mpm`.
/// Test: `harness_dir_is_under_the_harness_root`.
pub const HARNESS_DIR: &str = ".trusty-mpm";

/// Subdirectory holding project-stable framework config (`manifest.toml`).
///
/// Why: the ruling separates project-stable config from per-session output;
/// this is the config half, and an operator MAY track it.
/// What: `framework`.
/// Test: `framework_dir_is_under_the_harness_dir`.
pub const FRAMEWORK_DIR: &str = "framework";

/// Subdirectory holding per-session output.
///
/// Why: compiled instructions are session-scoped, not project-scoped — that is
/// what removes the concurrent-session overwrite by construction.
/// What: `sessions`.
/// Test: `session_dir_is_per_session_under_the_harness_root`.
pub const SESSIONS_DIR: &str = "sessions";

/// The shared bare clone trusty-mpm provisions inside a managed project.
///
/// Why: `provisioner::workspace` clones the shared base into `<project>/.base`
/// and adds every managed session's worktree from THERE, so those worktrees'
/// git common dir is the bare clone — a directory INSIDE the project, not the
/// project. This is not a path guess: it is the same directory name
/// `provisioner::workspace::BASE_CHECKOUT_DIRNAME` writes and
/// `session_manager::worktree_registry` interrogates.
/// What: `.base`.
/// Test: `harness_root_maps_a_base_clone_back_to_the_project`.
const BASE_CLONE_DIRNAME: &str = ".base";

/// Environment variable carrying the managed session id inside a tm pane.
const MANAGED_SESSION_ID_ENV: &str = "TM_MANAGED_SESSION_ID";

/// Session-directory segment for a launch with no managed session identity.
///
/// Why: three of the six production `prepare_session` call sites (in-place
/// start, standalone load, deploy-validate repair) run BEFORE any session id
/// exists, and `tm connect` never mints one at all. They still need a
/// deterministic, project-local directory, and inventing a fresh id per call
/// would strew unreachable directories that no later reader could match to a
/// session.
/// What: `local` — one bucket per project for unmanaged launches, which is
/// still strictly narrower than the single per-project file it replaces.
/// Test: `session_scope_falls_back_to_the_unmanaged_bucket`.
pub const UNMANAGED_SESSION_SCOPE: &str = "local";

/// The checkout that owns `dir`'s harness state, or `None` outside git.
///
/// Why: a worktree must never accumulate harness state, and the only
/// authoritative answer to "which checkout is this worktree a worktree OF" is
/// git's own shared git directory. `None` is the load-bearing result: it means
/// `dir` is not inside any git working tree, which is the condition
/// `tm session start` refuses on rather than scattering `.trusty-mpm/` into an
/// arbitrary shell directory (#4832 defect 5).
/// What: reads `git rev-parse --path-format=absolute --git-common-dir` through
/// the hardened [`git_command`] (which strips the environment variables that
/// could point git at a different repository). A normal checkout — and every
/// linked worktree of it — answers `<root>/.git`, so the root is its parent; a
/// bare clone answers with the bare directory itself. A bare clone named
/// [`BASE_CLONE_DIRNAME`] is trusty-mpm's own provisioning artifact, so its
/// PARENT is the project. Any failed probe returns `None`, never a guess.
/// Test: `harness_root_is_the_main_checkout_for_a_worktree`,
/// `harness_root_maps_a_base_clone_back_to_the_project`,
/// `harness_root_for_is_none_outside_a_git_repo`,
/// `harness_root_is_the_repo_root_from_a_subdirectory`.
pub fn harness_root_for(dir: &Path) -> Option<PathBuf> {
    let common_dir = git_common_dir(dir)?;
    let root = if common_dir.file_name().is_some_and(|n| n == ".git") {
        common_dir.parent()?.to_path_buf()
    } else {
        common_dir
    };
    if root.file_name().is_some_and(|n| n == BASE_CLONE_DIRNAME)
        && let Some(project) = root.parent()
    {
        return Some(project.to_path_buf());
    }
    Some(root)
}

/// [`harness_root_for`], falling back to `dir` itself.
///
/// Why: the library must still produce a deterministic, project-local path for
/// a caller that is legitimately outside a repository — hermetic tests, and the
/// `tm connect`/standalone paths that operate on a directory the operator
/// named. The fallback is `dir` and never a global location: a non-git launch
/// is REFUSED at the CLI boundary (`tm session start`), so this is a
/// last-resort default, not a silent success path.
/// What: `harness_root_for(dir).unwrap_or_else(|| dir.to_path_buf())`.
/// Test: `harness_root_falls_back_to_the_given_dir_outside_git`.
pub fn harness_root(dir: &Path) -> PathBuf {
    harness_root_for(dir).unwrap_or_else(|| dir.to_path_buf())
}

/// `<harness-root>/.trusty-mpm`.
///
/// Why: every project-local harness write resolves its directory here, so a
/// worktree cannot grow one.
/// What: [`harness_root`] joined with [`HARNESS_DIR`].
/// Test: `harness_dir_is_under_the_harness_root`.
pub fn harness_dir(dir: &Path) -> PathBuf {
    harness_root(dir).join(HARNESS_DIR)
}

/// `<harness-root>/.trusty-mpm/framework`.
///
/// Why: the project-stable config layer — today the operator's `manifest.toml`
/// override. Deliberately NOT where per-session output lands.
/// What: [`harness_dir`] joined with [`FRAMEWORK_DIR`].
/// Test: `framework_dir_is_under_the_harness_dir`.
pub fn framework_dir(dir: &Path) -> PathBuf {
    harness_dir(dir).join(FRAMEWORK_DIR)
}

/// `<harness-root>/.trusty-mpm/sessions/<session_id>`.
///
/// Why: per-session output cannot collide with a concurrent session's by
/// construction, which is what retires the shared per-project compiled prompt.
/// What: [`harness_dir`] joined with [`SESSIONS_DIR`] and the session segment.
/// Test: `session_dir_is_per_session_under_the_harness_root`.
pub fn session_dir(dir: &Path, session_id: &str) -> PathBuf {
    harness_dir(dir).join(SESSIONS_DIR).join(session_id)
}

/// Resolve the `<session-id>` directory segment for this launch.
///
/// Why: the writers reach this seam with three different amounts of knowledge —
/// a managed spawn HAS the session id, an in-place relaunch runs inside a pane
/// that exports it, and a fresh in-place start has none yet. One resolver keeps
/// the three from picking different segments for the same session.
/// What: `explicit` when it is a usable segment, else
/// [`MANAGED_SESSION_ID_ENV`] from the process environment, else
/// [`UNMANAGED_SESSION_SCOPE`]. A candidate is usable only when it is a
/// non-empty run of `[A-Za-z0-9._-]` that is not `.` or `..` — the environment
/// is operator-writable and this value becomes a path component, so a `../`
/// escape must never reach [`session_dir`].
/// Test: exercised through [`session_scope_from`] —
/// `session_scope_prefers_the_explicit_id`,
/// `session_scope_reads_the_managed_env_var`,
/// `session_scope_falls_back_to_the_unmanaged_bucket`,
/// `session_scope_rejects_a_path_traversal_segment`.
pub fn session_scope(explicit: Option<&str>) -> String {
    let ambient = std::env::var(MANAGED_SESSION_ID_ENV).ok();
    session_scope_from(explicit, ambient.as_deref())
}

/// [`session_scope`] with the ambient managed id passed in.
///
/// Why: the resolution ORDER and the path-component validation are the parts
/// worth testing, and testing them through `std::env` would mean mutating
/// process-global state from a parallel test binary — the exact race this
/// split avoids. `session_scope` is then a two-line adapter with nothing left
/// to get wrong.
/// What: first usable of `explicit`, then `ambient`, else
/// [`UNMANAGED_SESSION_SCOPE`].
/// Test: `session_scope_prefers_the_explicit_id`,
/// `session_scope_reads_the_managed_env_var`,
/// `session_scope_falls_back_to_the_unmanaged_bucket`,
/// `session_scope_rejects_a_path_traversal_segment`.
pub fn session_scope_from(explicit: Option<&str>, ambient: Option<&str>) -> String {
    explicit
        .and_then(usable_scope)
        .or_else(|| ambient.and_then(usable_scope))
        .unwrap_or_else(|| UNMANAGED_SESSION_SCOPE.to_string())
}

/// Is `raw` safe to use as a single path component?
///
/// Why/What: see [`session_scope`]. Returns the owned segment when it is a
/// non-empty run of ASCII alphanumerics, `.`, `_` or `-` and is neither `.` nor
/// `..`; `None` otherwise.
/// Test: `session_scope_rejects_a_path_traversal_segment`.
fn usable_scope(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed == "." || trimmed == ".." {
        return None;
    }
    trimmed
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        .then(|| trimmed.to_string())
}

/// One absolute path from `git rev-parse --git-common-dir`, or `None`.
///
/// Why: an empty stdout, a non-zero exit, or a missing `git` must all read as
/// "could not observe" rather than as a fact about `dir`.
/// What: `git -C <dir> rev-parse --path-format=absolute --git-common-dir`
/// through the hardened [`git_command`].
/// Test: covered by every `harness_root_*` test.
fn git_common_dir(dir: &Path) -> Option<PathBuf> {
    let out = git_command(
        dir,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )
    .output()
    .ok()?;
    if !out.status.success() {
        return None;
    }
    let raw = String::from_utf8_lossy(&out.stdout);
    let trimmed = raw.trim();
    (!trimmed.is_empty()).then(|| PathBuf::from(trimmed))
}

#[cfg(test)]
#[path = "harness_root_tests.rs"]
mod tests;
