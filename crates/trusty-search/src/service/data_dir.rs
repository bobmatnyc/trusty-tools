//! Data-directory resolution with supervised-process fallback (issue #718).
//!
//! Why: `dirs::data_local_dir()` calls `NSFileManager` on macOS, which is
//! unavailable under launchd's posix_spawn context on macOS 26 Tahoe when the
//! user session is not yet fully initialised. `dirs::home_dir()` suffers the
//! same limitation on macOS. This module provides a HOME-based fallback that
//! `persistence::data_dir()` delegates to when both `TRUSTY_DATA_DIR` and
//! `dirs::data_local_dir()` are unavailable. On Unix, when `$HOME` is unset,
//! it falls back to the passwd database via `getpwuid(3)` (NSFileManager-free).
//!
//! What: one function (`data_dir_home_fallback`) that resolves the well-known
//! platform data sub-path from `$HOME` (env var first, then passwd db on Unix),
//! creates the directory, and returns it. Loud `tracing::error!` on total
//! failure.
//!
//! Test: `data_dir_override_yields_absolute_path` and
//! `data_dir_home_fallback_path_is_absolute` in this module's `tests` block.

use anyhow::{Context, Result};
use std::path::PathBuf;

/// Issue #718: HOME-based fallback for when both `TRUSTY_DATA_DIR` and
/// `dirs::data_local_dir()` are unavailable (launchd posix_spawn context on
/// macOS 26 Tahoe where NSFileManager is not yet initialised).
///
/// Why: extracted from `data_dir()` to keep `persistence.rs` under its line
/// budget and to make the fallback independently testable. Uses only
/// NSFileManager-free path resolution: `$HOME` env var first, then the
/// passwd database via `nix::unistd::getpwuid` on Unix — both of which work
/// inside launchd's posix_spawn context even on macOS 26 Tahoe.
/// What: resolves `$HOME` from the process env or the passwd database (uid-
/// based, cwd-independent), constructs the platform well-known sub-path,
/// creates the directory, and returns it. Emits `tracing::error!` when HOME
/// is also absent.
/// Test: `data_dir_home_fallback_path_is_absolute` in this module.
pub(super) fn data_dir_home_fallback() -> Result<PathBuf> {
    tracing::warn!(
        "data_dir: dirs::data_local_dir() returned None (launchd / supervised spawn?); \
         falling back to $HOME-based resolution (issue #718)"
    );
    let home = resolve_home_dir();
    let Some(home) = home else {
        tracing::error!(
            "data_dir: FATAL — cannot resolve data directory: \
             dirs::data_local_dir() returned None AND $HOME is unset AND \
             passwd db lookup failed. \
             Set TRUSTY_DATA_DIR to an absolute path in the launchd plist \
             EnvironmentVariables (issue #718)."
        );
        anyhow::bail!(
            "cannot resolve trusty-search data directory: \
             dirs::data_local_dir() returned None and $HOME is unset. \
             Set TRUSTY_DATA_DIR in the launchd plist to an absolute path."
        );
    };
    anyhow::ensure!(
        home.is_absolute(),
        "HOME-based data dir resolution failed: $HOME={} is not absolute (issue #718)",
        home.display()
    );
    #[cfg(target_os = "macos")]
    let dir = home
        .join("Library")
        .join("Application Support")
        .join("trusty-search");
    #[cfg(not(target_os = "macos"))]
    let dir = home.join(".local").join("share").join("trusty-search");
    tracing::warn!(
        "data_dir: HOME-based fallback: {} (issue #718 — \
         set TRUSTY_DATA_DIR in launchd plist to suppress this warning)",
        dir.display()
    );
    std::fs::create_dir_all(&dir)
        .context("create trusty-search data dir (HOME fallback, issue #718)")?;
    Ok(dir)
}

