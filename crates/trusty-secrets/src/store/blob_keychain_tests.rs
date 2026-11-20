//! #10524 merge condition: with no vault blob item (no `tm secrets
//! migrate`) and no unlock grant, every Keychain path behaves as the
//! per-item layout on origin/main did.

use std::sync::Arc;

use super::*;
use crate::api::{SecretKey, SecretValue, SecretsError, VaultName};
use crate::store::mem_items::MemItems;

fn names() -> (VaultName, SecretKey) {
    (
        VaultName::new("trusty/acme/web").unwrap(),
        SecretKey::new("API_KEY").unwrap(),
    )
}

/// The backend the server opens, over `items`: [`BlobKeychain`], wrapped
/// in [`super::super::HeldKeychain`] with a cache that holds no grant.
fn backend(items: &Arc<MemItems>) -> Arc<dyn SecretBackend> {
    let store = Arc::new(BlobStore::new(Arc::clone(items) as Arc<_>));
    let inner: Arc<dyn SecretBackend> = Arc::new(BlobKeychain::new(store));
    #[cfg(feature = "server")]
    {
        let cache = Arc::new(super::super::SecretCache::new(
            std::time::Duration::from_secs(600),
        ));
        Arc::new(super::super::HeldKeychain::new(inner, cache))
    }
    #[cfg(not(feature = "server"))]
    inner
}

/// Why: #10524 merge condition — before `tm secrets migrate`, put, get,
/// remove, list and agent-control calls must reach the same per-item
/// Keychain items (origin's service/account names) with the same results,
/// and the only extra call is a read-only probe of the absent blob item. A
/// variant that answers a default read from the blob path (an absent blob
/// read as an empty blob) returns `None` for the stored value and fails
/// this test; one that creates the blob on a write fails the no-blob check.
/// Test: itself.
#[test]
fn no_blob_default_paths_use_per_item_layout() {
    let items = Arc::new(MemItems::default());
    let backend = backend(&items);
    let (vault, key) = names();
    let value_id = ItemId::new("trusty/acme/web", "API_KEY");
    let deny_vault = ItemId::new(control_service(ControlItem::AgentDeny), "trusty/acme/web");
    let deny_key = ItemId::new(
        control_service(ControlItem::AgentDeny),
        "trusty/acme/web/API_KEY",
    );

    backend.set(&vault, &key, &SecretValue::new("v1")).unwrap();
    assert_eq!(backend.get(&vault, &key).unwrap().unwrap().expose(), "v1");
    // As on origin, the Keychain cannot list (the index does), and touches
    // no item trying.
    assert!(matches!(
        backend.list_names(&vault),
        Err(SecretsError::Unsupported { .. })
    ));
    backend
        .set_control_item(ControlItem::AgentDeny, &vault, Some(&key), true)
        .unwrap();
    assert!(
        backend
            .control_item(ControlItem::AgentDeny, &vault, Some(&key))
            .unwrap()
    );
    assert!(
        !backend
            .control_item(ControlItem::AgentDeny, &vault, None)
            .unwrap()
    );
    assert_eq!(items.raw(&value_id).as_deref(), Some("v1"));
    assert_eq!(items.raw(&deny_key).as_deref(), Some("1"));
    assert!(backend.delete(&vault, &key).unwrap());
    assert!(!backend.delete(&vault, &key).unwrap());
    assert!(backend.get(&vault, &key).unwrap().is_none());
    backend
        .set_control_item(ControlItem::AgentDeny, &vault, Some(&key), false)
        .unwrap();

    let blob = ItemId::blob();
    let probe = ("read", blob.clone());
    let expected = vec![
        probe.clone(),
        ("write", value_id.clone()),
        probe.clone(),
        ("read", value_id.clone()),
        probe.clone(),
        ("write", deny_key.clone()),
        probe.clone(),
        ("read", deny_key.clone()),
        probe.clone(),
        ("read", deny_vault),
        probe.clone(),
        ("delete", value_id.clone()),
        probe.clone(),
        ("delete", value_id.clone()),
        probe.clone(),
        ("read", value_id),
        probe,
        ("delete", deny_key),
    ];
    let calls = items.calls.lock().unwrap().clone();
    assert_eq!(calls, expected);
    // The blob item is only ever read, and never comes into being.
    assert!(calls.iter().all(|(op, id)| *id != blob || *op == "read"));
    assert!(items.raw(&blob).is_none(), "a blob item was created");
    assert!(items.items.lock().unwrap().is_empty());
}

fn backend_error(err: SecretsError) -> (String, String, String) {
    match err {
        SecretsError::Backend {
            backend,
            vault,
            key,
            reason,
        } => {
            assert_eq!(backend, "keychain");
            (vault, key, reason)
        }
        other => panic!("not a backend error: {other:?}"),
    }
}

/// Why: #10524 merge condition — a failed Keychain call names the item the
/// caller asked for, as on origin/main: a value names its vault and key, a
/// control item its vault and key, or `(none)` for a per-vault item. A
/// variant that passes the store's item error through names the control
/// service or the blob item instead and fails this test.
/// Test: itself.
#[test]
fn no_blob_failures_name_the_requested_item() {
    let (vault, key) = names();
    let want = |k: &str| ("trusty/acme/web".to_string(), k.to_string());

    let items = Arc::new(MemItems::default());
    let value_id = ItemId::new("trusty/acme/web", "API_KEY");
    items.fail_read.lock().unwrap().insert(value_id.clone());
    items.fail_write.lock().unwrap().insert(value_id);
    let b = backend(&items);
    let (v, k, reason) = backend_error(b.get(&vault, &key).unwrap_err());
    assert_eq!((v, k), want("API_KEY"));
    assert_eq!(reason, "injected read failure");
    let (v, k, _) = backend_error(b.set(&vault, &key, &SecretValue::new("x")).unwrap_err());
    assert_eq!((v, k), want("API_KEY"));

    let items = Arc::new(MemItems::default());
    let service = control_service(ControlItem::AgentDeny);
    items
        .fail_read
        .lock()
        .unwrap()
        .insert(ItemId::new(service, "trusty/acme/web"));
    let b = backend(&items);
    let err = b
        .control_item(ControlItem::AgentDeny, &vault, None)
        .unwrap_err();
    assert_eq!(backend_error(err).0, "trusty/acme/web");
    let err = b
        .control_item(ControlItem::AgentDeny, &vault, None)
        .unwrap_err();
    assert_eq!(backend_error(err).1, "(none)");

    // A failed probe of the blob item names the requested item too.
    let items = Arc::new(MemItems::default());
    items.fail_read.lock().unwrap().insert(ItemId::blob());
    let b = backend(&items);
    let (v, k, _) = backend_error(b.get(&vault, &key).unwrap_err());
    assert_eq!((v, k), want("API_KEY"));
    let err = b
        .control_item(ControlItem::AgentDeny, &vault, Some(&key))
        .unwrap_err();
    let (v, k, _) = backend_error(err);
    assert_eq!((v, k), want("API_KEY"));
}
