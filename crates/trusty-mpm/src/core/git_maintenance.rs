//! Idempotently disable git's own background maintenance/gc on a managed
//! checkout, once, at provisioning time (#7171).
//!
//! Why: [`trusty_common::git`] protects every git invocation THIS codebase
//! makes by passing `-c maintenance.auto=false -c gc.auto=0` on argv, which
//! outranks config — but it protects nothing an OPERATOR types directly into
//! a shell inside one of the ~25 worktrees a base clone can carry. Ordinary
//! git commands (`fetch`, `commit`, `checkout`, …) still decide whether to
//! trigger `git maintenance run --auto` by reading `maintenance.auto` from
//! config, so an operator's own `git pull` in a worktree was exactly as able
//! to fire the storm as the daemon's own code. Writing the setting into the
//! repo's shared `.git/config` closes that gap: every worktree of one base
//! clone shares `GIT_COMMON_DIR`, so ONE write at clone/registration time
//! covers every worktree the base will ever host, the same sharing property
//! `core::push_guard` already relies on for its `pre-push` hook install.
//! What: [`disable_auto_maintenance`] runs `git config --local
//! maintenance.auto false` and `git config --local gc.auto 0` against a
//! repository path — plain `git config --local`, not the argv-level `-c`
//! form, because this needs to persist in the checkout rather than apply to
//! one invocation. [`disable_and_log`] wraps it exactly like
//! [`super::push_guard::install_and_log`]: best-effort, never fails the
//! caller's clone or registration, `warn!`-logs any failure. Both writes are
//! plain `git config --local` calls, so re-running either is a no-op — safe
//! to call on every clone/registration, not just the first.
//! Test: `disable_auto_maintenance_writes_both_keys`,
//! `disable_auto_maintenance_is_idempotent`,
//! `disable_auto_maintenance_reports_a_non_repo_path`.

use std::path::Path;

/// The `git config --local <key> <value>` pairs this module writes.
///
/// Why: kept as data rather than duplicated call sites so
/// [`disable_auto_maintenance`]'s "both keys or a named failure" contract
/// cannot drift from what it actually iterates.
/// What: mirrors [`trusty_common::git::MAINTENANCE_DISABLE_ARGS`]'s two keys,
/// spelled as `git config` values (`"false"`/`"0"`) rather than `-c` argv
/// pairs (`"maintenance.auto=false"`/`"gc.auto=0"`) — the persisted and the
/// per-invocation forms use git's two different value spellings for the same
/// setting.
/// Test: `disable_auto_maintenance_writes_both_keys`.
const MAINTENANCE_CONFIG: &[(&str, &str)] = &[("maintenance.auto", "false"), ("gc.auto", "0")];

/// Write [`MAINTENANCE_CONFIG`] into `repo_path`'s local git config.
///
/// Why: see the module doc — this is the provisioning-time half of #7171.
/// What: runs `git -C <repo_path> config --local <key> <value>` once per
/// entry in [`MAINTENANCE_CONFIG`], via [`trusty_common::git::command_in`] so
/// the write itself cannot trigger a maintenance run either. Fails on the
/// first key that cannot be written (not a git repo, an unreadable config, a
/// spawn failure) rather than partially applying.
/// Test: `disable_auto_maintenance_writes_both_keys`,
/// `disable_auto_maintenance_is_idempotent`,
/// `disable_auto_maintenance_reports_a_non_repo_path`.
pub fn disable_auto_maintenance(repo_path: &Path) -> Result<(), String> {
    for (key, value) in MAINTENANCE_CONFIG {
        let out = trusty_common::git::command_in(repo_path)
            .args(["config", "--local", key, value])
            .output()
            .map_err(|e| format!("git config --local {key} failed to spawn: {e}"))?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            return Err(format!(
                "git config --local {key} {value} failed ({}): {}",
                out.status,
                stderr.trim()
            ));
        }
    }
    Ok(())
}

/// Best-effort [`disable_auto_maintenance`]: `warn!`-logs a failure, never
/// returns one.
///
/// Why: provisioning a managed checkout (a fresh base clone, an explicit
/// `mpm.projects.register`) must not fail — or partially roll back — over a
/// setting that is a storm-prevention nicety, not a correctness requirement,
/// exactly the same call this module mirrors [`super::push_guard::install_and_log`]
/// on.
/// What: logs `info!` on success, `warn!` (naming `repo_path` and the error)
/// on failure — including the ordinary case of `repo_path` not being a git
/// repository at all, which `register_project` cannot rule out ahead of the
/// call.
/// Test: covered indirectly by the call sites' own integration tests; the
/// pure success/failure branches are exercised directly by
/// `disable_auto_maintenance`'s own tests above.
pub fn disable_and_log(repo_path: &Path) {
    match disable_auto_maintenance(repo_path) {
        Ok(()) => {
            tracing::info!(
                repo = %repo_path.display(),
                "git auto-maintenance disabled on managed checkout (#7171)"
            );
        }
        Err(e) => {
            tracing::warn!(
                repo = %repo_path.display(),
                "git auto-maintenance NOT disabled (non-fatal, #7171): {e}"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git_ok(dir: &Path, args: &[&str]) -> bool {
        trusty_common::git::command_in(dir)
            .args(args)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    fn git_config_get(dir: &Path, key: &str) -> Option<String> {
        let out = trusty_common::git::command_in(dir)
            .args(["config", "--get", key])
            .output()
            .ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    /// A real repo, or `None` when `git` is unavailable on the runner — mirrors
    /// the established pattern in `session_manager::decommission_worktree_tests`.
    fn repo() -> Option<tempfile::TempDir> {
        let dir = tempfile::tempdir().ok()?;
        if !git_ok(dir.path(), &["init", "-q", "."]) {
            return None;
        }
        Some(dir)
    }

    #[test]
    fn disable_auto_maintenance_writes_both_keys() {
        let Some(dir) = repo() else {
            return;
        };
        disable_auto_maintenance(dir.path()).expect("config write succeeds");
        assert_eq!(
            git_config_get(dir.path(), "maintenance.auto").as_deref(),
            Some("false")
        );
        assert_eq!(git_config_get(dir.path(), "gc.auto").as_deref(), Some("0"));
    }

    #[test]
    fn disable_auto_maintenance_is_idempotent() {
        let Some(dir) = repo() else {
            return;
        };
        disable_auto_maintenance(dir.path()).expect("first write succeeds");
        disable_auto_maintenance(dir.path()).expect("second write succeeds");
        assert_eq!(
            git_config_get(dir.path(), "maintenance.auto").as_deref(),
            Some("false")
        );
    }

    #[test]
    fn disable_auto_maintenance_reports_a_non_repo_path() {
        let Ok(dir) = tempfile::tempdir() else {
            return;
        };
        // Not a git repo at all — `git config --local` must fail, not panic
        // or silently succeed.
        assert!(disable_auto_maintenance(dir.path()).is_err());
    }

    #[test]
    fn disable_and_log_does_not_panic_on_a_non_repo_path() {
        let Ok(dir) = tempfile::tempdir() else {
            return;
        };
        // Best-effort: must not panic even when the underlying write fails.
        disable_and_log(dir.path());
    }
}
