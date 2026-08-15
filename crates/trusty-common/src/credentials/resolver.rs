//! Composite credential resolver (issue #2401; promoted out of `inference::`
//! by #4564).
//!
//! Why: every credential consumer needs the same 3-tier answer to
//! "what is provider X's secret" — process env var, then `.env.local`,
//! then the secure store — checked in that exact order every time.
//! What: [`resolve_key`] is the production entry point (loads `.env.local`
//! once, then delegates to [`resolve_key_with`] against
//! [`default_store`]). The provider→env-var-name mapping lives in
//! [`super::registry`].
//! [`resolve_key_with`] is the hermetic, store-injectable core: it checks
//! `std::env::var` (which — because `dotenvy` never overrides an existing
//! var — transparently reflects "process env" *or* "already-loaded
//! `.env.local`", whichever tier actually populated it) before falling
//! through to the injected [`super::KeyStore`]. This is why there is no
//! separate ".env.local tier" function: the tier is realised entirely by
//! *when* `.env.local` gets loaded relative to the `std::env::var` check,
//! not by a distinct code path.
//! Test: `resolver_tests` (sibling file) exercises all three tiers plus the
//! absent-tier fallthrough using [`resolve_key_with`] directly against a
//! `MemoryKeyStore`, so no test depends on `load_env_local_once`'s
//! once-per-process semantics or the real cwd.

use super::KeyStore;
use super::dotenv;
use super::file_store::FileKeyStore;
#[cfg(feature = "keyring-store")]
use super::keyring_store::KeyringStore;
use super::memory_store::MemoryKeyStore;
use super::registry::env_var_for;

/// Resolve `provider`'s credential using the full 3-tier precedence.
///
/// Why: the single production entry point every future `InferenceAdapter`
/// construction path calls.
/// What: loads `.env.local` once (see [`dotenv::load_env_local_once`]), then
/// delegates to [`resolve_key_with`] against [`default_store`].
/// Test: exercised via `resolve_key_with` (the pure core) in
/// `resolver_tests`; the `load_env_local_once` + `default_store` wiring is
/// intentionally not independently unit tested — see their own docs for why.
pub fn resolve_key(provider: &str) -> Option<String> {
    dotenv::load_env_local_once();
    resolve_key_with(provider, default_store().as_ref())
}

/// Hermetic core: env-var tier, then `store`.
///
/// Why: separated from [`resolve_key`] so tests can inject a
/// [`MemoryKeyStore`] and control the process environment directly, without
/// ever touching the real filesystem, `$HOME`, or an OS keychain.
/// What: returns the env var's value when [`env_var_for`] maps `provider`
/// and that var is set to a non-empty value; otherwise `store.get(provider)`.
/// Test: `resolver_tests::env_beats_store`,
/// `resolver_tests::dotenv_loaded_value_beats_store`,
/// `resolver_tests::falls_through_to_store`,
/// `resolver_tests::absent_everywhere_is_none`.
pub fn resolve_key_with(provider: &str, store: &dyn KeyStore) -> Option<String> {
    if let Some(value) = env_tier(provider) {
        return Some(value);
    }
    store.get(provider)
}

/// Non-empty `std::env::var` lookup for `provider`'s canonical env var.
///
/// Why: an env var set to the empty string is almost always an accidental
/// `FOO=` in a shell profile, not an intentional "use this empty key" — the
/// resolver treats it as absent so the store tier still gets a chance.
fn env_tier(provider: &str) -> Option<String> {
    let var = env_var_for(provider)?;
    std::env::var(var).ok().filter(|v| !v.is_empty())
}

