//! Environment variable compatibility shim: TAGENT_* with OPEN_MPM_* fallback.
//!
//! Why: The rename from open-mpm → trusty-agents renames all OPEN_MPM_* env
//!      vars to TAGENT_*. Existing user environments (launchd plists, scripts,
//!      shell profiles, CI) still set the old OPEN_MPM_* names. Rather than
//!      silently breaking those environments, this module provides a single
//!      read helper that tries the new TAGENT_* name first and falls back to
//!      the deprecated OPEN_MPM_* old name, emitting a `tracing::warn!` so
//!      users see the migration notice in logs.
//! What: `env_var(new_key, old_key)` → reads `new_key`; if absent, tries
//!      `old_key`, warns once (via `tracing::warn!`), and returns the old
//!      value. Returns `Err` only when both are absent.
//! Test: Unit tests cover all three code paths (new-only, old-only, neither).

use std::ffi::OsStr;

/// Read `new_key`; fall back to `old_key` with a deprecation warning.
///
/// Why: Centralises the two-step env-var read so individual call sites stay
///      one-liners and the deprecation notice is always emitted.
/// What: Calls `std::env::var(new_key)`. On `VarError::NotPresent` tries
///       `old_key`; if that succeeds, emits a `tracing::warn!` and returns
///       the old value. Returns `Err(VarError::NotPresent)` if neither is set.
/// Test: `env_compat::env_var_new_wins`, `env_compat::env_var_old_fallback`,
///       `env_compat::env_var_neither_absent`.
pub fn env_var(new_key: &str, old_key: &str) -> Result<String, std::env::VarError> {
    match std::env::var(new_key) {
        Ok(v) => Ok(v),
        Err(std::env::VarError::NotPresent) => match std::env::var(old_key) {
            Ok(v) => {
                tracing::warn!("{old_key} is deprecated; rename to {new_key} in your environment");
                Ok(v)
            }
            Err(e) => Err(e),
        },
        Err(e) => Err(e),
    }
}

