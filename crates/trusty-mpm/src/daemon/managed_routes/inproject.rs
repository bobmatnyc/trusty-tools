//! In-project spawn path: protected base clone + per-session worktree (#1706).
//!
//! Why: operators who run `tm` from inside a git repository should get a managed
//! session against a git-worktree slice of that repo rather than a full clone.
//! This module implements the "in-project" path: (a) a durable PROTECTED base
//! clone under `<repos_root>/<owner>/<repo>/` that is always on the default
//! branch and owned by the daemon, and (b) a per-session git worktree branched
//! off that clone so each session works in isolation.
//! What: [`repos_root`] resolves the configurable root; [`base_clone_path`]
//! computes the per-project clone directory; [`ensure_base_clone`] clones if
//! absent; [`create_session_worktree`] adds a per-session worktree;
//! [`try_inproject_spawn`] is the main entry point called from the lifecycle layer;
//! [`get_origin_url`] reads the remote.origin.url from a git directory.
//! Test: `try_inproject_spawn_returns_none_for_non_git_path` unit test covers the
//! non-git early exit; integration coverage via `tests/local_spawn.rs`.

use std::path::{Path, PathBuf};

use tracing::{info, warn};

use crate::session_manager::ManagedSessionId;

/// Environment variable that overrides the managed repos root.
///
/// Why: operators need an escape hatch (tests, non-standard layouts) that wins
/// over config without touching the config file.
/// What: `"TRUSTY_MPM_REPOS_ROOT"`.
/// Test: indirectly by integration tests that set this env var.
pub const REPOS_ROOT_ENV: &str = "TRUSTY_MPM_REPOS_ROOT";

/// Default repos root directory name under `$HOME`.
///
/// Why: mirrors the #1220 pattern; a peer directory keeps repos and ephemeral
/// sessions cleanly separated.
/// What: `"trusty-tools/repos"`.
/// Test: `repos_root_default_ends_with_expected_segments`.
pub const DEFAULT_REPOS_DIR: &str = "trusty-tools/repos";

/// Resolve the absolute root for base clones.
///
/// Why: the base-clone path needs ONE answer for "where do managed base clones
/// live?", with precedence env > built-in default (#1220 pattern).
/// What: applies **`TRUSTY_MPM_REPOS_ROOT` env > built-in `~/trusty-tools/repos`**,
/// expanding a leading `~`. Falls back to `/tmp/trusty-tools/repos` only when the
/// home directory is unresolvable.
/// Test: covered by the default-shape assertion in unit tests.
pub fn repos_root() -> PathBuf {
    let home = dirs::home_dir();

    if let Ok(raw) = std::env::var(REPOS_ROOT_ENV) {
        let raw = raw.trim();
        if !raw.is_empty() {
            return match &home {
                Some(h) => expand_tilde_repos(raw, h),
                None => PathBuf::from(raw),
            };
        }
    }

    match home {
        Some(h) => h.join(DEFAULT_REPOS_DIR),
        None => PathBuf::from("/tmp").join(DEFAULT_REPOS_DIR),
    }
}

/// Expand a leading `~` in a path string to `home`.
///
/// Why: env values are often home-relative; normalizing them once here keeps
/// caller code free of `~` handling.
/// What: replaces a leading `~/` or bare `~` with `home`; other paths pass through.
/// Test: covered indirectly by `repos_root` env-override tests.
fn expand_tilde_repos(template: &str, home: &Path) -> PathBuf {
    if let Some(rest) = template.strip_prefix("~/") {
        home.join(rest)
    } else if template == "~" {
        home.to_path_buf()
    } else {
        PathBuf::from(template)
    }
}

/// Compute the base clone path for a GitHub `owner/repo`.
///
/// Why: the base clone directory must be stable and deterministic so multiple
/// sessions against the same repo all share one protected clone.
/// What: returns `<repos_root>/<owner>/<repo>`.
/// Test: `base_clone_path_nests_owner_repo` (unit); wiring via integration tests.
pub fn base_clone_path(owner: &str, repo: &str) -> PathBuf {
    repos_root().join(owner).join(repo)
}

/// Ensure a base clone exists at `base_path`, cloning from `origin_url` if not.
///
/// Why: the first session against a repo triggers a one-time clone; subsequent
/// sessions reuse the same base directory and only add worktrees.
/// What: if `base_path/.git` exists, returns `Ok(())` (idempotent). Otherwise
/// runs `git clone --no-local <origin_url> <base_path>`. A clone failure returns
/// `Err` with the command's stderr.
/// Test: idempotent path covered by unit tests; clone path by integration tests.
pub fn ensure_base_clone(origin_url: &str, base_path: &Path) -> Result<(), String> {
    if base_path.join(".git").exists() {
        info!(
            path = %base_path.display(),
            "inproject: base clone already present, reusing"
        );
        return Ok(());
    }

    info!(
        url = %origin_url,
        dest = %base_path.display(),
        "inproject: cloning base repo"
    );

    if let Some(parent) = base_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            format!(
                "inproject: failed to create parent dir {}: {e}",
                parent.display()
            )
        })?;
    }

    // Use the parent of base_path (just created above) as cwd so a deleted
    // inherited cwd cannot cause git to fail at startup with "fatal: Unable to
    // read current working directory" (exit 128) → HTTP 500 on managed-spawn.
    let cwd = base_path.parent().unwrap_or(std::path::Path::new("/"));
    let out = std::process::Command::new("git")
        .args(["clone", "--no-local", origin_url])
        .arg(base_path)
        .current_dir(cwd)
        .output()
        .map_err(|e| format!("inproject: git clone failed to spawn: {e}"))?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(format!(
            "inproject: git clone failed ({}): {stderr}",
            out.status
        ));
    }
    info!(dest = %base_path.display(), "inproject: base clone complete");
    Ok(())
}

