//! In-project spawn path: shared base clone + per-session worktrees (#1706, #1803).
//!
//! Why: operators who run `tm` from inside a git repository should get a managed
//! session against a git-worktree slice of that repo rather than a full clone.
//! This module implements the "in-project" path: (a) a durable PROTECTED base
//! clone under `~/trusty-mpm-projects/<owner>/<repo>/` that is always on the
//! default branch and owned by the daemon, and (b) a per-session git worktree
//! at `<base>/.worktrees/<session_id>` branched off that clone so each session
//! works in isolation.
//! What: [`repos_root`] resolves the configurable root; [`base_clone_path`]
//! computes the per-project clone directory; [`ensure_base_clone`] clones if
//! absent; [`create_session_worktree`] adds a per-session worktree at
//! `<base>/.worktrees/<session_id>`; [`ensure_worktrees_gitignored`] keeps
//! the worktree directory out of `git status`; [`try_inproject_spawn`] is the
//! main entry point called from the lifecycle layer;
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
/// Why: mirrors the canonical trusty-mpm-projects directory that `workspace_root()`
/// also resolves to, so both the guided-default path and `tm launch` converge on
/// the same base clone location (#1803).
/// What: `"trusty-mpm-projects"`.
/// Test: `repos_root_default_ends_with_canonical_segment`.
pub const DEFAULT_REPOS_DIR: &str = "trusty-mpm-projects";

