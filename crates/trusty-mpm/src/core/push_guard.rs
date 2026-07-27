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
//! (never forced) when a `core.hooksPath` redirect is set, or when an existing
//! `pre-push` is anything other than provably ours — a foreign hook, a symlink,
//! or a file that cannot be read at all (non-UTF-8 bytes, a permissions error).
//! So this never fights husky/lefthook/pre-commit. The write is atomic
//! (temp + `rename`) because the file is shared by every worktree of the base.
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

/// What a repository's `pre-push` slot currently holds — read-only.
///
/// Why: `tm doctor` must be able to REPORT whether a base clone is protected
/// without mutating it (doctor is warn-only by convention), and
/// [`install_pre_push_guard`] must make exactly the same ownership judgement.
/// Two implementations of "is this hook ours?" would drift, and a drift here
/// means either a false all-clear or a clobbered foreign hook.
/// What: one variant per state the installer branches on. `Missing` and the
/// two owned variants carry the resolved hook path; `Foreign` carries the
/// human-readable reason the slot is not ours to touch.
/// Test: `inspect_reports_missing_then_current`, `refuses_foreign_hook`,
/// `refuses_non_utf8_foreign_hook`, `refuses_symlinked_hook`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuardState {
    /// No `pre-push` hook exists; the path is where one would be written.
    Missing(PathBuf),
    /// Our guard is present and byte-identical to [`PRE_PUSH_HOOK`].
    Current(PathBuf),
    /// Our guard is present but an older revision; a reinstall would upgrade it.
    Outdated(PathBuf),
    /// The slot belongs to somebody else, or cannot be inspected at all.
    Foreign(String),
}

/// Classify `repo_path`'s `pre-push` slot without writing anything.
///
/// Why: the read-only half of the installer, so doctor and the installer share
/// one ownership judgement (see [`GuardState`]).
/// What: resolves the effective hooks dir, then classifies. A symlink is
/// [`GuardState::Foreign`] — `fs::write` follows symlinks and would rewrite
/// another manager's own file. Bytes are read as bytes, never as a `String`: a
/// foreign hook may legitimately not be UTF-8 (a compiled hook, latin-1 in a
/// comment), and only `ErrorKind::NotFound` means "absent". Every other read or
/// stat error is [`GuardState::Foreign`] — a file you cannot inspect is a file
/// you must not overwrite.
/// Test: `inspect_reports_missing_then_current`, `refuses_non_utf8_foreign_hook`,
/// `refuses_symlinked_hook`, `refuses_unreadable_hook`.
pub fn inspect_pre_push_guard(repo_path: &Path) -> GuardState {
    let hooks_dir = match effective_hooks_dir(repo_path) {
        Ok(d) => d,
        Err(reason) => return GuardState::Foreign(reason),
    };
    let hook_path = hooks_dir.join("pre-push");

    match std::fs::symlink_metadata(&hook_path) {
        Ok(meta) if meta.file_type().is_symlink() => {
            return GuardState::Foreign(format!(
                "{} is a symlink; another hook manager owns it",
                hook_path.display()
            ));
        }
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return GuardState::Missing(hook_path);
        }
        Err(e) => {
            return GuardState::Foreign(format!("cannot stat {}: {e}", hook_path.display()));
        }
    }

    match std::fs::read(&hook_path) {
        Ok(bytes) if !contains_marker(&bytes) => GuardState::Foreign(format!(
            "a non-trusty-mpm pre-push hook already exists at {}",
            hook_path.display()
        )),
        Ok(bytes) if bytes == PRE_PUSH_HOOK.as_bytes() => GuardState::Current(hook_path),
        Ok(_) => GuardState::Outdated(hook_path),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => GuardState::Missing(hook_path),
        Err(e) => GuardState::Foreign(format!(
            "cannot read existing {} ({e}); refusing to overwrite a hook that cannot be inspected",
            hook_path.display()
        )),
    }
}