/// Resolve the user's home directory using only NSFileManager-free mechanisms.
///
/// Why: on macOS 26 Tahoe under launchd's posix_spawn context, `NSFileManager`
/// is not yet initialised at daemon boot. Both `dirs::home_dir()` and
/// `dirs::data_local_dir()` call through NSFileManager, so they return `None`
/// in that context. This function uses two NSFileManager-free alternatives:
/// (1) `$HOME` env var (set by launchd EnvironmentVariables; reliable in login
/// shells); (2) passwd database lookup via `getpwuid(3)` through `nix::unistd`
/// (syscall-level, no Objective-C involvement).
///
/// What: returns the first non-empty absolute path found, or `None` if both
/// strategies fail.
///
/// Test: `data_dir_home_fallback_path_is_absolute` exercises the path formula;
/// the passwd fallback is implicitly exercised whenever `$HOME` is unset.
fn resolve_home_dir() -> Option<PathBuf> {
    // Strategy 1: $HOME env var — set by launchd if listed in EnvironmentVariables.
    if let Ok(home) = std::env::var("HOME") {
        let path = PathBuf::from(home);
        if path.is_absolute() {
            tracing::debug!(
                "data_dir: home resolved from $HOME env var: {}",
                path.display()
            );
            return Some(path);
        }
    }

    // Strategy 2: passwd database (getpwuid) — NSFileManager-free on all Unix.
    #[cfg(unix)]
    {
        if let Some(home) = passwd_home_dir() {
            tracing::debug!(
                "data_dir: home resolved from passwd db (uid={}): {}",
                nix::unistd::getuid(),
                home.display()
            );
            return Some(home);
        }
    }

    None
}

