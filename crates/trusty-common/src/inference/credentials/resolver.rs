//! Composite credential resolver (issue #2401).
//!
//! Why: every inference-adapter consumer needs the same 3-tier answer to
//! "what is provider X's API key" — process env var, then `.env.local`,
//! then the secure store — checked in that exact order every time.
//! What: [`resolve_key`] is the production entry point (loads `.env.local`
//! once, then delegates to [`resolve_key_with`] against
//! [`default_store`]). [`env_var_for`] is the provider→env-var-name mapping.
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

/// Canonical process-env variable name for a provider's API key.
///
/// Why: every call site (the resolver, and eventually the `config` clap
/// module's `--env` hint) must agree on one name per provider rather than
/// re-deriving `{PROVIDER}_API_KEY` ad hoc (which breaks for providers whose
/// canonical env var doesn't follow that shape).
/// What: case-insensitive match on `provider`; extend this list as new
/// providers are added in later Wave 1/2 tickets. `None` for an unknown
/// provider — callers treat that as "env tier does not apply", not an
/// error.
/// Test: `resolver_tests::env_var_for_known_providers`,
/// `resolver_tests::env_var_for_unknown_provider_is_none`.
pub fn env_var_for(provider: &str) -> Option<&'static str> {
    match provider.to_ascii_lowercase().as_str() {
        "fireworks" => Some("FIREWORKS_API_KEY"),
        "openrouter" => Some("OPENROUTER_API_KEY"),
        "anthropic" => Some("ANTHROPIC_API_KEY"),
        "openai" => Some("OPENAI_API_KEY"),
        _ => None,
    }
}

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
/// What: behind the `keyring-store` feature, probes [`KeyringStore`] first
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

    /// Why: pins the canonical mapping used everywhere else in the epic.
    /// Test: itself.
    #[test]
    fn env_var_for_known_providers() {
        assert_eq!(env_var_for("fireworks"), Some("FIREWORKS_API_KEY"));
        assert_eq!(env_var_for("OpenRouter"), Some("OPENROUTER_API_KEY"));
        assert_eq!(env_var_for("anthropic"), Some("ANTHROPIC_API_KEY"));
        assert_eq!(env_var_for("OPENAI"), Some("OPENAI_API_KEY"));
    }

    /// Why: an unmapped provider must not panic or synthesise a guess.
    /// Test: itself.
    #[test]
    fn env_var_for_unknown_provider_is_none() {
        assert_eq!(env_var_for("some-future-provider"), None);
    }

    /// Why: tier 1 (process env) must win over tier 3 (store) — the core
    /// precedence contract.
    /// Test: itself.
    #[test]
    #[serial(dotenv_credential_env)]
    fn env_beats_store() {
        // SAFETY: `#[serial(dotenv_credential_env)]` guarantees no other
        // test in this crate mutates process env concurrently with this one.
        unsafe {
            std::env::set_var("FIREWORKS_API_KEY", "from-env");
        }
        let store = MemoryKeyStore::new();
        store.set("fireworks", "from-store").unwrap();
        assert_eq!(
            resolve_key_with("fireworks", &store),
            Some("from-env".to_string())
        );
        unsafe {
            std::env::remove_var("FIREWORKS_API_KEY");
        }
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
        // SAFETY: see `env_beats_store`.
        unsafe {
            std::env::remove_var("OPENROUTER_API_KEY");
        }
        let tmp = tempfile::TempDir::new().unwrap();
        let env_path = tmp.path().join(".env.local");
        std::fs::write(&env_path, "OPENROUTER_API_KEY=from-dotenv\n").unwrap();
        assert!(dotenv::load_env_from_path(&env_path));

        let store = MemoryKeyStore::new();
        store.set("openrouter", "from-store").unwrap();
        assert_eq!(
            resolve_key_with("openrouter", &store),
            Some("from-dotenv".to_string())
        );
        unsafe {
            std::env::remove_var("OPENROUTER_API_KEY");
        }
    }

    /// Why: with no env var and no `.env.local` value, the store tier must
    /// still answer — the fallthrough contract.
    /// Test: itself.
    #[test]
    #[serial(dotenv_credential_env)]
    fn falls_through_to_store() {
        // SAFETY: see `env_beats_store`.
        unsafe {
            std::env::remove_var("ANTHROPIC_API_KEY");
        }
        let store = MemoryKeyStore::new();
        store.set("anthropic", "from-store").unwrap();
        assert_eq!(
            resolve_key_with("anthropic", &store),
            Some("from-store".to_string())
        );
    }

    /// Why: when every tier is absent, resolution must return `None`, not
    /// panic or synthesise a value.
    /// Test: itself.
    #[test]
    #[serial(dotenv_credential_env)]
    fn absent_everywhere_is_none() {
        // SAFETY: see `env_beats_store`.
        unsafe {
            std::env::remove_var("OPENAI_API_KEY");
        }
        let store = MemoryKeyStore::new();
        assert_eq!(resolve_key_with("openai", &store), None);
    }

    /// Why: an env var explicitly set to the empty string must not shadow
    /// the store tier — see [`env_tier`] docs.
    /// Test: itself.
    #[test]
    #[serial(dotenv_credential_env)]
    fn empty_env_var_falls_through_to_store() {
        // SAFETY: see `env_beats_store`.
        unsafe {
            std::env::set_var("FIREWORKS_API_KEY", "");
        }
        let store = MemoryKeyStore::new();
        store.set("fireworks", "from-store").unwrap();
        assert_eq!(
            resolve_key_with("fireworks", &store),
            Some("from-store".to_string())
        );
        unsafe {
            std::env::remove_var("FIREWORKS_API_KEY");
        }
    }
}
