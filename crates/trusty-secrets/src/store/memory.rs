//! [`MemoryBackend`]: an in-process test double for [`super::SecretBackend`].
//!
//! Why: logic tests — here and in S2's consumers — must never touch the OS
//! keychain. Compiled only for this crate's tests and under the
//! `test-support` feature, so no production build can select it.
//! What: a mutex-guarded map keyed by (vault, key), with configurable
//! capabilities. `Debug` shows the entry count only. [`MemoryBackend::reads`]
//! counts `get` calls, so a test can prove a gate refused before any read.
//! Test: `store_debug_never_contains_a_value`.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard};

use super::{Capabilities, SecretBackend};
use crate::api::{BackendId, SecretKey, SecretValue, SecretsError, VaultName};

type Entries = BTreeMap<(VaultName, SecretKey), SecretValue>;

/// An in-memory backend for tests.
pub struct MemoryBackend {
    id: BackendId,
    capabilities: Capabilities,
    entries: Mutex<Entries>,
    reads: AtomicUsize,
}

impl MemoryBackend {
    /// A `READ | WRITE` backend with id `memory`.
    pub fn new() -> Self {
        Self::with_capabilities(Capabilities::READ | Capabilities::WRITE)
    }

    /// A backend declaring exactly `capabilities`.
    pub fn with_capabilities(capabilities: Capabilities) -> Self {
        Self {
            id: BackendId::from_static("memory"),
            capabilities,
            entries: Mutex::new(BTreeMap::new()),
            reads: AtomicUsize::new(0),
        }
    }

    fn entries(
        &self,
        vault: &VaultName,
        key: &SecretKey,
    ) -> Result<MutexGuard<'_, Entries>, SecretsError> {
        self.entries.lock().map_err(|_| SecretsError::Backend {
            backend: self.id.to_string(),
            vault: vault.to_string(),
            key: key.to_string(),
            reason: "memory backend lock poisoned".to_string(),
        })
    }

    /// Number of stored entries across all vaults.
    pub fn len(&self) -> usize {
        self.entries.lock().map_or(0, |e| e.len())
    }

    /// Whether no entry is stored.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// How many times `get` has been called, hit or miss.
    ///
    /// Test: `resolve_agent_gate_refuses_flag_off_before_any_read`.
    pub fn reads(&self) -> usize {
        // #7525: the counter that proves a refused key never reached a read.
        self.reads.load(Ordering::SeqCst)
    }
}

impl Default for MemoryBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for MemoryBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MemoryBackend")
            .field("capabilities", &self.capabilities)
            .field("entries", &self.len())
            .finish()
    }
}

impl SecretBackend for MemoryBackend {
    fn id(&self) -> BackendId {
        self.id.clone()
    }

    fn capabilities(&self) -> Capabilities {
        self.capabilities
    }

    fn get(&self, vault: &VaultName, key: &SecretKey) -> Result<Option<SecretValue>, SecretsError> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        Ok(self
            .entries(vault, key)?
            .get(&(vault.clone(), key.clone()))
            .cloned())
    }

    fn set(
        &self,
        vault: &VaultName,
        key: &SecretKey,
        value: &SecretValue,
    ) -> Result<(), SecretsError> {
        self.entries(vault, key)?
            .insert((vault.clone(), key.clone()), value.clone());
        Ok(())
    }

    fn delete(&self, vault: &VaultName, key: &SecretKey) -> Result<bool, SecretsError> {
        Ok(self
            .entries(vault, key)?
            .remove(&(vault.clone(), key.clone()))
            .is_some())
    }
}
