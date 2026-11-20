//! The vault blob: every trusty Keychain item kept in one Keychain item
//! (#10524, owner ruling 2026-11-19 "one Keychain item, one prompt total").
//!
//! Why: macOS evaluates an item's access control per item, so N secrets
//! meant N prompts after each `cargo install` re-signed the binary. One item
//! means one read, and one prompt, at `tm secrets unlock`.
//! What: [`ItemId`] names one (service, account) item; [`ItemStore`] is the
//! seam over the OS Keychain (tests give an in-memory fake). [`Blob`] is
//! the decoded map, serialized as versioned JSON; a blob that does not parse
//! is an error with fixed text, never the parser's message (which can quote
//! value bytes). [`BlobStore`] serves item reads and writes: while the blob
//! item exists every item lives in it (migrated layout); while it is absent
//! each item is its own Keychain item (the per-item layout, unchanged). A
//! corrupt blob fails every call; it never falls back to per-item items,
//! which may be stale. Writes are read-modify-write under one lock and land
//! as one item write, so a write is never partial and two writes in this
//! process never lose each other. Values are zeroed on drop.
//! Test: `vault_blob_tests.rs` beside this file.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::api::{BackendId, SecretsError};

/// The Keychain service of the vault blob item.
pub(crate) const BLOB_SERVICE: &str = "trusty-secrets.vault-blob";
/// The Keychain service of the migration backup item.
pub(crate) const BACKUP_SERVICE: &str = "trusty-secrets.vault-blob-backup";
/// The account of the blob and backup items.
const BLOB_ACCOUNT: &str = "v1";
/// The `format` field every blob carries.
const FORMAT: &str = "trusty-secrets.vault-blob";
/// The only blob version this build reads or writes.
const VERSION: u32 = 1;

/// One Keychain item: its service and account.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ItemId {
    /// The Keychain service; a vault name for a value item.
    pub service: String,
    /// The Keychain account; a key name for a value item.
    pub account: String,
}

impl ItemId {
    /// The item `service`/`account`.
    pub(crate) fn new(service: impl Into<String>, account: impl Into<String>) -> Self {
        Self {
            service: service.into(),
            account: account.into(),
        }
    }

    /// The vault blob item.
    pub(crate) fn blob() -> Self {
        Self::new(BLOB_SERVICE, BLOB_ACCOUNT)
    }

    /// The migration backup item.
    pub(crate) fn backup() -> Self {
        Self::new(BACKUP_SERVICE, BLOB_ACCOUNT)
    }
}

/// The OS item store, or a test fake: read, write and delete one item.
///
/// What: a missing item is `Ok(None)` from `read` and `Ok(false)` from
/// `delete`; any other failure is an `Err` whose text carries no value.
pub trait ItemStore: Send + Sync + fmt::Debug {
    /// The item's text, or `None` when it does not exist.
    fn read(&self, id: &ItemId) -> Result<Option<Zeroizing<String>>, SecretsError>;
    /// Create or replace the item with `text`.
    fn write(&self, id: &ItemId, text: &str) -> Result<(), SecretsError>;
    /// Remove the item; whether it existed.
    fn delete(&self, id: &ItemId) -> Result<bool, SecretsError>;
}

/// A [`SecretsError::Backend`] about item `id`, with fixed `reason` text.
pub(crate) fn item_error(id: &ItemId, reason: impl Into<String>) -> SecretsError {
    SecretsError::Backend {
        backend: BackendId::KEYCHAIN.to_string(),
        vault: id.service.clone(),
        key: id.account.clone(),
        reason: reason.into(),
    }
}

/// The decoded blob: item id to value. `Debug` shows the count only.
#[derive(Default)]
pub struct Blob(BTreeMap<ItemId, Zeroizing<String>>);

impl fmt::Debug for Blob {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Blob")
            .field("items", &self.0.len())
            .finish()
    }
}

/// The serialized form, borrowed for writing.
#[derive(Serialize)]
struct WireOut<'a> {
    format: &'a str,
    version: u32,
    items: Vec<(&'a str, &'a str, &'a str)>,
}

/// The serialized form, owned for reading.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireIn {
    format: String,
    version: u32,
    items: Vec<(String, String, String)>,
}

impl Blob {
    /// How many items it holds.
    pub(crate) fn len(&self) -> usize {
        self.0.len()
    }

    /// The value of `id`, if held.
    pub(crate) fn get(&self, id: &ItemId) -> Option<&str> {
        self.0.get(id).map(|v| v.as_str())
    }

    /// Hold `value` for `id`, replacing any value.
    pub(crate) fn insert(&mut self, id: ItemId, value: &str) {
        self.0.insert(id, Zeroizing::new(value.to_owned()));
    }

    /// Drop `id`; whether it was held.
    pub(crate) fn remove(&mut self, id: &ItemId) -> bool {
        self.0.remove(id).is_some()
    }

