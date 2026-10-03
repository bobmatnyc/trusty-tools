//! Test-only helpers for credential tests: an environment guard and a
//! redacting assertion (#9121, #9123).
//!
//! Why: a credential test that mutates the environment without restoring it
//! sends a sibling test to the operator's real store, and an `assert_eq!` on a
//! resolved value prints that value when it fails. Both happened: a real
//! Telegram token reached test output (#9121, #9123).
//! What: [`EnvVarGuard`] sets or removes one variable and restores it on drop;
//! callers are `#[serial]`. [`assert_secret_eq`] compares a resolved value and,
//! on failure, prints only `trusty_common::credentials::redact_secret`'s
//! non-reversible preview — modelled on trusty-common `resolver.rs`'s
//! `assert_resolved`.
//! Test: `a_failed_secret_assert_never_prints_the_value`,
//! `env_var_guard_restores_the_prior_state`.

use trusty_common::credentials::redact_secret;

/// Sets or removes one environment variable until it drops, then restores it.
///
/// Why: see the module docs. Every user is `#[serial]`, which is what makes
/// the unsafe set/remove sound.
pub(crate) struct EnvVarGuard {
    /// The variable this guard owns.
    key: &'static str,
    /// Its value before the guard took it, if it had one.
    prev: Option<std::ffi::OsString>,
}

impl EnvVarGuard {
    /// Take `key` out of the environment until the guard drops.
    pub(crate) fn unset(key: &'static str) -> Self {
        let prev = std::env::var_os(key);
        // SAFETY: every user of this guard is `#[serial]`, so no other thread
        // races the remove/restore. Restored in `Drop`.
        unsafe { std::env::remove_var(key) };
        Self { key, prev }
    }

    /// Set `key` to `value` until the guard drops.
    pub(crate) fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
        let prev = std::env::var_os(key);
        // SAFETY: as `unset`.
        unsafe { std::env::set_var(key, value) };
        Self { key, prev }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        // SAFETY: as the constructors.
        unsafe {
            match &self.prev {
                Some(v) => std::env::set_var(self.key, v),
                None => std::env::remove_var(self.key),
            }
        }
    }
}

/// Render a resolved credential for a failure message without disclosing it.
fn describe(value: Option<&str>) -> String {
    match value {
        None => "None".to_string(),
        Some(v) => format!("Some({})", redact_secret(v)),
    }
}

/// Assert a resolved credential equals `expected`, redacting `actual` on
/// failure.
///
/// Why: `actual` may hold a real credential the test reached by accident;
/// only the `expected` side is a test literal and safe to print verbatim.
/// Test: `a_failed_secret_assert_never_prints_the_value`.
#[track_caller]
pub(crate) fn assert_secret_eq(actual: Option<&str>, expected: Option<&str>, what: &str) {
    assert!(
        actual == expected,
        "{what}: expected {expected:?}, got {} (actual value redacted)",
        describe(actual)
    );
}

#[cfg(test)]
mod tests {
    use serial_test::serial;

    use super::*;

    /// Why: the leak lived in the FAILURE path, so a passing run proves
    /// nothing; provoke a failure and read the message.
    /// Test: this test.
    #[test]
    fn a_failed_secret_assert_never_prints_the_value() {
        let secret = "9121000000:fake-value-0123456789abcdef";
        let panic = std::panic::catch_unwind(|| {
            assert_secret_eq(Some(secret), Some("expected"), "probe");
        })
        .expect_err("a mismatch must panic");
        let msg = panic
            .downcast_ref::<String>()
            .expect("assert! panics with a String payload");

        assert!(!msg.contains(secret), "the message echoed the value");
        assert!(!msg.contains("0123456789"), "the message echoed its tail");
        assert!(msg.contains("expected"), "the message lost the expectation");
    }

    /// Test: this test.
    #[test]
    #[serial]
    fn env_var_guard_restores_the_prior_state() {
        const VAR: &str = "TM_TEST_ENV_GUARD_9121";
        let outer = EnvVarGuard::unset(VAR);
        {
            let _set = EnvVarGuard::set(VAR, "inner");
            assert_eq!(std::env::var(VAR).as_deref(), Ok("inner"));
        }
        assert!(std::env::var_os(VAR).is_none(), "absent was not restored");
        drop(outer);
    }
}
