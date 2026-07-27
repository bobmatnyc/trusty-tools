//! Cross-branch `git push` guard: the `pre-push` hook and its installer (#2867).
//!
//! Why: in the PR #2863 incident an agent-created worktree carried
//! `branch.<local>.merge = refs/heads/<a-foreign-PR-branch>`, so a bare `git
//! push` from it clobbered that PR's reviewed lineage 46 minutes after the
//! session paused. Two facts from the post-mortem shape this module: the
//! offending worktree was created by an agent's own `git worktree add` (NOT by
//! trusty-mpm's provisioner), and no live process owner was ever identified.
//! A provisioner-side or agent-lifecycle fix therefore cannot cover the real
//! case — only a guard that fires on the push itself, whoever runs it, can.
//! What: bundles the `pre-push` script (see `../assets/hooks/pre-push`) and
//! installs it into a repository's EFFECTIVE hooks directory. Empirically
//! verified (git 2.54.0, real temp repos): `$GIT_COMMON_DIR/hooks` is shared by
//! the main checkout, provisioner-created linked worktrees, AND ad-hoc
//! `git worktree add` worktrees alike, and stays shared when
//! `extensions.worktreeConfig` is enabled — so ONE file covers every worktree
//! of a base clone with no config change at all. Installation is refused
//! (never forced) when a foreign `pre-push` hook or a `core.hooksPath`
//! redirect is already in place, so this never fights husky/lefthook/pre-commit.
//! Test: `core::push_guard` unit tests below plus the real-git integration
//! test `crates/trusty-mpm/tests/push_guard_hook.rs`.

use std::path::{Path, PathBuf};

/// The bundled `pre-push` guard script, installed verbatim.
///
/// Why: shipping the script as an asset (rather than a Rust string literal)
/// keeps it lintable/executable as a real shell script in the source tree.
/// What: the contents of `../assets/hooks/pre-push`.
/// Test: `bundled_hook_carries_marker_and_shebang`.
pub const PRE_PUSH_HOOK: &str = include_str!("../assets/hooks/pre-push");

/// Marker line identifying a `pre-push` hook as trusty-mpm's own.
///
/// Why: the installer must be able to distinguish "our hook, possibly an older
/// revision — safe to overwrite" from "somebody else's hook — never touch".
/// What: a comment line present in [`PRE_PUSH_HOOK`]; bump the version suffix
/// in the asset when the script's behaviour changes.
/// Test: `bundled_hook_carries_marker_and_shebang`.
pub const HOOK_MARKER: &str = "trusty-mpm-push-guard:";

/// Result of an [`install_pre_push_guard`] attempt.
///
/// Why: callers log the three outcomes differently — an install is worth an
/// `info!`, an unchanged hook is silent, and a refusal is a `warn!` naming the
/// reason so an operator can resolve it by hand.
/// What: one variant per terminal state. `Refused` carries a human-readable
/// reason, never a path the caller is expected to delete.
/// Test: `refuses_foreign_hook`, `installs_then_reports_already_current`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookInstall {
    /// The guard was written (fresh install, or an older revision replaced).
    Installed(PathBuf),
    /// The guard was already present and byte-identical; nothing was written.
    AlreadyCurrent(PathBuf),
    /// Nothing was written because another hook manager owns this hook.
    Refused(String),
}