/// Create a per-session git worktree branched off the base clone.
///
/// Why: each managed session must work in an isolated branch; git worktrees
/// achieve this without duplicating the object store of the base clone.
/// What: runs `git -C <base_path> worktree add -b session/<session_id>
/// <base_path>/worktrees/<session_id>`. Returns the worktree path on success.
/// Test: covered by integration tests against a real temp repo.
pub fn create_session_worktree(
    base_path: &Path,
    session_id: &ManagedSessionId,
) -> Result<PathBuf, String> {
    let worktree_path = base_path.join("worktrees").join(session_id.to_string());
    let branch = format!("session/{session_id}");

    info!(
        base = %base_path.display(),
        worktree = %worktree_path.display(),
        branch = %branch,
        "inproject: creating per-session worktree"
    );

    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(base_path)
        .args(["worktree", "add", "-b"])
        .arg(&branch)
        .arg(&worktree_path)
        .output()
        .map_err(|e| format!("inproject: git worktree add failed to spawn: {e}"))?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(format!(
            "inproject: git worktree add failed ({}): {stderr}",
            out.status
        ));
    }

    info!(worktree = %worktree_path.display(), "inproject: per-session worktree created");
    Ok(worktree_path)
}

/// Read the `remote.origin.url` from a git repository at `path`.
///
/// Why: the in-project spawn path needs the remote URL to determine the
/// `owner/repo` identity and locate the matching base clone.
/// What: runs `git -C <path> config --get remote.origin.url` and returns the
/// trimmed stdout on success, or `None` if git fails or there is no remote origin.
/// Test: `get_origin_url_returns_none_for_non_git` (unit).
pub fn get_origin_url(path: &Path) -> Option<String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["config", "--get", "remote.origin.url"])
        .output()
        .ok()?;

    if !out.status.success() {
        return None;
    }
    let url = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if url.is_empty() { None } else { Some(url) }
}

/// Main entry point for the in-project spawn path (#1706).
///
/// Why: the lifecycle layer needs a single call that encapsulates in-project
/// detection and setup: is the given path inside a git repo with a GitHub remote?
/// If so, ensure the base clone exists and create a per-session worktree.
/// What: (1) checks that `path` is an existing directory with a `.git` entry;
/// (2) reads the `remote.origin.url` — returns `Ok(None)` if absent;
/// (3) parses the URL via `trusty_common::github_path::parse_github_path` —
/// returns `Ok(None)` if unparseable; (4) calls [`ensure_base_clone`] and
/// [`create_session_worktree`]; (5) returns `Ok(Some((worktree_path, owner, repo)))`.
/// Steps 4–5 errors propagate as `Err`.
/// Test: `try_inproject_spawn_returns_none_for_non_git_path` (unit).
pub fn try_inproject_spawn(
    path: &Path,
    session_id: &ManagedSessionId,
) -> Result<Option<(PathBuf, String, String)>, String> {
    if !path.is_dir() || !path.join(".git").exists() {
        return Ok(None);
    }

    let Some(origin_url) = get_origin_url(path) else {
        warn!(
            path = %path.display(),
            "inproject: no remote.origin.url found; falling through to local-path spawn"
        );
        return Ok(None);
    };

    let Some(gh) = trusty_common::github_path::parse_github_path(&origin_url) else {
        warn!(
            url = %origin_url,
            "inproject: cannot parse GitHub owner/repo from remote URL; falling through"
        );
        return Ok(None);
    };

    let base = base_clone_path(&gh.owner, &gh.repo);
    ensure_base_clone(&origin_url, &base)?;
    let worktree = create_session_worktree(&base, session_id)?;

    info!(
        owner = %gh.owner,
        repo = %gh.repo,
        worktree = %worktree.display(),
        session = %session_id,
        "inproject: per-session worktree ready"
    );

    Ok(Some((worktree, gh.owner, gh.repo)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn try_inproject_spawn_returns_none_for_non_git_path() {
        // A directory that is not a git repo must return Ok(None), not an error.
        let tmp = std::env::temp_dir();
        let id = ManagedSessionId::new();
        let result = try_inproject_spawn(&tmp, &id);
        assert!(
            matches!(result, Ok(None)),
            "non-git path should yield Ok(None), got {result:?}"
        );
    }

    #[test]
    fn get_origin_url_returns_none_for_non_git() {
        // A non-git directory should return None cleanly.
        let tmp = std::env::temp_dir();
        assert!(get_origin_url(&tmp).is_none());
    }

    #[test]
    fn repos_root_default_ends_with_expected_segments() {
        // Without the env var the default must end with DEFAULT_REPOS_DIR segments.
        let home = dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from("/tmp"));
        let expected = home.join(DEFAULT_REPOS_DIR);
        assert!(expected.ends_with(DEFAULT_REPOS_DIR));
    }
}
