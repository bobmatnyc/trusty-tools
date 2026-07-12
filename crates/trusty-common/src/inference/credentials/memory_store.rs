//! In-memory `KeyStore` — the test/CI backend.
//!
//! Why: unit tests and CI must never touch the real filesystem or an OS
//! keychain. `MemoryKeyStore` gives every precedence/round-trip test a
//! process-local, infallible backend so the acceptance criterion "zero tests
//! touch a real keychain" is trivially satisfiable.
//! What: a `HashMap<String, String>` behind a `Mutex`. All four `KeyStore`
//! operations are infallible — `set`/`unset` always return `Ok(())`.
//! Test: `memory_store_tests` (sibling file).

use std::collections::HashMap;
use std::sync::Mutex;

use super::{KeyStore, KeyStoreError};

/// In-memory credential store. See module docs.
#[derive(Default)]
pub struct MemoryKeyStore {
    inner: Mutex<HashMap<String, String>>,
}

impl MemoryKeyStore {
    /// New, empty store.
    pub fn new() -> Self {
        Self::default()
    }
}

/// Hand-written redacting `Debug` — provider names only, never values.
///
/// Why: a derived `Debug` would print the raw credential values (QA finding
/// on PR #2427), and this type is public AND the production fallback when
/// `$HOME` is unresolvable, so a stray `{:?}` in a log line would leak every
/// stored key. Hand-writing it keeps the type debuggable (names + count)
/// while making value leakage structurally impossible.
/// What: renders as `MemoryKeyStore { providers: [..names..] }`.
/// Test: `debug_output_never_contains_values`.
impl std::fmt::Debug for MemoryKeyStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let names: Vec<String> = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .keys()
            .cloned()
            .collect();
        f.debug_struct("MemoryKeyStore")
            .field("providers", &names)
            .finish()
    }
}

impl KeyStore for MemoryKeyStore {
    fn get(&self, provider: &str) -> Option<String> {
        // A poisoned lock (a prior panic while held) is not the caller's
        // problem — recover the map rather than propagating the poison, per
        // the "no unwrap in library code" / "fail gracefully" conventions.
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(provider)
            .cloned()
    }

    fn set(&self, provider: &str, value: &str) -> Result<(), KeyStoreError> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(provider.to_string(), value.to_string());
        Ok(())
    }

    fn unset(&self, provider: &str) -> Result<(), KeyStoreError> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(provider);
        Ok(())
    }

    fn list(&self) -> Vec<String> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .keys()
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: the store must round-trip a value through set/get exactly.
    /// Test: itself.
    #[test]
    fn set_then_get_round_trips() {
        let store = MemoryKeyStore::new();
        assert_eq!(store.get("fireworks"), None);
        store.set("fireworks", "fw-secret").unwrap();
        assert_eq!(store.get("fireworks"), Some("fw-secret".to_string()));
    }

    /// Why: `unset` must remove the entry and be a no-op when absent.
    /// Test: itself.
    #[test]
    fn unset_removes_and_is_idempotent() {
        let store = MemoryKeyStore::new();
        store.set("openai", "sk-abc").unwrap();
        store.unset("openai").unwrap();
        assert_eq!(store.get("openai"), None);
        // Unsetting an already-absent key must not error.
        store.unset("openai").unwrap();
    }

    /// Why: `list` must return names only, never the stored values.
    /// Test: itself.
    #[test]
    fn list_returns_names_only() {
        let store = MemoryKeyStore::new();
        store.set("anthropic", "sk-ant-secret").unwrap();
        store.set("openrouter", "or-secret").unwrap();
        let mut names = store.list();
        names.sort();
        assert_eq!(
            names,
            vec!["anthropic".to_string(), "openrouter".to_string()]
        );
        assert!(!names.contains(&"sk-ant-secret".to_string()));
        assert!(!names.contains(&"or-secret".to_string()));
    }

    /// Why: QA regression (PR #2427) — the previous derived `Debug` printed
    /// raw credential values; `{:?}` after a `set` must never contain the
    /// plaintext value (provider names are fine).
    /// Test: itself.
    #[test]
    fn debug_output_never_contains_values() {
        let store = MemoryKeyStore::new();
        store.set("fireworks", "fw-super-secret-value").unwrap();
        let dbg = format!("{store:?}");
        assert!(
            !dbg.contains("fw-super-secret-value"),
            "Debug output leaked a credential value: {dbg}"
        );
        assert!(
            dbg.contains("fireworks"),
            "names should remain visible: {dbg}"
        );
    }
}