/// Resolve the hooks directory git will actually consult for `repo_path`.
///
/// Why: writing to `<repo>/.git/hooks` is wrong whenever `core.hooksPath`
/// redirects hook lookup elsewhere — the file would sit there inert, giving
/// false assurance. Resolving the EFFECTIVE directory makes "installed" mean
/// "will run".
/// What: returns `Err` when `core.hooksPath` is set (that repo belongs to
/// another hook manager, so the caller must refuse rather than write into a
/// directory it does not own); otherwise returns `$GIT_COMMON_DIR/hooks`,
/// which linked and ad-hoc worktrees share with the base clone. The common dir
/// is asked for as an absolute path so the result is usable regardless of the
/// caller's cwd.
/// Test: `hooks_dir_resolves_to_common_dir`, `hooks_dir_errs_on_custom_hookspath`.
pub fn effective_hooks_dir(repo_path: &Path) -> Result<PathBuf, String> {
    let custom = git_config_get(repo_path, "core.hooksPath");
    if let Some(custom) = custom {
        return Err(format!(
            "core.hooksPath is set to {custom:?}; another hook manager owns this repository"
        ));
    }

    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(repo_path)
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .output()
        .map_err(|e| format!("git rev-parse --git-common-dir failed to spawn: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git rev-parse --git-common-dir failed ({}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let common = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if common.is_empty() {
        return Err("git rev-parse --git-common-dir returned nothing".to_string());
    }
    Ok(PathBuf::from(common).join("hooks"))
}

/// Install the bundled cross-branch push guard into `repo_path`, idempotently.
///
/// Why: see the module docs — this is the only #2867 mitigation that fires for
/// a push issued by a process trusty-mpm did not create.
/// What: resolves the effective hooks directory via [`effective_hooks_dir`],
/// then writes [`PRE_PUSH_HOOK`] to `<hooks>/pre-push` with mode `0o755`.
/// Refuses (returns [`HookInstall::Refused`], never an `Err`, and never
/// overwrites) when a `pre-push` hook exists that does not carry
/// [`HOOK_MARKER`]. Returns `Err` only for genuine I/O or git failures.
/// Repeated calls are safe: an identical hook yields
/// [`HookInstall::AlreadyCurrent`] with no write.
/// Test: `installs_then_reports_already_current`, `refuses_foreign_hook`,
/// plus `crates/trusty-mpm/tests/push_guard_hook.rs` end-to-end.
pub fn install_pre_push_guard(repo_path: &Path) -> Result<HookInstall, String> {
    let hooks_dir = match effective_hooks_dir(repo_path) {
        Ok(d) => d,
        Err(reason) => return Ok(HookInstall::Refused(reason)),
    };
    let hook_path = hooks_dir.join("pre-push");

    if let Ok(existing) = std::fs::read_to_string(&hook_path) {
        if !existing.contains(HOOK_MARKER) {
            return Ok(HookInstall::Refused(format!(
                "a non-trusty-mpm pre-push hook already exists at {}",
                hook_path.display()
            )));
        }
        if existing == PRE_PUSH_HOOK {
            return Ok(HookInstall::AlreadyCurrent(hook_path));
        }
    }

    std::fs::create_dir_all(&hooks_dir)
        .map_err(|e| format!("failed to create {}: {e}", hooks_dir.display()))?;
    std::fs::write(&hook_path, PRE_PUSH_HOOK)
        .map_err(|e| format!("failed to write {}: {e}", hook_path.display()))?;
    set_executable(&hook_path)?;
    Ok(HookInstall::Installed(hook_path))
}

/// Best-effort [`install_pre_push_guard`] that logs its outcome and never fails.
///
/// Why: both worktree-creating code paths (the provisioner's bare clone and the
/// in-project `ensure_base_clone`) want identical behaviour — try to install,
/// say so, and carry on. Duplicating the four-arm match at each call site is
/// how the two paths would drift.
/// What: installs the guard, logging an `info!` on a fresh install, nothing on
/// a no-op, and a `warn!` naming the reason on a refusal or error. Never
/// returns a value: no caller may fail a clone over hook installation.
/// Test: `crates/trusty-mpm/tests/push_guard_hook.rs` (behaviour) and
/// `provisioner::workspace::tests::ensure_base_checkout_installs_push_guard`
/// (call-site wiring).
pub fn install_and_log(repo_path: &Path) {
    match install_pre_push_guard(repo_path) {
        Ok(HookInstall::Installed(p)) => {
            tracing::info!(hook = %p.display(), "cross-branch push guard installed (#2867)");
        }
        Ok(HookInstall::AlreadyCurrent(_)) => {}
        Ok(HookInstall::Refused(reason)) => {
            tracing::warn!(
                repo = %repo_path.display(),
                "cross-branch push guard NOT installed (#2867): {reason}"
            );
        }
        Err(e) => {
            tracing::warn!(
                repo = %repo_path.display(),
                "cross-branch push guard install failed (non-fatal): {e}"
            );
        }
    }
}

/// Read a single git config value, returning `None` when unset.
///
/// Why: `git config --get` exits 1 for "not set", which is not an error here.
/// What: runs `git -C <repo> config --get <key>` and maps a non-zero exit or
/// blank output to `None`.
/// Test: exercised indirectly by `hooks_dir_errs_on_custom_hookspath`.
fn git_config_get(repo_path: &Path, key: &str) -> Option<String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(repo_path)
        .args(["config", "--get", key])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let value = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if value.is_empty() { None } else { Some(value) }
}