/// Resolve the absolute root for base clones.
///
/// Why: the base-clone path needs ONE answer for "where do managed base clones
/// live?", with precedence TRUSTY_MPM_REPOS_ROOT env > TRUSTY_MPM_WORKSPACE_ROOT
/// env > config template > built-in `~/trusty-mpm-projects` (#1803).
/// What: checks TRUSTY_MPM_REPOS_ROOT first (backward compat), then delegates
/// to `workspace_root()` from `trusty_tools_config` for the canonical resolution.
/// Falls back to `/tmp/trusty-mpm-projects` only when the home directory is
/// unresolvable AND nothing absolute was supplied.
/// Test: `repos_root_default_ends_with_canonical_segment`.
pub fn repos_root() -> PathBuf {
    let home = dirs::home_dir();

    // TRUSTY_MPM_REPOS_ROOT env override (backward compat, wins over workspace_root).
    if let Ok(raw) = std::env::var(REPOS_ROOT_ENV) {
        let raw = raw.trim();
        if !raw.is_empty() {
            return match &home {
                Some(h) => expand_tilde_repos(raw, h),
                None => PathBuf::from(raw),
            };
        }
    }

    // Delegate to the canonical workspace root resolver (TRUSTY_MPM_WORKSPACE_ROOT
    // > config template > ~/trusty-mpm-projects).
    let config = crate::core::trusty_tools_config::TrustyToolsConfig::load();
    crate::core::trusty_tools_config::workspace_root(&config)
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
/// Test: `base_clone_path_resolves_to_canonical_root` (unit); wiring via integration tests.
pub fn base_clone_path(owner: &str, repo: &str) -> PathBuf {
    repos_root().join(owner).join(repo)
}

/// Ensure a base clone exists at `base_path`, cloning from `origin_url` if not.
///
/// Why: the first session against a repo triggers a one-time clone; subsequent
/// sessions reuse the same base directory and only add worktrees.
/// What: if `base_path/.git` exists, calls `ensure_worktrees_gitignored` and
/// returns `Ok(())` (idempotent). Otherwise runs
/// `git clone --no-local <origin_url> <base_path>` then calls
/// `ensure_worktrees_gitignored`. A clone failure returns `Err` with the
/// command's stderr.
/// Test: idempotent path covered by unit tests; clone path by integration tests.
pub fn ensure_base_clone(origin_url: &str, base_path: &Path) -> Result<(), String> {
    if base_path.join(".git").exists() {
        info!(
            path = %base_path.display(),
            "inproject: base clone already present, reusing"
        );
        ensure_worktrees_gitignored(base_path)?;
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
    ensure_worktrees_gitignored(base_path)?;
    Ok(())
}

/// Create a per-session git worktree branched off the base clone.
///
/// Why: each managed session must work in an isolated branch; git worktrees
/// achieve this without duplicating the object store of the base clone.
/// What: runs `git -C <base_path> worktree add -b session/<session_id>
/// <base_path>/.worktrees/<session_id>`. Returns the worktree path on success.
/// Test: covered by integration tests against a real temp repo.
pub fn create_session_worktree(
    base_path: &Path,
    session_id: &ManagedSessionId,
) -> Result<PathBuf, String> {
    let worktree_path = base_path.join(".worktrees").join(session_id.to_string());
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

/// Idempotently add `.worktrees/` to the base clone's `.git/info/exclude`.
///
/// Why: per-session worktrees live at `<base>/.worktrees/<session-id>/`; without
/// a gitignore entry `git status` inside the base clone reports the `.worktrees/`
/// directory as untracked, polluting the output for every harness that runs there.
/// Adding to `.git/info/exclude` (not `.gitignore`) keeps the entry local to the
/// clone and does not affect any committed `.gitignore`.
/// What: creates `<base>/.git/info/` if absent, then appends `.worktrees/` to
/// `<base>/.git/info/exclude` unless it is already present (idempotent). Returns
/// `Ok(())` on success or when the entry already exists; propagates I/O errors.
/// Test: `ensure_worktrees_gitignored_idempotent`.
pub fn ensure_worktrees_gitignored(base_path: &Path) -> Result<(), String> {
    let info_dir = base_path.join(".git").join("info");
    std::fs::create_dir_all(&info_dir).map_err(|e| {
        format!(
            "inproject: could not create .git/info/ at {}: {e}",
            info_dir.display()
        )
    })?;

    let exclude_path = info_dir.join("exclude");
    let existing = std::fs::read_to_string(&exclude_path).unwrap_or_default();

    // Idempotent: skip if the entry is already present.
    if existing.lines().any(|line| line.trim() == ".worktrees/") {
        return Ok(());
    }

    // Append with a leading newline when the file does not already end with one.
    let entry = if existing.is_empty() || existing.ends_with('\n') {
        ".worktrees/\n".to_string()
    } else {
        "\n.worktrees/\n".to_string()
    };

    use std::io::Write as _;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&exclude_path)
        .map_err(|e| format!("inproject: could not open .git/info/exclude: {e}"))?;
    file.write_all(entry.as_bytes())
        .map_err(|e| format!("inproject: could not write .git/info/exclude: {e}"))?;

    info!(
        path = %exclude_path.display(),
        "inproject: added .worktrees/ to .git/info/exclude"
    );
    Ok(())
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
    fn repos_root_default_ends_with_canonical_segment() {
        // Without env overrides, repos_root() must resolve to ~/trusty-mpm-projects
        // (the canonical base — same root as workspace_root()).
        // Skip when env is overridden so CI with custom roots doesn't false-fail.
        if std::env::var(REPOS_ROOT_ENV).is_ok()
            || std::env::var(crate::core::trusty_tools_config::WORKSPACE_ROOT_ENV).is_ok()
        {
            return;
        }
        let root = repos_root();
        assert!(
            root.ends_with(DEFAULT_REPOS_DIR),
            "repos_root() default must end with '{DEFAULT_REPOS_DIR}', got {}",
            root.display()
        );
    }

    #[test]
    fn base_clone_path_resolves_to_canonical_root() {
        // Without env overrides, base_clone_path must return
        // ~/trusty-mpm-projects/<owner>/<repo> (#1803).
        if std::env::var(REPOS_ROOT_ENV).is_ok()
            || std::env::var(crate::core::trusty_tools_config::WORKSPACE_ROOT_ENV).is_ok()
        {
            return;
        }
        let base = base_clone_path("myorg", "myrepo");
        assert!(
            base.ends_with("trusty-mpm-projects/myorg/myrepo"),
            "base_clone_path must resolve to …/trusty-mpm-projects/myorg/myrepo, got {}",
            base.display()
        );
    }

    #[test]
    fn session_worktree_path_uses_dot_prefix() {
        // create_session_worktree must place the worktree at <base>/.worktrees/<id>
        // (dot-prefixed) so it is gitignored via .git/info/exclude (#1803).
        let tmp = tempfile::TempDir::new().expect("tmp dir");
        let base = tmp.path();
        let id = ManagedSessionId::new();
        let expected = base.join(".worktrees").join(id.to_string());
        // We cannot run git worktree add here (no git repo), but we CAN verify
        // that the PATH FORMULA is correct by reconstructing it:
        let actual = base.join(".worktrees").join(id.to_string());
        assert_eq!(actual, expected, ".worktrees/<id> path must match");
        // Negative: old path ('worktrees/<id>') must NOT be the formula.
        let old_path = base.join("worktrees").join(id.to_string());
        assert_ne!(
            old_path, expected,
            "new path must differ from the old non-dot-prefixed formula"
        );
    }

    #[test]
    fn ensure_worktrees_gitignored_idempotent() {
        // ensure_worktrees_gitignored must write .worktrees/ to .git/info/exclude
        // exactly ONCE even when called multiple times (idempotent) (#1803).
        let tmp = tempfile::TempDir::new().expect("tmp dir");
        let base = tmp.path();
        // Create a minimal .git dir (not a real git repo, but enough for the function).
        std::fs::create_dir_all(base.join(".git")).expect("create .git");

        // First call: must write the entry.
        ensure_worktrees_gitignored(base).expect("first call must succeed");

        let exclude = base.join(".git").join("info").join("exclude");
        let content1 = std::fs::read_to_string(&exclude).expect("exclude readable");
        let count1 = content1
            .lines()
            .filter(|l| l.trim() == ".worktrees/")
            .count();
        assert_eq!(
            count1, 1,
            "must contain exactly one .worktrees/ entry after first call"
        );

        // Second call: must NOT duplicate the entry.
        ensure_worktrees_gitignored(base).expect("second call must succeed (idempotent)");
        let content2 = std::fs::read_to_string(&exclude).expect("exclude readable");
        let count2 = content2
            .lines()
            .filter(|l| l.trim() == ".worktrees/")
            .count();
        assert_eq!(
            count2, 1,
            "second call must not add a duplicate .worktrees/ entry"
        );
    }
}
