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
    /// Test: Creates a tempdir with only `.open-mpm/` present, calls
    /// `default_bundled_config_dir_checking` rooted at that tempdir, expects
    /// `.open-mpm` path returned.
    ///
    /// (#3516) This test used to `std::env::set_current_dir` into the
    /// tempdir for the duration of the check, relying on `#[serial]` to
    /// keep it from racing every OTHER test in this crate — but CWD is
    /// process-global exactly like an env var, and at least two OTHER test
    /// modules (`api::server::tests` handler tests) resolve their own
    /// COMMON-relative paths (`.trusty-agents/projects`,
    /// `.trusty-agents/agents`) against CWD while holding a DIFFERENT lock
    /// (`HOME_LOCK`, which was never meant to guard CWD) — so `#[serial]`
    /// alone did not actually protect them. `default_bundled_config_dir_checking`
    /// (`lib.rs`) now takes the existence-check root as an explicit
    /// parameter, so this test points it directly at the tempdir — no CWD
    /// mutation, no `#[serial]`, and no lock of any kind is needed, and it
    /// can never race any other test again.
    #[test]
    fn config_dir_migration_returns_legacy_open_mpm_when_new_absent() {
        use std::path::PathBuf;
        use tempfile::TempDir;

        let tmp = TempDir::new().expect("tempdir");
        // Create only the OLD dir name.
        let old_dir = tmp.path().join(".open-mpm");
        std::fs::create_dir_all(&old_dir).expect("create .open-mpm");

        // Ensure TAGENT_CONFIG_DIR / OPEN_MPM_CONFIG_DIR are unset so we
        // exercise the fallback path, not the env-var path. Reading (not
        // mutating) these is safe unguarded; this only removes them if
        // already absent from THIS test's perspective, matching the
        // production default when neither is set. Any test that legitimately
        // sets these itself does so under its own guard, so this is not a
        // shared-state hazard.
        let had_new = std::env::var_os("TAGENT_CONFIG_DIR");
        let had_old = std::env::var_os("OPEN_MPM_CONFIG_DIR");
        if had_new.is_none() && had_old.is_none() {
            let result = crate::default_bundled_config_dir_checking(tmp.path());
            assert_eq!(
                result,
                PathBuf::from(".open-mpm"),
                "default_bundled_config_dir must return .open-mpm when .trusty-agents absent and .open-mpm present"
            );
        } else {
            eprintln!(
                "skipping config_dir_migration_returns_legacy_open_mpm_when_new_absent: \
                 TAGENT_CONFIG_DIR/OPEN_MPM_CONFIG_DIR set in this environment"
            );
        }
    }
}
