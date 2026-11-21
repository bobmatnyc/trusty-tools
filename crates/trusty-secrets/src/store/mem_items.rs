//! An in-memory [`ItemStore`] with injectable faults, for tests only
//! (#10524).
//!
//! Why: no test may read or write the real login Keychain; the blob, the
//! migration and the unlock snapshot are all proven against this fake.
//! What: a map of item id to text, a read counter (a Keychain read is a
//! prompt), and faults: a failing read, write or delete for one item, a
//! write that stores other text than it was given (a read-back mismatch),
//! a delete budget after which every delete fails, a delay inside each
//! write to widen races, and a log of every call.
//! Test: used by `vault_blob_tests.rs`, `blob_migrate_tests.rs` and the
//! server's unlock tests.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use zeroize::Zeroizing;

use super::vault_blob::{Blob, ItemId, ItemStore, item_error};
use crate::api::SecretsError;

/// The fake store. Every field is public to the tests that arm it.
#[derive(Debug, Default)]
pub(crate) struct MemItems {
    pub(crate) items: Mutex<BTreeMap<ItemId, String>>,
    pub(crate) reads: AtomicUsize,
    pub(crate) fail_read: Mutex<BTreeSet<ItemId>>,
    pub(crate) fail_write: Mutex<BTreeSet<ItemId>>,
    pub(crate) garble_write: Mutex<BTreeSet<ItemId>>,
    /// Deletes allowed before every later one fails; `None` is unlimited.
    pub(crate) delete_budget: Mutex<Option<usize>>,
    pub(crate) write_delay: Mutex<Option<Duration>>,
    /// Every call in order: `("read" | "write" | "delete", item)`.
    pub(crate) calls: Mutex<Vec<(&'static str, ItemId)>>,
}

impl MemItems {
    /// The store holding `items`.
    pub(crate) fn with(items: &[(&ItemId, &str)]) -> Self {
        let store = Self::default();
        for (id, text) in items {
            store.put_raw(id, text);
        }
        store
    }

    /// Store `text` at `id` with no fault and no count.
    pub(crate) fn put_raw(&self, id: &ItemId, text: &str) {
        self.items
            .lock()
            .unwrap()
            .insert(id.clone(), text.to_owned());
    }

    /// The raw text at `id`, uncounted.
    pub(crate) fn raw(&self, id: &ItemId) -> Option<String> {
        self.items.lock().unwrap().get(id).cloned()
    }

    /// Record one call on `id`.
    fn log(&self, op: &'static str, id: &ItemId) {
        self.calls.lock().unwrap().push((op, id.clone()));
    }

    /// How many reads ran.
    pub(crate) fn read_count(&self) -> usize {
        self.reads.load(Ordering::SeqCst)
    }
}

/// `text` changed but still well formed: a blob loses its first item, any
/// other text gains a byte.
fn garbled(id: &ItemId, text: &str) -> String {
    match Blob::decode(id, text) {
        Ok(mut blob) if blob.len() > 0 => {
            let first = blob.items().next().map(|(id, _)| id.clone());
            if let Some(first) = first {
                blob.remove(&first);
            }
            blob.encode().map(|t| t.to_string()).unwrap_or_default()
        }
        _ => format!("{text}x"),
    }
}

impl ItemStore for MemItems {
    fn read(&self, id: &ItemId) -> Result<Option<Zeroizing<String>>, SecretsError> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.log("read", id);
        if self.fail_read.lock().unwrap().contains(id) {
            return Err(item_error(id, "injected read failure"));
        }
        Ok(self.raw(id).map(Zeroizing::new))
    }

    fn write(&self, id: &ItemId, text: &str) -> Result<(), SecretsError> {
        self.log("write", id);
        if self.fail_write.lock().unwrap().contains(id) {
            return Err(item_error(id, "injected write failure"));
        }
        let delay = *self.write_delay.lock().unwrap();
        if let Some(delay) = delay {
            std::thread::sleep(delay);
        }
        let stored = if self.garble_write.lock().unwrap().contains(id) {
            garbled(id, text)
        } else {
            text.to_owned()
        };
        self.items.lock().unwrap().insert(id.clone(), stored);
        Ok(())
    }

    fn delete(&self, id: &ItemId) -> Result<bool, SecretsError> {
        self.log("delete", id);
        let mut budget = self.delete_budget.lock().unwrap();
        if let Some(left) = budget.as_mut() {
            if *left == 0 {
                return Err(item_error(id, "injected delete failure"));
            }
            *left -= 1;
        }
        Ok(self.items.lock().unwrap().remove(id).is_some())
    }
}
