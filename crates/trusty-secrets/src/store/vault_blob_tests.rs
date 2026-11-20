//! Tests for the vault blob (#10524). Every store is the in-memory fake;
//! none touches the login Keychain.
//!
//! Test: itself.

use std::sync::Arc;
use std::time::Duration;

use super::*;
use crate::api::{SecretKey, SecretValue, VaultName};
use crate::store::mem_items::MemItems;
use crate::store::{BlobKeychain, ControlItem, SecretBackend};

const VALUE: &str = "sk-fake-blob-91c2";

fn item(n: usize) -> ItemId {
    ItemId::new("trusty/acme/web", format!("KEY_{n}"))
}

fn store_over(items: &Arc<MemItems>) -> BlobStore {
    BlobStore::new(Arc::clone(items) as Arc<dyn ItemStore>)
}

/// A blob holding `pairs`, encoded.
fn encoded(pairs: &[(&ItemId, &str)]) -> String {
    let mut blob = Blob::default();
    for (id, value) in pairs {
        blob.insert((*id).clone(), value);
    }
    blob.encode().unwrap().to_string()
}

/// Why: #10524 — the blob round-trips, its `Debug` is a count, and a blob
/// that does not parse is one fixed error that never quotes the input (a
/// serde message would: `invalid type: string "<value>"`).
/// Test: itself.
#[test]
fn vault_blob_round_trips_and_corrupt_text_never_leaks() {
    let text = encoded(&[(&item(1), VALUE), (&item(2), "")]);
    let blob = Blob::decode(&ItemId::blob(), &text).unwrap();
    assert_eq!(blob.len(), 2);
    assert_eq!(blob.get(&item(1)), Some(VALUE));
    assert!(!format!("{blob:?}").contains(VALUE));
    let leaky =
        format!(r#"{{"format":"trusty-secrets.vault-blob","version":"{VALUE}","items":[]}}"#);
    for bad in [
        leaky.as_str(),
        "{",
        r#"{"format":"x","version":1,"items":[]}"#,
    ] {
        let err = Blob::decode(&ItemId::blob(), bad).unwrap_err();
        let shown = format!("{err} / {err:?}");
        assert!(shown.contains("vault blob is corrupt"), "{shown}");
        assert!(!shown.contains(VALUE), "{shown}");
    }
    let twice = r#"{"format":"trusty-secrets.vault-blob","version":1,"items":[["a","b","1"],["a","b","2"]]}"#;
    assert!(
        Blob::decode(&ItemId::blob(), twice).is_err(),
        "a repeated item parsed"
    );
}

/// Why: #10524 AC1, AC2 — with no blob item each item is its own item (the
/// per-item layout, unchanged); once the blob exists every read and write
/// goes to it and the old items are neither read nor written.
/// Test: itself.
#[test]
fn blob_store_routes_by_layout() {
    let items = Arc::new(MemItems::with(&[(&item(1), "per-item")]));
    let store = store_over(&items);
    assert_eq!(store.get(&item(1)).unwrap().unwrap().as_str(), "per-item");
    store.put(&item(2), Some("new")).unwrap();
    assert_eq!(items.raw(&item(2)).as_deref(), Some("new"));

    items.put_raw(&ItemId::blob(), &encoded(&[(&item(1), VALUE)]));
    assert_eq!(store.get(&item(1)).unwrap().unwrap().as_str(), VALUE);
    assert!(
        store.get(&item(2)).unwrap().is_none(),
        "fell back to the old item"
    );
    assert!(store.put(&item(1), Some("changed")).unwrap());
    assert!(!store.put(&item(3), None).unwrap());
    assert_eq!(items.raw(&item(1)).as_deref(), Some("per-item"));
    let blob = store.read_blob().unwrap().unwrap();
    assert_eq!(blob.get(&item(1)), Some("changed"));
    assert!(store.put(&item(1), None).unwrap());
    assert!(store.read_blob().unwrap().unwrap().get(&item(1)).is_none());
}

/// Why: #10524 fail closed — a corrupt blob must fail every read and write;
/// a variant that fell back to the (possibly stale) per-item items returns
/// the old value and fails this test.
/// Test: itself.
#[test]
fn blob_store_corrupt_blob_fails_every_call() {
    let items = Arc::new(MemItems::with(&[(&item(1), "stale")]));
    items.put_raw(&ItemId::blob(), "not json");
    let store = store_over(&items);
    assert!(
        store.get(&item(1)).is_err(),
        "a corrupt blob read as per-item"
    );
    assert!(store.put(&item(1), Some("x")).is_err());
    assert_eq!(items.raw(&item(1)).as_deref(), Some("stale"));
    assert_eq!(items.raw(&ItemId::blob()).as_deref(), Some("not json"));
}

/// Why: #10524 AC1 — a failed blob write is an error and leaves the blob as
/// it was; it never reports success.
/// Test: itself.
#[test]
fn blob_store_failed_blob_write_changes_nothing() {
    let items = Arc::new(MemItems::default());
    items.put_raw(&ItemId::blob(), &encoded(&[(&item(1), VALUE)]));
    items.fail_write.lock().unwrap().insert(ItemId::blob());
    let store = store_over(&items);
    assert!(store.put(&item(1), Some("lost")).is_err());
    assert_eq!(store.get(&item(1)).unwrap().unwrap().as_str(), VALUE);
}

/// Why: #10524 AC1 — two writes racing in one process must both land. A
/// variant without the write lock lets each thread write back a blob read
/// before the other's write, and loses keys; this test then fails.
/// Test: itself.
#[test]
fn blob_store_concurrent_writes_keep_both() {
    let items = Arc::new(MemItems::default());
    items.put_raw(&ItemId::blob(), &encoded(&[]));
    *items.write_delay.lock().unwrap() = Some(Duration::from_millis(10));
    let store = Arc::new(store_over(&items));
    let threads: Vec<_> = (0..8)
        .map(|n| {
            let store = Arc::clone(&store);
            std::thread::spawn(move || store.put(&item(n), Some("v")).unwrap())
        })
        .collect();
    for thread in threads {
        thread.join().unwrap();
    }
    assert_eq!(
        store.read_blob().unwrap().unwrap().len(),
        8,
        "a write was lost"
    );
}

/// Why: #10524 AC1 — values and agent-control items both live in the one
/// blob item once migrated, so a control lookup under a grant needs no
/// second Keychain item.
/// Test: itself.
#[test]
fn blob_keychain_values_and_controls_share_the_blob() {
    let items = Arc::new(MemItems::default());
    items.put_raw(&ItemId::blob(), &encoded(&[]));
    let backend = BlobKeychain::new(Arc::new(store_over(&items)));
    let (vault, key) = (
        VaultName::new("trusty/acme/web").unwrap(),
        SecretKey::new("API_KEY").unwrap(),
    );
    backend.set(&vault, &key, &SecretValue::new(VALUE)).unwrap();
    backend
        .set_control_item(ControlItem::AgentDeny, &vault, Some(&key), true)
        .unwrap();
    assert_eq!(
        items.items.lock().unwrap().len(),
        1,
        "an item outside the blob"
    );
    assert_eq!(backend.get(&vault, &key).unwrap().unwrap().expose(), VALUE);
    assert!(
        backend
            .control_item(ControlItem::AgentDeny, &vault, Some(&key))
            .unwrap()
    );
    backend
        .set_control_item(ControlItem::AgentDeny, &vault, Some(&key), false)
        .unwrap();
    assert!(
        !backend
            .control_item(ControlItem::AgentDeny, &vault, Some(&key))
            .unwrap()
    );
    assert!(backend.blob_store().is_some());
}
