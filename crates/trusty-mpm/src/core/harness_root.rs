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

/// The shared bare clone trusty-mpm provisioned inside a managed project
/// before #4270.
///
/// Why: `provisioner::workspace` used to clone the shared base into
/// `<project>/.base` and add every managed session's worktree from THERE, so
/// those worktrees' git common dir is the bare clone — a directory INSIDE the
/// project, not the project. #4270 retired that store: provisioning now clones
/// the base into `<project>` itself and puts worktrees at
/// `<project>/.worktrees/<id>`, matching the in-project path. Existing `.base`
/// stores were deliberately NOT migrated, so this mapping stays load-bearing
/// for every worktree already under one, and the name is also what
/// `provisioner::workspace`'s own guard checks before refusing to clone over a
/// live legacy store.
/// What: `.base`. The name alone is never sufficient — see
/// [`map_base_clone_to_project`], which also requires the directory to be a
/// BARE repository before rewriting it.
/// Test: `harness_root_maps_a_base_clone_back_to_the_project`,
/// `harness_root_for_a_non_bare_repo_named_base_is_itself`,
/// `provision_in_leaves_an_existing_dot_base_store_untouched`.
pub(crate) const BASE_CLONE_DIRNAME: &str = ".base";

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
/// What: probes git through the hardened [`git_command`] (which strips the
/// environment variables that could point git at a different repository) and
/// branches on the ONE distinction that matters — whether `dir` sits in a
/// linked worktree, i.e. whether `--git-dir` differs from `--git-common-dir`.
///
/// - Not a linked worktree, not bare: `dir` is in a working tree that owns its
///   own state, so the answer is `--show-toplevel`. This is deliberately NOT
///   derived from the git directory, because for a submodule
///   (`<super>/.git/modules/<name>`) and for a `--separate-git-dir` checkout
///   (`<store>.git`) the git directory is not inside the working tree at all.
/// - Not a linked worktree, bare: there is no working tree, so the repository
///   directory is the root — modulo [`map_base_clone_to_project`].
/// - A linked worktree: git's SHARED directory identifies the checkout it
///   belongs to. `<main>/.git` → the main checkout; anything else is a
///   repository directory, again modulo [`map_base_clone_to_project`].
///
/// Any failed probe returns `None`, never a guess.
/// Test: `harness_root_is_the_main_checkout_for_a_worktree`,
/// `harness_root_maps_a_base_clone_back_to_the_project`,
/// `harness_root_for_is_none_outside_a_git_repo`,
/// `harness_root_is_the_repo_root_from_a_subdirectory`,
/// `harness_root_for_a_non_bare_repo_named_base_is_itself`,
/// `harness_root_for_a_submodule_is_the_submodule_checkout`,
/// `harness_root_for_a_separate_git_dir_checkout_is_the_working_tree`.
pub fn harness_root_for(dir: &Path) -> Option<PathBuf> {
    let common_dir = git_rev_parse_path(dir, "--git-common-dir")?;
    let git_dir = git_rev_parse_path(dir, "--git-dir")?;

    if git_dir == common_dir {
        if git_is_bare(dir).unwrap_or(false) {
            return Some(map_base_clone_to_project(common_dir));
        }
        return git_rev_parse_path(dir, "--show-toplevel");
    }

    if common_dir.file_name().is_some_and(|n| n == ".git") {
        return Some(common_dir.parent()?.to_path_buf());
    }
    Some(map_base_clone_to_project(common_dir))
}

/// Rewrite a bare [`BASE_CLONE_DIRNAME`] clone to the project that contains it.
///
/// Why: `<project>/.base` is trusty-mpm's own provisioning artifact — a
/// directory INSIDE the project, not the project — so harness state resolved
/// through it belongs one level up. Bareness is part of the test, not an
/// assumption: an ordinary checkout that merely happens to be named `.base`
/// owns its own state and must resolve to ITSELF, or a plain repository would
/// write its harness state outside itself and possibly into a different
/// repository (#4841 review).
/// What: returns `repo_dir`'s parent only when `repo_dir` is named `.base` AND
/// `git rev-parse --is-bare-repository` confirms it is bare; otherwise
/// `repo_dir` unchanged. An unobservable bareness probe does not remap.
/// Test: `harness_root_maps_a_base_clone_back_to_the_project`,
/// `harness_root_for_a_non_bare_repo_named_base_is_itself`.
fn map_base_clone_to_project(repo_dir: PathBuf) -> PathBuf {
    if repo_dir
        .file_name()
        .is_some_and(|n| n == BASE_CLONE_DIRNAME)
        && git_is_bare(&repo_dir).unwrap_or(false)
        && let Some(project) = repo_dir.parent()
    {
        return project.to_path_buf();
    }
    repo_dir
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

/// One absolute path from a `git rev-parse` path selector, or `None`.
///
/// Why: an empty stdout, a non-zero exit, or a missing `git` must all read as
/// "could not observe" rather than as a fact about `dir`. `--show-toplevel`
/// legitimately fails inside a bare repository, which is exactly why the caller
/// establishes bareness first.
/// What: `git -C <dir> rev-parse --path-format=absolute <selector>` through the
/// hardened [`git_command`].
/// Test: covered by every `harness_root_*` test.
fn git_rev_parse_path(dir: &Path, selector: &str) -> Option<PathBuf> {
    let out = git_command(dir, &["rev-parse", "--path-format=absolute", selector])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let raw = String::from_utf8_lossy(&out.stdout);
    let trimmed = raw.trim();
    (!trimmed.is_empty()).then(|| PathBuf::from(trimmed))
}

/// Is `dir` inside a bare repository? `None` when the probe is unobservable.
///
/// Why: bareness is what separates "no working tree owns this, so the
/// repository directory is the root" from "a working tree owns this". Anything
/// other than a clean `true`/`false` is `None` so a caller can refuse to guess.
/// What: `git -C <dir> rev-parse --is-bare-repository` through [`git_command`].
/// Test: `harness_root_maps_a_base_clone_back_to_the_project`,
/// `harness_root_for_a_non_bare_repo_named_base_is_itself`.
fn git_is_bare(dir: &Path) -> Option<bool> {
    let out = git_command(dir, &["rev-parse", "--is-bare-repository"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    match String::from_utf8_lossy(&out.stdout).trim() {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
#[path = "harness_root_tests.rs"]
mod tests;