/// Give the installed hook file the executable bit git requires.
///
/// Why: git silently ignores a non-executable hook on unix — a mode-less
/// install is indistinguishable from no install at all.
/// What: sets mode `0o755` on unix; a no-op elsewhere (Windows git derives
/// executability from the shebang, not the filesystem mode).
/// Test: `installs_then_reports_already_current` asserts the mode on unix.
fn set_executable(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("failed to chmod {}: {e}", path.display()))?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Create a real, minimal git repo in a hermetic temp dir.
    fn temp_repo() -> Option<(tempfile::TempDir, PathBuf)> {
        let dir = crate::test_support::hermetic_temp_dir();
        let path = dir.path().to_path_buf();
        let ok = std::process::Command::new("git")
            .args(["init", "-q", "-b", "main"])
            .arg(&path)
            .status()
            .ok()?;
        if !ok.success() {
            return None;
        }
        Some((dir, path))
    }

    #[test]
    fn bundled_hook_carries_marker_and_shebang() {
        assert!(
            PRE_PUSH_HOOK.starts_with("#!/bin/sh"),
            "the bundled hook must be a POSIX sh script"
        );
        assert!(
            PRE_PUSH_HOOK.contains(HOOK_MARKER),
            "the bundled hook must carry the ownership marker {HOOK_MARKER}"
        );
        assert!(
            PRE_PUSH_HOOK.contains("TM_ALLOW_CROSS_BRANCH_PUSH"),
            "the bundled hook must document its override env var"
        );
    }

    #[test]
    fn hooks_dir_resolves_to_common_dir() {
        let Some((_dir, repo)) = temp_repo() else {
            return;
        };
        let hooks = effective_hooks_dir(&repo).expect("hooks dir must resolve in a fresh repo");
        assert!(
            hooks.ends_with("hooks"),
            "resolved hooks dir must end in `hooks`, got {}",
            hooks.display()
        );
        assert!(
            hooks.is_absolute(),
            "resolved hooks dir must be absolute, got {}",
            hooks.display()
        );
    }

    #[test]
    fn hooks_dir_errs_on_custom_hookspath() {
        let Some((_dir, repo)) = temp_repo() else {
            return;
        };
        assert!(
            std::process::Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(["config", "core.hooksPath", "/somewhere/else"])
                .status()
                .expect("git config")
                .success()
        );
        let err = effective_hooks_dir(&repo).expect_err("a custom hooksPath must be refused");
        assert!(
            err.contains("core.hooksPath"),
            "refusal must name core.hooksPath, got: {err}"
        );
        // And the installer must surface it as a Refused, not an Err.
        match install_pre_push_guard(&repo).expect("install must not hard-error") {
            HookInstall::Refused(reason) => assert!(reason.contains("core.hooksPath")),
            other => panic!("expected Refused, got {other:?}"),
        }
    }

    #[test]
    fn installs_then_reports_already_current() {
        let Some((_dir, repo)) = temp_repo() else {
            return;
        };
        let first = install_pre_push_guard(&repo).expect("first install");
        let hook_path = match &first {
            HookInstall::Installed(p) => p.clone(),
            other => panic!("expected Installed, got {other:?}"),
        };
        assert_eq!(
            std::fs::read_to_string(&hook_path).expect("read hook"),
            PRE_PUSH_HOOK
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&hook_path)
                .expect("stat hook")
                .permissions()
                .mode();
            assert_eq!(
                mode & 0o111,
                0o111,
                "hook must be executable, mode {mode:o}"
            );
        }

        let second = install_pre_push_guard(&repo).expect("second install");
        assert_eq!(
            second,
            HookInstall::AlreadyCurrent(hook_path),
            "a repeat install must be a no-op"
        );
    }

    #[test]
    fn reinstalls_over_an_older_trusty_mpm_revision() {
        let Some((_dir, repo)) = temp_repo() else {
            return;
        };
        let hooks = effective_hooks_dir(&repo).expect("hooks dir");
        std::fs::create_dir_all(&hooks).expect("mkdir hooks");
        std::fs::write(
            hooks.join("pre-push"),
            "#!/bin/sh\n# trusty-mpm-push-guard: v0\nexit 0\n",
        )
        .expect("seed old hook");

        match install_pre_push_guard(&repo).expect("install") {
            HookInstall::Installed(p) => {
                assert_eq!(std::fs::read_to_string(p).expect("read"), PRE_PUSH_HOOK);
            }
            other => panic!("an older trusty-mpm revision must be upgraded, got {other:?}"),
        }
    }

    #[test]
    fn refuses_foreign_hook() {
        let Some((_dir, repo)) = temp_repo() else {
            return;
        };
        let hooks = effective_hooks_dir(&repo).expect("hooks dir");
        std::fs::create_dir_all(&hooks).expect("mkdir hooks");
        let foreign = "#!/bin/sh\n# husky\nexit 0\n";
        std::fs::write(hooks.join("pre-push"), foreign).expect("seed foreign hook");

        match install_pre_push_guard(&repo).expect("install must not hard-error") {
            HookInstall::Refused(reason) => {
                assert!(reason.contains("non-trusty-mpm"), "got: {reason}");
            }
            other => panic!("expected Refused, got {other:?}"),
        }
        assert_eq!(
            std::fs::read_to_string(hooks.join("pre-push")).expect("read"),
            foreign,
            "a foreign hook must never be overwritten"
        );
    }
}