    /// Every held item.
    pub(crate) fn items(&self) -> impl Iterator<Item = (&ItemId, &str)> {
        self.0.iter().map(|(id, v)| (id, v.as_str()))
    }

    /// Whether `other` holds the same ids with the same values. Compares in
    /// memory only; nothing is printed.
    pub(crate) fn same_as(&self, other: &Self) -> bool {
        self.0.len() == other.0.len()
            && self
                .0
                .iter()
                .zip(other.0.iter())
                .all(|((a, av), (b, bv))| a == b && av.as_str() == bv.as_str())
    }

    /// The blob as versioned JSON.
    pub(crate) fn encode(&self) -> Result<Zeroizing<String>, SecretsError> {
        let wire = WireOut {
            format: FORMAT,
            version: VERSION,
            items: self
                .0
                .iter()
                .map(|(id, v)| (id.service.as_str(), id.account.as_str(), v.as_str()))
                .collect(),
        };
        serde_json::to_string(&wire)
            .map(Zeroizing::new)
            .map_err(|_| item_error(&ItemId::blob(), "the vault blob cannot be encoded"))
    }

    /// Parse `text` read from item `from`.
    ///
    /// What: any parse failure, a foreign format, an unknown version or a
    /// repeated item is one fixed error naming `from`; the parser's own
    /// message is dropped, because it can quote value bytes.
    pub(crate) fn decode(from: &ItemId, text: &str) -> Result<Self, SecretsError> {
        let corrupt = || item_error(from, "the vault blob is corrupt");
        // #10524: never format the serde error; it may quote a value.
        let wire: WireIn = serde_json::from_str(text).map_err(|_| corrupt())?;
        if wire.format != FORMAT || wire.version != VERSION {
            return Err(corrupt());
        }
        let mut blob = Self::default();
        for (service, account, value) in wire.items {
            let value = Zeroizing::new(value);
            if blob
                .0
                .insert(ItemId::new(service, account), value)
                .is_some()
            {
                return Err(corrupt());
            }
        }
        Ok(blob)
    }
}

/// The item store with the blob layered over it.
///
/// Why: see the module docs.
/// What: one per process for the OS Keychain, so every caller shares the
/// write lock. `Debug` names no item.
/// Test: `blob_store_routes_by_layout`, `blob_store_corrupt_blob_fails_every_call`,
/// `blob_store_concurrent_writes_keep_both`.
pub struct BlobStore {
    items: Arc<dyn ItemStore>,
    writes: Mutex<()>,
}

impl fmt::Debug for BlobStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BlobStore").finish_non_exhaustive()
    }
}

impl BlobStore {
    /// The blob layered over `items`.
    pub(crate) fn new(items: Arc<dyn ItemStore>) -> Self {
        Self {
            items,
            writes: Mutex::new(()),
        }
    }

    /// The raw item store, for migration.
    pub(crate) fn items(&self) -> &dyn ItemStore {
        self.items.as_ref()
    }

    /// The write lock. Guards no memory, only the order of Keychain writes,
    /// so a poisoned lock is taken as is.
    pub(crate) fn write_lock(&self) -> MutexGuard<'_, ()> {
        self.writes.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The decoded item `id` (the blob or the backup), or `None` when absent.
    pub(crate) fn read_blob_at(&self, id: &ItemId) -> Result<Option<Blob>, SecretsError> {
        match self.items.read(id)? {
            Some(text) => Blob::decode(id, &text).map(Some),
            None => Ok(None),
        }
    }

    /// The decoded vault blob, or `None` in the per-item layout.
    pub(crate) fn read_blob(&self) -> Result<Option<Blob>, SecretsError> {
        self.read_blob_at(&ItemId::blob())
    }

    /// Item `id`'s value under the current layout.
    pub(crate) fn get(&self, id: &ItemId) -> Result<Option<Zeroizing<String>>, SecretsError> {
        match self.read_blob()? {
            Some(blob) => Ok(blob.get(id).map(|v| Zeroizing::new(v.to_owned()))),
            None => self.items.read(id),
        }
    }

    /// Set (`Some`) or remove (`None`) item `id` under the current layout;
    /// whether it existed before.
    ///
    /// What: under the write lock; the migrated layout rereads the blob,
    /// changes one item and writes the whole blob back in one item write.
    pub(crate) fn put(&self, id: &ItemId, value: Option<&str>) -> Result<bool, SecretsError> {
        let _writes = self.write_lock();
        let Some(mut blob) = self.read_blob()? else {
            return match value {
                Some(value) => self.items.write(id, value).map(|()| true),
                None => self.items.delete(id),
            };
        };
        let existed = match value {
            Some(value) => {
                let existed = blob.get(id).is_some();
                blob.insert(id.clone(), value);
                existed
            }
            None if blob.remove(id) => true,
            // Nothing held: nothing to write.
            None => return Ok(false),
        };
        self.items.write(&ItemId::blob(), &blob.encode()?)?;
        Ok(existed)
    }
}

#[cfg(test)]
#[path = "vault_blob_tests.rs"]
mod tests;
