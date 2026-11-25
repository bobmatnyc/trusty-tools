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
//! blob may carry a SHA-256 `digest` of its items, checked on read when
//! present; this build writes none yet, so older readers still decode what
//! it writes (#11738). A corrupt or tampered
//! blob fails every read and write and is never rewritten; a read never
//! falls back to the backup or per-item items, which can hold a revoked
//! value (#11786). Writes are read-modify-write under one lock and land
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
    // #11738: absent on a blob written before the digest existed.
    #[serde(default)]
    digest: Option<String>,
}

/// The lowercase hex SHA-256 of `items` as JSON (#11738). The JSON holds
/// values, so it is zeroed on drop.
fn digest_of<T: AsRef<str>>(items: &[(T, T, T)]) -> Result<String, ()> {
    use sha2::{Digest, Sha256};
    let rows: Vec<(&str, &str, &str)> = items
        .iter()
        .map(|(s, a, v)| (s.as_ref(), a.as_ref(), v.as_ref()))
        .collect();
    let json = Zeroizing::new(serde_json::to_string(&rows).map_err(|_| ())?);
    let hash = Sha256::digest(json.as_bytes());
    Ok(hash.iter().map(|b| format!("{b:02x}")).collect())
}

/// Which source a read used because the vault blob was corrupt (#11738).
///
/// What: a typed warning; it names a source, never a value. Kept so audit
/// logs written by 0.9.x builds with #11738 still decode; this build never
/// records one, since a corrupt blob fails closed (#11786).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum BlobFallback {
    /// The migration backup item.
    Backup,
    /// The per-item Keychain items.
    PerItem,
    /// A fallback a newer build records. Never written by this build.
    #[serde(other)]
    Other,
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
        let failed = || item_error(&ItemId::blob(), "the vault blob cannot be encoded");
        let items: Vec<(&str, &str, &str)> = self
            .0
            .iter()
            .map(|(id, v)| (id.service.as_str(), id.account.as_str(), v.as_str()))
            .collect();
        // #11738: verify-only this release; digest emission deferred until every reader verifies (mixed-version safety)
        let wire = WireOut {
            format: FORMAT,
            version: VERSION,
            items,
        };
        serde_json::to_string(&wire)
            .map(Zeroizing::new)
            .map_err(|_| failed())
    }

    /// Parse `text` read from item `from`.
    ///
    /// What: any parse failure, a foreign format, an unknown version, a
    /// digest that does not match the items, or a repeated item is one
    /// fixed error naming `from`; the parser's own message is dropped,
    /// because it can quote value bytes. A blob with no digest is checked
    /// for the rest only (#11738, backward compatible).
    pub(crate) fn decode(from: &ItemId, text: &str) -> Result<Self, SecretsError> {
        let corrupt = || item_error(from, "the vault blob is corrupt");
        // #10524: never format the serde error; it may quote a value.
        let wire: WireIn = serde_json::from_str(text).map_err(|_| corrupt())?;
        if wire.format != FORMAT || wire.version != VERSION {
            return Err(corrupt());
        }
        // #11738: a blob that parses but whose items were changed is corrupt.
        if let Some(digest) = &wire.digest
            && digest_of(&wire.items).map_err(|_| corrupt())? != *digest
        {
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
/// Test: `blob_store_routes_by_layout`, `blob_store_corrupt_blob_fails_every_write`,
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
    ///
    /// Why: #11786 — a corrupt blob must fail closed. The migration backup
    /// and the per-item items can hold a key rotated or deleted since the
    /// migration, so serving either brings a revoked value back.
    /// What: reads the blob. When it is corrupt or tampered (digest
    /// mismatch), the read is a fixed error naming `id` and the
    /// `tm secrets migrate --rollback` recovery and that it needs a
    /// migration backup, with no value bytes. The
    /// blob item is left untouched.
    /// Test: `blob_store_corrupt_blob_with_backup_fails_closed`,
    /// `blob_store_corrupt_blob_without_backup_fails_closed`,
    /// `blob_store_tampered_blob_fails_closed`.
    pub(crate) fn get(&self, id: &ItemId) -> Result<Option<Zeroizing<String>>, SecretsError> {
        let Some(text) = self.items.read(&ItemId::blob())? else {
            return self.items.read(id);
        };
        // #11786: no backup or per-item fallback; a corrupt blob fails closed.
        let blob = Blob::decode(&ItemId::blob(), &text).map_err(|_| {
            // #11786: rollback needs a migration backup and restores its values.
            item_error(
                id,
                "the vault blob is corrupt; `tm secrets migrate --rollback` recovers it \
                 only when a migration backup exists, and restores the backup's values",
            )
        })?;
        Ok(blob.get(id).map(|v| Zeroizing::new(v.to_owned())))
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
