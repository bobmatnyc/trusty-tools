//! [`BlobKeychain`]: the Keychain backend's value and control items over
//! the vault blob (#10524).
//!
//! Why: every Keychain item the backend touches, values and agent-control
//! items alike, must move into the one blob item, or a control-item read
//! would still prompt per key under a grant.
//! What: maps a value to item (vault, key) and a control item to
//! ([`control_service`], [`agents_account`]), and forwards each call to
//! [`BlobStore`], which picks the blob or the per-item layout.
//! [`SecretBackend::blob_store`] hands the store to the server for the
//! unlock snapshot and the migration.
//! Test: `blob_keychain_values_and_controls_share_the_blob`.

use std::sync::Arc;

use super::keychain::{AGENTS_MARKER, agents_account, control_service};
use super::vault_blob::{BlobStore, ItemId};
use super::{Capabilities, ControlItem, SecretBackend};
use crate::api::{BackendId, SecretKey, SecretValue, SecretsError, VaultName};

/// The value item of `key` in `vault`.
pub(crate) fn value_item(vault: &VaultName, key: &SecretKey) -> ItemId {
    ItemId::new(vault.as_str(), key.as_str())
}

/// The control `item` of `key` (or of the vault, for `None`) in `vault`.
pub(crate) fn control_item_id(
    item: ControlItem,
    vault: &VaultName,
    key: Option<&SecretKey>,
) -> ItemId {
    ItemId::new(control_service(item), agents_account(vault, key))
}

/// `err` naming the item the caller asked for: `vault` and `key`, or
/// `(none)` for a per-vault control item; other errors pass unchanged.
///
/// Why: #10524 merge condition — before `tm secrets migrate`, a failed call
/// reads as it did on origin/main, never naming the blob item the store
/// probed or a control item's service; the reason text is kept.
/// Test: `no_blob_failures_name_the_requested_item`.
fn as_requested(vault: &VaultName, key: Option<&SecretKey>, err: SecretsError) -> SecretsError {
    match err {
        SecretsError::Backend {
            backend, reason, ..
        } => SecretsError::Backend {
            backend,
            vault: vault.to_string(),
            key: key.map_or_else(|| "(none)".to_string(), ToString::to_string),
            reason,
        },
        other => other,
    }
}

/// The Keychain backend's calls over a [`BlobStore`].
#[derive(Debug, Clone)]
pub(crate) struct BlobKeychain {
    store: Arc<BlobStore>,
}

impl BlobKeychain {
    /// The backend over `store`.
    pub(crate) fn new(store: Arc<BlobStore>) -> Self {
        Self { store }
    }
}

impl SecretBackend for BlobKeychain {
    fn id(&self) -> BackendId {
        BackendId::keychain()
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::READ | Capabilities::WRITE
    }

    fn get(&self, vault: &VaultName, key: &SecretKey) -> Result<Option<SecretValue>, SecretsError> {
        Ok(self
            .store
            .get(&value_item(vault, key))
            .map_err(|e| as_requested(vault, Some(key), e))?
            .map(|v| SecretValue::new(v.as_str())))
    }

    fn set(
        &self,
        vault: &VaultName,
        key: &SecretKey,
        value: &SecretValue,
    ) -> Result<(), SecretsError> {
        self.store
            .put(&value_item(vault, key), Some(value.expose()))
            .map(drop)
            .map_err(|e| as_requested(vault, Some(key), e))
    }

    fn delete(&self, vault: &VaultName, key: &SecretKey) -> Result<bool, SecretsError> {
        self.store
            .put(&value_item(vault, key), None)
            .map_err(|e| as_requested(vault, Some(key), e))
    }

    // #9070, #10386: control items are Keychain items, never index fields.
    fn holds_agent_controls(&self) -> bool {
        true
    }

    fn control_item(
        &self,
        item: ControlItem,
        vault: &VaultName,
        key: Option<&SecretKey>,
    ) -> Result<bool, SecretsError> {
        Ok(self
            .store
            .get(&control_item_id(item, vault, key))
            .map_err(|e| as_requested(vault, key, e))?
            .is_some())
    }

    fn set_control_item(
        &self,
        item: ControlItem,
        vault: &VaultName,
        key: Option<&SecretKey>,
        present: bool,
    ) -> Result<(), SecretsError> {
        let id = control_item_id(item, vault, key);
        self.store
            .put(&id, present.then_some(AGENTS_MARKER))
            .map(drop)
            .map_err(|e| as_requested(vault, key, e))
    }

    fn blob_store(&self) -> Option<Arc<BlobStore>> {
        Some(Arc::clone(&self.store))
    }
}

#[cfg(test)]
#[path = "blob_keychain_tests.rs"]
mod tests;
