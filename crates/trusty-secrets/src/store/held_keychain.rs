//! [`HeldKeychain`]: the Keychain backend as the server opens it, answering
//! reads from the unlock grant's held blob (#10524).
//!
//! Why: AC1 (owner ruling 2026-11-19) — after one prompt at unlock, no read
//! of any Keychain item may prompt for 8 h. Values and agent-control items
//! are both read on the request path, so both are answered here.
//! What: wraps the opened backend. With a held blob, `get` and
//! `control_item` answer from it (an item it lacks is absent) and never call
//! the Keychain. Writes go to the Keychain first; the held copy changes only
//! after the write landed, and the item reads from the Keychain while its
//! write is in flight or after it failed. Without a held blob every call
//! passes straight through.
//! Test: `migrate_then_unlock_reads_the_keychain_once`,
//! `grant_held_write_in_flight_or_failed_reads_the_keychain`.

use std::sync::Arc;

use super::blob_keychain::{control_item_id, value_item};
use super::grant_cache::HeldRead;
use super::keychain::AGENTS_MARKER;
use super::vault_blob::{BlobStore, ItemId};
use super::{Capabilities, ControlItem, SecretBackend, SecretCache};
use crate::api::{BackendId, SecretKey, SecretValue, SecretsError, VaultName};

/// The Keychain backend behind the grant's held blob.
#[derive(Debug)]
pub(crate) struct HeldKeychain {
    inner: Arc<dyn SecretBackend>,
    cache: Arc<SecretCache>,
}

impl HeldKeychain {
    /// `inner`, answered from `cache`'s grant while it holds a blob.
    pub(crate) fn new(inner: Arc<dyn SecretBackend>, cache: Arc<SecretCache>) -> Self {
        Self { inner, cache }
    }

    /// Run `write` on `id`, keeping the held copy in step with `landed`.
    fn write<T>(
        &self,
        id: &ItemId,
        landed: Option<&str>,
        write: impl FnOnce() -> Result<T, SecretsError>,
    ) -> Result<T, SecretsError> {
        let ticket = self.cache.grant().held_begin(id);
        let result = write()?;
        if let Some(ticket) = ticket {
            self.cache.grant().held_end(id, ticket, landed);
        }
        Ok(result)
    }
}

impl SecretBackend for HeldKeychain {
    fn id(&self) -> BackendId {
        self.inner.id()
    }

    fn capabilities(&self) -> Capabilities {
        self.inner.capabilities()
    }

    fn get(&self, vault: &VaultName, key: &SecretKey) -> Result<Option<SecretValue>, SecretsError> {
        match self.cache.grant().held_get(&value_item(vault, key)) {
            HeldRead::Hit(value) => Ok(Some(value)),
            HeldRead::Absent => Ok(None),
            HeldRead::Off => self.inner.get(vault, key),
        }
    }

    fn set(
        &self,
        vault: &VaultName,
        key: &SecretKey,
        value: &SecretValue,
    ) -> Result<(), SecretsError> {
        self.write(&value_item(vault, key), Some(value.expose()), || {
            self.inner.set(vault, key, value)
        })
    }

    fn delete(&self, vault: &VaultName, key: &SecretKey) -> Result<bool, SecretsError> {
        self.write(&value_item(vault, key), None, || {
            self.inner.delete(vault, key)
        })
    }

    fn list_names(&self, vault: &VaultName) -> Result<Vec<SecretKey>, SecretsError> {
        self.inner.list_names(vault)
    }

    fn holds_agent_controls(&self) -> bool {
        self.inner.holds_agent_controls()
    }

    fn control_item(
        &self,
        item: ControlItem,
        vault: &VaultName,
        key: Option<&SecretKey>,
    ) -> Result<bool, SecretsError> {
        match self
            .cache
            .grant()
            .held_get(&control_item_id(item, vault, key))
        {
            HeldRead::Hit(_) => Ok(true),
            HeldRead::Absent => Ok(false),
            HeldRead::Off => self.inner.control_item(item, vault, key),
        }
    }

    fn set_control_item(
        &self,
        item: ControlItem,
        vault: &VaultName,
        key: Option<&SecretKey>,
        present: bool,
    ) -> Result<(), SecretsError> {
        let id = control_item_id(item, vault, key);
        self.write(&id, present.then_some(AGENTS_MARKER), || {
            self.inner.set_control_item(item, vault, key, present)
        })
    }

    fn blob_store(&self) -> Option<Arc<BlobStore>> {
        self.inner.blob_store()
    }
}