/// Read `new_key` as an OsString; fall back to `old_key` with a deprecation
/// warning. Mirrors `std::env::var_os` semantics (non-UTF8 safe).
///
/// Why: Some call sites use `var_os` for paths that may contain non-UTF8
///      bytes; the shim must preserve that property.
/// What: Tries `new_key`, then `old_key` with warn, then returns `None`.
/// Test: `env_compat::env_var_os_new_wins`, `env_compat::env_var_os_old_fallback`.
pub fn env_var_os(new_key: &str, old_key: &str) -> Option<std::ffi::OsString> {
    if let Some(v) = std::env::var_os(new_key) {
        return Some(v);
    }
    if let Some(v) = std::env::var_os(OsStr::new(old_key)) {
        tracing::warn!("{old_key} is deprecated; rename to {new_key} in your environment");
        return Some(v);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    /// Regression test: when only the legacy OPEN_MPM_* var is set, env_var
    /// must return the legacy value (not Err).
    ///
    /// Why: Bug #858 — every call site had identical new/old args so the
    /// OPEN_MPM_* fallback could never fire. This test proves the fallback
    /// path works end-to-end.
    /// Test: Sets only the OLD var, calls env_var(new, old), expects Ok(legacy_value).
    #[test]
    #[serial]
    fn env_var_legacy_open_mpm_fallback_resolves() {
        const NEW: &str = "__TAGENT_COMPAT_TEST_NEW";
        const OLD: &str = "__OPEN_MPM_COMPAT_TEST_OLD";
        const VALUE: &str = "legacy-value-from-open-mpm";

        // Ensure new var is absent, old var is present.
        // SAFETY: serialised by `serial`; no other thread reads these vars.
        unsafe {
            std::env::remove_var(NEW);
            std::env::set_var(OLD, VALUE);
        }

        let result = env_var(NEW, OLD);

        // Restore env before any assertion can panic.
        unsafe {
            std::env::remove_var(OLD);
        }

        assert_eq!(
            result.expect("env_var should fall back to OLD when NEW is absent"),
            VALUE,
            "env_var must return the legacy OPEN_MPM_* value when only that var is set"
        );
    }

    /// Regression test: new TAGENT_* var wins over legacy OPEN_MPM_* var when both set.
    ///
    /// Why: Priority order must be new > old. Ensures we don't accidentally
    /// return the old value when users have already migrated.
    /// Test: Sets both vars to different values, expects the NEW var's value.
    #[test]
    #[serial]
    fn env_var_new_wins_over_legacy() {
        const NEW: &str = "__TAGENT_COMPAT_TEST_NEW2";
        const OLD: &str = "__OPEN_MPM_COMPAT_TEST_OLD2";
        const NEW_VALUE: &str = "tagent-value";
        const OLD_VALUE: &str = "open-mpm-value";

        // SAFETY: serialised by `serial`.
        unsafe {
            std::env::set_var(NEW, NEW_VALUE);
            std::env::set_var(OLD, OLD_VALUE);
        }

        let result = env_var(NEW, OLD);

        unsafe {
            std::env::remove_var(NEW);
            std::env::remove_var(OLD);
        }

        assert_eq!(
            result.expect("env_var should succeed when new key is set"),
            NEW_VALUE,
            "new TAGENT_* var must take priority over legacy OPEN_MPM_* var"
        );
    }

    /// Regression test: Err when neither new nor old var is set.
    ///
    /// Why: Callers depend on Err meaning "completely absent" — not "absent
    /// with a stale fallback". Confirms the no-op branch returns Err.
    /// Test: Both vars absent → result is Err.
    #[test]
    fn env_var_both_absent_returns_err() {
        let result = env_var("__TAGENT_TEST_NEW_ABSENT", "__OPEN_MPM_TEST_OLD_ABSENT");
        assert!(
            result.is_err(),
            "env_var must return Err when neither key is set"
        );
    }

    /// Regression test (config-dir): default_bundled_config_dir returns the
    /// legacy `.open-mpm` path when `.trusty-agents` is absent but `.open-mpm`
    /// exists.
    ///
    /// Why: Bug #858 — the legacy dir was `PathBuf::from(".trusty-agents")`
    /// instead of `PathBuf::from(".open-mpm")`, so the migration fallback
    /// could never match the pre-rename directory on disk.
    /// Test: Creates a tempdir with only `.open-mpm/` present, sets cwd,
    /// calls default_bundled_config_dir, expects `.open-mpm` path returned.
    #[test]
    #[serial]
    fn config_dir_migration_returns_legacy_open_mpm_when_new_absent() {
        use std::path::PathBuf;
        use tempfile::TempDir;

        let tmp = TempDir::new().expect("tempdir");
        // Create only the OLD dir name.
        let old_dir = tmp.path().join(".open-mpm");
        std::fs::create_dir_all(&old_dir).expect("create .open-mpm");

        // Ensure TAGENT_CONFIG_DIR / OPEN_MPM_CONFIG_DIR are unset so we
        // exercise the fallback path, not the env-var path.
        // SAFETY: serialised by `serial`; no other thread reads these vars.
        // remove_var remains unsafe (requires single-threaded context).
        unsafe {
            std::env::remove_var("TAGENT_CONFIG_DIR");
            std::env::remove_var("OPEN_MPM_CONFIG_DIR");
        }

        // Change cwd into the tempdir so the relative `PathBuf::from(".open-mpm")`
        // check hits our created directory.
        let orig_cwd = std::env::current_dir().ok();
        // set_current_dir is safe in Rust 2024 — no `unsafe` block needed.
        std::env::set_current_dir(tmp.path()).expect("set cwd");

        let result = crate::default_bundled_config_dir();

        // Restore cwd and env before asserting.
        if let Some(cwd) = orig_cwd {
            let _ = std::env::set_current_dir(&cwd);
        }

        // The result should be `.open-mpm` (relative) — matching old_dir.
        assert_eq!(
            result,
            PathBuf::from(".open-mpm"),
            "default_bundled_config_dir must return .open-mpm when .trusty-agents absent and .open-mpm present"
        );
    }
}
