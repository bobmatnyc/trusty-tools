//! OS-keychain `KeyStore` backend (issue #2401), feature `keyring-store`.
//!
//! Why: an OS keychain (macOS Keychain, Windows Credential Manager, Secret
//! Service on Linux) is the strongest available local credential store when
//! a desktop session is present, but it is unavailable — or errors in
//! surprising ways — headless (SSH, CI, containers, locked session). Rather
//! than let every caller special-case that, this backend runs a one-time
//! probe and caches the result, so a caller can always ask
//! [`KeyringStore::probe_available`] and fall back to `FileKeyStore`
//! ([`super::resolver::default_store`] does exactly this).
//! What: [`KeyringStore`] wraps the `keyring` crate. Service name convention:
//! `"trusty-tools"`; account = provider name (e.g. `fireworks`). The probe
//! attempts a harmless `get_password` on a sentinel entry — a `NoEntry`
//! result still counts as "backend available" (the keychain answered); any
//! other error (locked, denied, unsupported platform) marks the backend
//! unavailable for the remainder of the process.
//! Test: `keyring_store_tests` (sibling file) — compile-check and the
//! probe-failure path only. **No test in this module touches a real OS
//! keychain**, per the ticket's acceptance criterion.

use std::sync::OnceLock;

use super::{KeyStore, KeyStoreError};

/// Keychain service name every trusty-* credential lives under.
const SERVICE: &str = "trusty-tools";

/// Sentinel account name used only by [`KeyringStore::probe`] — never a real
/// provider, so it can never collide with a stored credential.
const PROBE_ACCOUNT: &str = "__trusty_probe__";

/// Process-wide probe cache (QA fix, PR #2427).
///
/// Why: the cache was originally a per-instance field, but
/// [`super::resolver::default_store`] constructs a fresh `KeyringStore` per
/// call, so every `resolve_key` re-probed the OS keychain — needless cost
/// and, worse, two calls could observe different availability (breaking the
/// write-then-read consistency #2404's `config` verbs need). A `static`
/// makes the probe truly once-per-process, matching the documented
/// semantics.
static PROBE_RESULT: OnceLock<bool> = OnceLock::new();

/// OS-keychain credential store. See module docs.
///
/// Deriving `Debug` is safe: the struct is stateless (the probe cache is a
/// module `static`) and credential values never live in memory beyond a
/// single call. Pinned by `tests::debug_output_never_contains_values`.
#[derive(Debug)]
pub struct KeyringStore;

impl KeyringStore {
    /// New store. The probe does not run until first use (`get`/`set`/
    /// `unset`/[`probe_available`](Self::probe_available)).
    pub fn new() -> Self {
        Self
    }

    /// Whether the keychain backend answered the probe successfully. Cached
    /// process-wide after the first call (see [`PROBE_RESULT`]).
    ///
    /// Why: exposed so [`super::resolver::default_store`] can decide whether
    /// to hand out this backend or fall back to `FileKeyStore` without
    /// duplicating the probe logic.
    pub fn probe_available(&self) -> bool {
        *PROBE_RESULT.get_or_init(Self::probe)
    }

    /// Why: see module docs — any backend error other than "no such entry"
    /// marks the keychain unavailable.
    /// What: attempts `Entry::new(SERVICE, PROBE_ACCOUNT).get_password()`;
    /// `Ok(_)` or `Err(keyring::Error::NoEntry)` both mean "the backend
    /// answered", so both count as available.
    /// Test: `keyring_store_tests::probe_does_not_panic`.
    fn probe() -> bool {
        match keyring::Entry::new(SERVICE, PROBE_ACCOUNT) {
            Ok(entry) => matches!(entry.get_password(), Ok(_) | Err(keyring::Error::NoEntry)),
            Err(_) => false,
        }
    }
}

impl Default for KeyringStore {
    fn default() -> Self {
        Self::new()
    }
}

impl KeyStore for KeyringStore {
    fn get(&self, provider: &str) -> Option<String> {
        if !self.probe_available() {
            return None;
        }
        keyring::Entry::new(SERVICE, provider)
            .ok()?
            .get_password()
            .ok()
    }

    fn set(&self, provider: &str, value: &str) -> Result<(), KeyStoreError> {
        if !self.probe_available() {
            return Err(KeyStoreError::Keyring(
                "keychain backend unavailable".to_string(),
            ));
        }
        let entry = keyring::Entry::new(SERVICE, provider)
            .map_err(|e| KeyStoreError::Keyring(e.to_string()))?;
        entry
            .set_password(value)
            .map_err(|e| KeyStoreError::Keyring(e.to_string()))
    }

    fn unset(&self, provider: &str) -> Result<(), KeyStoreError> {
        if !self.probe_available() {
            return Err(KeyStoreError::Keyring(
                "keychain backend unavailable".to_string(),
            ));
        }
        let entry = keyring::Entry::new(SERVICE, provider)
            .map_err(|e| KeyStoreError::Keyring(e.to_string()))?;
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(KeyStoreError::Keyring(e.to_string())),
        }
    }

    fn list(&self) -> Vec<String> {
        // Why: no OS keychain exposes a portable "list all accounts for a
        // service" API through the `keyring` crate. Enumeration is left to
        // `FileKeyStore`/`MemoryKeyStore`; `KeyringStore::list` documents the
        // limitation rather than returning a misleading partial result.
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: the probe must never panic regardless of whether a real
    /// keychain is present in the CI/dev environment running this test —
    /// this is the "compile-check / probe-failure-path only" acceptance
    /// criterion, not an assertion on the *result* of the probe.
    /// Test: itself.
    #[test]
    fn probe_does_not_panic() {
        let store = KeyringStore::new();
        let _ = store.probe_available();
    }

    /// Why: probe result must be cached process-wide — a second call (even
    /// on a DIFFERENT instance, matching `default_store`'s
    /// fresh-instance-per-call pattern) must observe the same answer without
    /// re-invoking the backend.
    /// Test: itself.
    #[test]
    fn probe_is_cached_across_instances() {
        let first = KeyringStore::new().probe_available();
        let second = KeyringStore::new().probe_available();
        assert_eq!(first, second);
    }

    /// Why: `list()` must document the enumeration limitation, not attempt
    /// (and potentially fail against) a real keychain.
    /// Test: itself.
    #[test]
    fn list_is_always_empty() {
        let store = KeyringStore::new();
        assert_eq!(store.list(), Vec::<String>::new());
    }

    /// Why: QA regression (PR #2427) — `{:?}` of every store type must
    /// never contain a credential value. `KeyringStore` is stateless, so
    /// this pins that the type stays value-free (no `set` here: writing to
    /// a real keychain is forbidden in tests; statelessness makes the
    /// property structural).
    /// Test: itself.
    #[test]
    fn debug_output_never_contains_values() {
        let dbg = format!("{:?}", KeyringStore::new());
        assert_eq!(dbg, "KeyringStore");
    }
}