/// Install the bundled cross-branch push guard into `repo_path`, idempotently.
///
/// Why: see the module docs — this is the only #2867 mitigation that fires for
/// a push issued by a process trusty-mpm did not create.
/// What: classifies the slot with [`inspect_pre_push_guard`], then writes
/// [`PRE_PUSH_HOOK`] with mode `0o755` only for [`GuardState::Missing`] and
/// [`GuardState::Outdated`]. [`GuardState::Foreign`] becomes
/// [`HookInstall::Refused`] — never an `Err`, and never an overwrite. Returns
/// `Err` only for genuine write-side I/O failures. Repeated calls are safe.
/// The write is atomic — temp file, chmod, `rename` — because this one file is
/// shared by every worktree of a base clone and a half-written shell script
/// exits non-zero, which git's hook contract reads as "refuse every push".
/// Test: `installs_then_reports_already_current`, `refuses_foreign_hook`,
/// `refuses_non_utf8_foreign_hook`, `refuses_symlinked_hook`, plus
/// `crates/trusty-mpm/tests/push_guard_hook.rs` end-to-end.
pub fn install_pre_push_guard(repo_path: &Path) -> Result<HookInstall, String> {
    let hook_path = match inspect_pre_push_guard(repo_path) {
        GuardState::Current(p) => return Ok(HookInstall::AlreadyCurrent(p)),
        GuardState::Foreign(reason) => return Ok(HookInstall::Refused(reason)),
        GuardState::Missing(p) | GuardState::Outdated(p) => p,
    };
    let hooks_dir = hook_path
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", hook_path.display()))?
        .to_path_buf();

    std::fs::create_dir_all(&hooks_dir)
        .map_err(|e| format!("failed to create {}: {e}", hooks_dir.display()))?;
    write_hook_atomically(&hooks_dir, &hook_path)?;
    Ok(HookInstall::Installed(hook_path))
}

/// Does `bytes` carry [`HOOK_MARKER`] anywhere in it?
///
/// Why: the ownership test must work on a hook that is not valid UTF-8, so it
/// cannot go through `str::contains`.
/// What: a byte-level substring search for the marker.
/// Test: `refuses_non_utf8_foreign_hook`, `reinstalls_over_an_older_trusty_mpm_revision`.
fn contains_marker(bytes: &[u8]) -> bool {
    let needle = HOOK_MARKER.as_bytes();
    bytes.windows(needle.len()).any(|w| w == needle)
}

