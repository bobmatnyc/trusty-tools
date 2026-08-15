//! The ONE test-only RAII guard for mutating a process environment variable
//! and restoring it on drop.
//!
//! Why (#3451): three independent copies of this exact guard accumulated —
//! one in `credentials::resolver::tests`, one in
//! `memory_core::dream::tests`, one in `memory_core::semantic_consolidation`'s
//! test module — each protecting the same process-wide hazard (env vars are
//! global state; the test harness runs tests in parallel threads within one
//! binary, and `OPENROUTER_API_KEY` / the credential-provider vars are each
//! read and written by tests in more than one of those three files). Three
//! subtly different guards around one hazard is how the mutation race the
//! guard exists to prevent stayed alive: a fourth copy, or a fix applied to
//! only one of the three, was one accidental omission away. This module is
//! the merge of all three call sites onto a single implementation.
//!
//! What: [`EnvVarGuard`] captures the variable's prior value on construction,
//! applies the new value (or removes the variable) for the guard's lifetime,
//! and reinstates the prior value — or removes the variable again if there
//! was none — on [`Drop`]. It carries no lock of its own: every caller across
//! all three call sites already joins the shared `#[serial(dotenv_credential_env)]`
//! group (see `credentials::resolver::tests`, `memory_core::dream::tests`,
//! and `memory_core::semantic_consolidation::tests`), which is what
//! serialises access to the real process environment across test threads
//! and across files. That external-lock contract is identical in all three
//! prior copies, so merging them changes no caller's behavior.
//!
//! Test: exercised transitively by every test in the three modules listed
//! above; `env_var_guard_restores_absent_as_absent` and
//! `env_var_guard_restores_prior_value` pin the restore contract directly.

/// RAII guard over one process-wide environment variable, for tests only.
///
/// Why/What: see the module docs.
pub(crate) struct EnvVarGuard {
    key: &'static str,
    previous: Option<String>,
}

impl EnvVarGuard {
    /// Set `key` to `value` for the guard's lifetime, restoring the prior
    /// value (or absence) on drop.
    ///
    /// The caller MUST already hold the `dotenv_credential_env` serial group
    /// — see the module docs — for the whole window the guard is live.
    pub(crate) fn set(key: &'static str, value: &str) -> Self {
        let previous = std::env::var(key).ok();
        // SAFETY: every caller is `#[serial(dotenv_credential_env)]`, so no
        // other thread reads or writes the environment concurrently.
        unsafe { std::env::set_var(key, value) };
        Self { key, previous }
    }

    /// Remove `key` for the guard's lifetime, restoring the prior value (or
    /// absence) on drop.
    ///
    /// Why a test reaches for this instead of just assuming the variable is
    /// unset: the ambient shell environment is not a fixture the test
    /// controls (#4407) — a test asserting "behavior X holds when this var is
    /// absent" must make it absent itself, not assume the machine happens to
    /// agree.
    pub(crate) fn remove(key: &'static str) -> Self {
        let previous = std::env::var(key).ok();
        // SAFETY: see `EnvVarGuard::set`.
        unsafe { std::env::remove_var(key) };
        Self { key, previous }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        // SAFETY: see `EnvVarGuard::set`.
        unsafe {
            match self.previous.take() {
                Some(v) => std::env::set_var(self.key, v),
                None => std::env::remove_var(self.key),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::EnvVarGuard;
    use serial_test::serial;

    /// Why: the restore-on-drop contract must put back exactly what was
    /// there before, not a default or an empty string.
    /// What: sets a var with a known prior value, drops the guard, asserts
    /// the prior value came back.
    /// Test: this test.
    #[test]
    #[serial(dotenv_credential_env)]
    fn env_var_guard_restores_prior_value() {
        const KEY: &str = "TRUSTY_COMMON_ENV_GUARD_TEST_PRIOR";
        // SAFETY: `#[serial(dotenv_credential_env)]` above.
        unsafe { std::env::set_var(KEY, "original") };

        {
            let _guard = EnvVarGuard::set(KEY, "overwritten");
            assert_eq!(std::env::var(KEY).as_deref(), Ok("overwritten"));
        }

        assert_eq!(
            std::env::var(KEY).as_deref(),
            Ok("original"),
            "drop must restore the exact prior value"
        );
        // SAFETY: `#[serial(dotenv_credential_env)]` above.
        unsafe { std::env::remove_var(KEY) };
    }

    /// Why: a variable that was ABSENT before the guard must come back
    /// absent, not present-with-some-value — the distinction the task
    /// description calls out as potentially load-bearing.
    /// What: ensures the var starts absent, applies a value through the
    /// guard, drops it, asserts the var is absent again.
    /// Test: this test.
    #[test]
    #[serial(dotenv_credential_env)]
    fn env_var_guard_restores_absent_as_absent() {
        const KEY: &str = "TRUSTY_COMMON_ENV_GUARD_TEST_ABSENT";
        // SAFETY: `#[serial(dotenv_credential_env)]` above.
        unsafe { std::env::remove_var(KEY) };
        assert!(std::env::var(KEY).is_err(), "precondition: var is absent");

        {
            let _guard = EnvVarGuard::set(KEY, "temporary");
            assert_eq!(std::env::var(KEY).as_deref(), Ok("temporary"));
        }

        assert!(
            std::env::var(KEY).is_err(),
            "drop must restore absence, not leave an empty or stale value"
        );
    }

    /// Why: `remove` is the other half of the contract — used when a test
    /// needs the var unset for its body regardless of ambient state.
    /// What: sets a prior value, uses `remove` to clear it for the guard's
    /// lifetime, asserts it is absent while the guard lives and restored on
    /// drop.
    /// Test: this test.
    #[test]
    #[serial(dotenv_credential_env)]
    fn env_var_guard_remove_then_restores() {
        const KEY: &str = "TRUSTY_COMMON_ENV_GUARD_TEST_REMOVE";
        // SAFETY: `#[serial(dotenv_credential_env)]` above.
        unsafe { std::env::set_var(KEY, "was-here") };

        {
            let _guard = EnvVarGuard::remove(KEY);
            assert!(std::env::var(KEY).is_err(), "guard must clear the var");
        }

        assert_eq!(
            std::env::var(KEY).as_deref(),
            Ok("was-here"),
            "drop must restore the value `remove` displaced"
        );
    }
}