/// Select the secure-store backend: OS keychain when available and
/// compiled in, else the `0600` file store, else (only when even the home
/// directory is unresolvable) an in-memory store.
///
/// Why: the resolver's store tier must always hand back *some* working
/// `KeyStore` rather than erroring — a missing home directory or an
/// unavailable keychain degrades the backend, it doesn't break credential
/// resolution (env-var-only usage still works).
/// What: behind the `keyring-store` feature, probes `KeyringStore` first
/// (see its docs for the probe/cache semantics) and returns it when
/// available; otherwise constructs [`FileKeyStore::new`], falling back to
/// [`MemoryKeyStore`] only in the (CI/container) case where
/// `dirs::home_dir()` itself fails.
/// Test: not independently unit tested (it makes real OS/filesystem calls
/// by design) — the selection logic is exercised structurally by
/// `resolve_key`'s production path, and each backend is tested in its own
/// module.
pub fn default_store() -> Box<dyn KeyStore> {
    #[cfg(feature = "keyring-store")]
    {
        let keyring = KeyringStore::new();
        if keyring.probe_available() {
            return Box::new(keyring);
        }
    }
    match FileKeyStore::new() {
        Ok(store) => Box::new(store),
        Err(_) => Box::new(MemoryKeyStore::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    /// RAII guard over one process-wide credential environment variable.
    ///
    /// Why: these tests must drive `std::env` to exercise the env tier, but
    /// the same process environment is where [`dotenv::load_env_local_once`]
    /// deposits the developer's REAL credentials. A test that ended with a
    /// bare `remove_var` therefore destroyed a real value for every test that
    /// ran after it in the same binary, and a test that assumed a variable was
    /// absent was reading machine state rather than a fixture it built. This
    /// guard makes each test set what it needs and put back exactly what it
    /// found, so no test depends on — or damages — ambient state (#3451).
    /// What: captures the prior value on construction and restores it on drop,
    /// removing the variable again when there was no prior value.
    /// Test: used by every test in this module.
    struct EnvVarGuard {
        key: &'static str,
        previous: Option<String>,
    }

    impl EnvVarGuard {
        /// Set `key` to `value` for the guard's lifetime.
        fn set(key: &'static str, value: &str) -> Self {
            let previous = std::env::var(key).ok();
            // SAFETY: every caller is `#[serial(dotenv_credential_env)]` — the
            // group every test in this crate that reads or writes a credential
            // env var joins — so no other test thread mutates the environment
            // concurrently with this call.
            unsafe { std::env::set_var(key, value) };
            Self { key, previous }
        }

        /// Remove `key` for the guard's lifetime.
        fn remove(key: &'static str) -> Self {
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

    /// Render a resolved credential for a failure message without disclosing it.
    ///
    /// Why: the env tier reads REAL credential variables, so a failing
    /// `assert_eq!` on the raw `Option<String>` prints whatever the process
    /// actually holds. That is how this module printed a live OpenRouter key
    /// into test output. Every ACTUAL value goes through
    /// [`crate::credentials::redact_secret`], which is non-reversible by
    /// contract; only the EXPECTED side is a test literal and safe verbatim.
    fn describe(value: Option<&str>) -> String {
        match value {
            None => "None".to_string(),
            Some(v) => format!("Some({})", crate::credentials::redact_secret(v)),
        }
    }

    /// Assert a resolution result, redacting the actual value on failure.
    ///
    /// Why: no assertion in this module may print a credential, pass or fail —
    /// see [`describe`].
    fn assert_resolved(actual: Option<String>, expected: Option<&str>) {
        assert!(
            actual.as_deref() == expected,
            "expected {expected:?}, got {} (actual value redacted: this \
             assertion can observe a real credential from the process \
             environment)",
            describe(actual.as_deref())
        );
    }

    /// Why: the leak this module shipped lived in the FAILURE path — a passing
    /// run printed nothing, so no happy-path test could catch its return. The
    /// env tier reads real credential variables, so a mismatch here can hold a
    /// live key; assert directly that the panic message does not carry it.
    /// What: provokes a failed [`assert_resolved`] with a secret-shaped actual
    /// value and checks the message keeps only the non-reversible preview.
    /// Test: itself.
    #[test]
    fn a_failed_resolution_assert_never_prints_the_secret() {
        let secret = "sk-or-v1-0123456789abcdef0123456789abcdef";
        let panic = std::panic::catch_unwind(|| {
            assert_resolved(Some(secret.to_string()), Some("from-dotenv"));
        })
        .expect_err("a mismatched resolution must panic");
        let msg = panic
            .downcast_ref::<String>()
            .expect("assert! panics with a String payload");

        assert!(
            !msg.contains(secret),
            "the failure message echoed the secret verbatim"
        );
        assert!(
            !msg.contains("0123456789"),
            "the failure message echoed secret material past the redaction head"
        );
        assert!(
            msg.contains(&crate::credentials::redact_secret(secret)),
            "the failure message must still identify WHICH credential it saw"
        );
    }

    /// Why: tier 1 (process env) must win over tier 3 (store) — the core
    /// precedence contract.
    /// Test: itself.
    #[test]
    #[serial(dotenv_credential_env)]
    fn env_beats_store() {
        let _guard = EnvVarGuard::set("FIREWORKS_API_KEY", "from-env");
        let store = MemoryKeyStore::new();
        store.set("fireworks", "from-store").unwrap();
        assert_resolved(resolve_key_with("fireworks", &store), Some("from-env"));
    }

    /// Why: tier 2 (`.env.local`, once loaded into the process env) must
    /// still win over tier 3 (store) even though `resolve_key_with` only
    /// ever inspects `std::env::var` directly — this proves the "no
    /// separate .env.local code path" design actually implements the
    /// documented precedence.
    /// Test: itself.
    #[test]
    #[serial(dotenv_credential_env)]
    fn dotenv_loaded_value_beats_store() {
        // The guard clears the variable so `load_env_from_path` below is what
        // populates it (`dotenvy` never overrides an already-set var), and
        // restores whatever the machine had on drop. Nothing here reads an
        // ambient value: the fixture is a `.env.local` this test writes into a
        // temp dir, so the outcome is identical on a machine with no such file
        // and on one whose file says something else.
        let _guard = EnvVarGuard::remove("OPENROUTER_API_KEY");
        let tmp = tempfile::TempDir::new().unwrap();
        let env_path = tmp.path().join(".env.local");
        std::fs::write(&env_path, "OPENROUTER_API_KEY=from-dotenv\n").unwrap();
        assert!(dotenv::load_env_from_path(&env_path));

        let store = MemoryKeyStore::new();
        store.set("openrouter", "from-store").unwrap();
        assert_resolved(resolve_key_with("openrouter", &store), Some("from-dotenv"));
    }

    /// Why: with no env var and no `.env.local` value, the store tier must
    /// still answer — the fallthrough contract.
    /// Test: itself.
    #[test]
    #[serial(dotenv_credential_env)]
    fn falls_through_to_store() {
        let _guard = EnvVarGuard::remove("ANTHROPIC_API_KEY");
        let store = MemoryKeyStore::new();
        store.set("anthropic", "from-store").unwrap();
        assert_resolved(resolve_key_with("anthropic", &store), Some("from-store"));
    }

    /// Why: when every tier is absent, resolution must return `None`, not
    /// panic or synthesise a value.
    /// Test: itself.
    #[test]
    #[serial(dotenv_credential_env)]
    fn absent_everywhere_is_none() {
        let _guard = EnvVarGuard::remove("OPENAI_API_KEY");
        let store = MemoryKeyStore::new();
        assert_resolved(resolve_key_with("openai", &store), None);
    }

    /// Why: an env var explicitly set to the empty string must not shadow
    /// the store tier — see [`env_tier`] docs.
    /// Test: itself.
    #[test]
    #[serial(dotenv_credential_env)]
    fn empty_env_var_falls_through_to_store() {
        let _guard = EnvVarGuard::set("FIREWORKS_API_KEY", "");
        let store = MemoryKeyStore::new();
        store.set("fireworks", "from-store").unwrap();
        assert_resolved(resolve_key_with("fireworks", &store), Some("from-store"));
    }
}