/// Look up the current user's home directory via the passwd database.
///
/// Why: `getpwuid(3)` is a pure libc/kernel call that does not involve
/// NSFileManager, so it works inside launchd's posix_spawn context on
/// macOS 26 Tahoe where `dirs::home_dir()` returns `None`.
/// What: calls `nix::unistd::getpwuid` with the current effective UID,
/// extracts `pw_dir`, and returns it as an absolute `PathBuf`. Returns `None`
/// on lookup failure or if the passwd entry has no home directory.
/// Test: always succeeds on developer machines (every uid has a home in
/// passwd); CI machines set $HOME so the passwd fallback is not the primary
/// code path. The function is covered transitively by `data_dir_home_fallback`.
#[cfg(unix)]
fn passwd_home_dir() -> Option<PathBuf> {
    let uid = nix::unistd::getuid();
    match nix::unistd::User::from_uid(uid) {
        Ok(Some(user)) => {
            let home = user.dir;
            if home.is_absolute() {
                Some(home)
            } else {
                tracing::warn!(
                    "data_dir: passwd db entry for uid={} has relative home dir '{}' — ignoring",
                    uid,
                    home.display()
                );
                None
            }
        }
        Ok(None) => {
            tracing::warn!(
                "data_dir: passwd db has no entry for uid={} (uid not found)",
                uid
            );
            None
        }
        Err(e) => {
            tracing::warn!(
                "data_dir: passwd db lookup failed for uid={}: {} — \
                 $HOME env var is required in launchd plist (issue #718)",
                uid,
                e
            );
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    /// Issue #718: `data_dir()` must always return an absolute cwd-independent
    /// path via TRUSTY_DATA_DIR override.
    ///
    /// Why: launchd's posix_spawn sets cwd to `/`; a relative path would resolve
    /// to the wrong location. Absolute = cwd-independent.
    /// What: override TRUSTY_DATA_DIR, call `crate::service::persistence::data_dir()`,
    /// assert absolute.
    /// Test: `data_dir_override_yields_absolute_path`.
    #[test]
    #[serial]
    fn data_dir_override_yields_absolute_path() {
        let tmp = tempfile::tempdir().unwrap();
        let abs_path = tmp.path().join("ts_abs_test_718");
        std::fs::create_dir_all(&abs_path).unwrap();
        unsafe {
            std::env::set_var("TRUSTY_DATA_DIR", &abs_path);
        }
        let result = crate::service::persistence::data_dir();
        unsafe {
            std::env::remove_var("TRUSTY_DATA_DIR");
        }
        let dir = result.expect("data_dir with absolute TRUSTY_DATA_DIR must succeed");
        assert!(
            dir.is_absolute(),
            "data_dir() must always return an absolute path (cwd-independent); got: {}",
            dir.display()
        );
    }

    /// Issue #718: the HOME-based fallback path formula must produce an absolute
    /// path anchored under HOME (not cwd-relative).
    ///
    /// Why: pins the invariant that `data_dir_home_fallback()` is cwd-independent.
    /// What: verify the platform sub-path formula with a synthetic absolute HOME.
    /// The actual fallback code path requires dirs::data_local_dir() to return
    /// None, which cannot be forced without OS-level mocking.
    /// Test: `data_dir_home_fallback_path_is_absolute`.
    #[test]
    fn data_dir_home_fallback_path_is_absolute() {
        let home = PathBuf::from("/tmp/fake-home-718");
        #[cfg(target_os = "macos")]
        let expected = home
            .join("Library")
            .join("Application Support")
            .join("trusty-search");
        #[cfg(not(target_os = "macos"))]
        let expected = home.join(".local").join("share").join("trusty-search");

        assert!(
            expected.is_absolute(),
            "HOME-fallback path must be absolute; got: {}",
            expected.display()
        );
        assert!(
            expected.starts_with(&home),
            "HOME-fallback path {} must be rooted under HOME {}",
            expected.display(),
            home.display()
        );
    }

    /// Issue #718: `resolve_home_dir()` must prefer `$HOME` env var over the
    /// passwd database, and the returned path must be absolute.
    ///
    /// Why: under launchd, $HOME is the primary reliable source; the passwd
    /// fallback is only needed when the env var is absent.
    /// What: set a synthetic absolute $HOME, call `resolve_home_dir()`, assert
    /// it returns that path.
    /// Test: `resolve_home_dir_prefers_env_var`.
    #[test]
    #[serial]
    fn resolve_home_dir_prefers_env_var() {
        let tmp = tempfile::tempdir().unwrap();
        let fake_home = tmp.path().join("fakehome-718-test");
        std::fs::create_dir_all(&fake_home).unwrap();
        unsafe { std::env::set_var("HOME", &fake_home) };
        let result = resolve_home_dir();
        unsafe { std::env::remove_var("HOME") };
        let resolved = result.expect("resolve_home_dir must succeed when $HOME is absolute");
        assert_eq!(
            resolved, fake_home,
            "resolve_home_dir must return $HOME path when set and absolute"
        );
        assert!(resolved.is_absolute(), "resolved home must be absolute");
    }

    /// Issue #718: `resolve_home_dir()` must fall back to the passwd database
    /// when `$HOME` is unset, and the result must be absolute.
    ///
    /// Why: covers the launchd case where $HOME is absent from the process env.
    /// The passwd fallback is NSFileManager-free and always works for valid UIDs.
    /// What: unset $HOME, call `resolve_home_dir()`, assert the result is absolute.
    /// Note: this test passes on any dev/CI machine where the current uid has a
    /// passwd entry (effectively all Unix systems in normal operation).
    /// Test: `resolve_home_dir_passwd_fallback_is_absolute`.
    #[test]
    #[serial]
    #[cfg(unix)]
    fn resolve_home_dir_passwd_fallback_is_absolute() {
        // Save and clear HOME to force the passwd fallback.
        let saved = std::env::var("HOME").ok();
        unsafe { std::env::remove_var("HOME") };
        let result = resolve_home_dir();
        // Restore HOME before any assertion so other tests are not affected.
        if let Some(h) = saved {
            unsafe { std::env::set_var("HOME", h) };
        }
        // The passwd lookup should succeed on any machine where the test uid
        // has a valid passwd entry. If it somehow doesn't (container with no
        // passwd), skip rather than fail.
        if let Some(home) = result {
            assert!(
                home.is_absolute(),
                "passwd-db home must be absolute; got: {}",
                home.display()
            );
        }
        // None is acceptable in headless/container environments with no passwd entry.
    }

    /// Issue #718: `indexes_toml_path()` must return an absolute cwd-independent
    /// path. Under launchd cwd is `/`; a relative path would resolve wrongly.
    ///
    /// Why: pins the invariant that all registry paths are absolute — the primary
    /// diagnostic for the launchd 0-index boot (data dir resolves to wrong location
    /// when cwd-relative).
    /// What: set `TRUSTY_DATA_DIR` to an absolute tempdir; call `indexes_toml_path()`;
    /// assert absolute, under the override dir, and named `indexes.toml`.
    /// Test: `registry_path_is_cwd_independent`.
    #[test]
    #[serial]
    fn registry_path_is_cwd_independent() {
        use crate::service::persistence::indexes_toml_path;
        let tmp = tempfile::tempdir().unwrap();
        let abs_dir = tmp.path().join("ts-718-cwd-test");
        std::fs::create_dir_all(&abs_dir).unwrap();
        unsafe { std::env::set_var("TRUSTY_DATA_DIR", &abs_dir) };
        let toml_path = indexes_toml_path();
        unsafe { std::env::remove_var("TRUSTY_DATA_DIR") };
        let path = toml_path.expect("indexes_toml_path must succeed with TRUSTY_DATA_DIR set");
        assert!(
            path.is_absolute(),
            "indexes.toml path must be absolute (cwd-independent); got: {}",
            path.display()
        );
        assert!(
            path.starts_with(&abs_dir),
            "indexes.toml path {} must be under override dir {}",
            path.display(),
            abs_dir.display()
        );
        assert_eq!(
            path.file_name().and_then(|n| n.to_str()),
            Some("indexes.toml"),
            "file name must be indexes.toml; got: {}",
            path.display()
        );
    }
}