/// Write [`PRE_PUSH_HOOK`] to `hook_path` without ever exposing a partial file.
///
/// Why: `hooks_dir` is `$GIT_COMMON_DIR/hooks`, shared by every worktree of the
/// base clone — on this repo, ~95 of them. An in-place `fs::write` truncates
/// first, so a concurrent `git push` can exec a truncated script; `sh` exits
/// non-zero and git's pre-push contract reads that as REFUSE, turning a partial
/// write into a fleet-wide push outage. `rename(2)` is atomic within a
/// filesystem and leaves an already-exec'ing shell reading the old inode.
/// What: writes `pre-push.tmp.<pid>` beside the target, chmods it 0o755, then
/// renames it over `hook_path`. The temp file is removed on any failure.
/// Test: `installs_then_reports_already_current` (mode + content survive the
/// rename), `atomic_write_leaves_no_temp_file_behind`.
fn write_hook_atomically(hooks_dir: &Path, hook_path: &Path) -> Result<(), String> {
    let tmp_path = hooks_dir.join(format!("pre-push.tmp.{}", std::process::id()));
    let cleanup = |e: String| {
        let _ = std::fs::remove_file(&tmp_path);
        e
    };
    std::fs::write(&tmp_path, PRE_PUSH_HOOK)
        .map_err(|e| cleanup(format!("failed to write {}: {e}", tmp_path.display())))?;
    set_executable(&tmp_path).map_err(cleanup)?;
    std::fs::rename(&tmp_path, hook_path).map_err(|e| {
        cleanup(format!(
            "failed to rename {} onto {}: {e}",
            tmp_path.display(),
            hook_path.display()
        ))
    })?;
    Ok(())
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

    /// A foreign hook that is not valid UTF-8 must be refused, not clobbered.
    ///
    /// Before the fix `read_to_string` returned `Err(InvalidData)` for these
    /// bytes and every `Err` fell through to an unconditional `fs::write`,
    /// destroying the file. Compiled hooks and shell hooks carrying latin-1
    /// bytes in a comment both land here.
    #[test]
    fn refuses_non_utf8_foreign_hook() {
        let Some((_dir, repo)) = temp_repo() else {
            return;
        };
        let hooks = effective_hooks_dir(&repo).expect("hooks dir");
        std::fs::create_dir_all(&hooks).expect("mkdir hooks");
        // Built at runtime, not as a `b"…"` literal: the compiler's
        // `invalid_from_utf8` lint sees through a literal and the assertion
        // below — which is what makes this test meaningful — would be a
        // deny-level warning.
        let mut foreign: Vec<u8> = b"#!/bin/sh\n# lefthook ".to_vec();
        foreign.extend_from_slice(&[0xff, 0xfe]);
        foreign.extend_from_slice(b" binary marker\nexit 0\n");
        assert!(
            std::str::from_utf8(&foreign).is_err(),
            "the fixture must genuinely not be UTF-8, or this test proves nothing"
        );
        std::fs::write(hooks.join("pre-push"), &foreign).expect("seed foreign hook");

        match install_pre_push_guard(&repo).expect("install must not hard-error") {
            HookInstall::Refused(reason) => {
                assert!(reason.contains("non-trusty-mpm"), "got: {reason}");
            }
            other => panic!("expected Refused, got {other:?}"),
        }
        assert_eq!(
            std::fs::read(hooks.join("pre-push")).expect("read"),
            foreign,
            "a non-UTF-8 foreign hook must be byte-identical after a refusal"
        );
    }

    /// A `pre-push` symlink belongs to whatever manager created it: writing
    /// through it would rewrite that manager's own file.
    #[cfg(unix)]
    #[test]
    fn refuses_symlinked_hook() {
        let Some((_dir, repo)) = temp_repo() else {
            return;
        };
        let hooks = effective_hooks_dir(&repo).expect("hooks dir");
        std::fs::create_dir_all(&hooks).expect("mkdir hooks");
        let target = hooks.join("managed-by-someone-else.sh");
        let target_body = "#!/bin/sh\n# husky\nexit 0\n";
        std::fs::write(&target, target_body).expect("seed target");
        std::os::unix::fs::symlink(&target, hooks.join("pre-push")).expect("symlink");

        match install_pre_push_guard(&repo).expect("install must not hard-error") {
            HookInstall::Refused(reason) => {
                assert!(reason.contains("symlink"), "got: {reason}");
            }
            other => panic!("expected Refused, got {other:?}"),
        }
        assert_eq!(
            std::fs::read_to_string(&target).expect("read target"),
            target_body,
            "the symlink target must never be written through"
        );
    }

    /// An unreadable existing hook must be refused, never overwritten: the
    /// installer cannot tell whose it is, so it must not touch it.
    #[cfg(unix)]
    #[test]
    fn refuses_unreadable_hook() {
        use std::os::unix::fs::PermissionsExt;
        let Some((_dir, repo)) = temp_repo() else {
            return;
        };
        let hooks = effective_hooks_dir(&repo).expect("hooks dir");
        std::fs::create_dir_all(&hooks).expect("mkdir hooks");
        let hook = hooks.join("pre-push");
        std::fs::write(&hook, "#!/bin/sh\nexit 0\n").expect("seed hook");
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o000)).expect("chmod 000");
        if std::fs::read(&hook).is_ok() {
            // Mode 0 does not bind for this uid (root, or an exotic
            // filesystem). Restore and skip rather than assert a lie.
            std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o644))
                .expect("restore");
            return;
        }

        let outcome = install_pre_push_guard(&repo).expect("install must not hard-error");
        // Restore before asserting so the temp dir can always be cleaned up.
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o644)).expect("restore");
        match outcome {
            HookInstall::Refused(reason) => {
                assert!(reason.contains("cannot read"), "got: {reason}");
            }
            other => panic!("expected Refused for an unreadable hook, got {other:?}"),
        }
        assert_eq!(
            std::fs::read_to_string(&hook).expect("read"),
            "#!/bin/sh\nexit 0\n",
            "an unreadable hook must be byte-identical after a refusal"
        );
    }

    /// The read-only probe must report the same ownership judgement the
    /// installer acts on — that shared judgement is what lets `tm doctor`
    /// report without mutating.
    #[test]
    fn inspect_reports_missing_then_current() {
        let Some((_dir, repo)) = temp_repo() else {
            return;
        };
        match inspect_pre_push_guard(&repo) {
            GuardState::Missing(p) => assert!(p.ends_with("hooks/pre-push"), "{}", p.display()),
            other => panic!("a fresh repo must report Missing, got {other:?}"),
        }
        install_pre_push_guard(&repo).expect("install");
        match inspect_pre_push_guard(&repo) {
            GuardState::Current(_) => {}
            other => panic!("after install the probe must report Current, got {other:?}"),
        }

        // An older revision must read as Outdated, not Current or Foreign.
        let hooks = effective_hooks_dir(&repo).expect("hooks dir");
        std::fs::write(
            hooks.join("pre-push"),
            "#!/bin/sh\n# trusty-mpm-push-guard: v0\nexit 0\n",
        )
        .expect("downgrade hook");
        match inspect_pre_push_guard(&repo) {
            GuardState::Outdated(_) => {}
            other => panic!("an older revision must report Outdated, got {other:?}"),
        }
    }

    /// The atomic write must not leave its temp file in the shared hooks dir.
    #[test]
    fn atomic_write_leaves_no_temp_file_behind() {
        let Some((_dir, repo)) = temp_repo() else {
            return;
        };
        install_pre_push_guard(&repo).expect("install");
        let hooks = effective_hooks_dir(&repo).expect("hooks dir");
        let leftovers: Vec<_> = std::fs::read_dir(&hooks)
            .expect("read hooks dir")
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.starts_with("pre-push.tmp."))
            .collect();
        assert!(
            leftovers.is_empty(),
            "the atomic write must clean up after itself, found: {leftovers:?}"
        );
    }
}
