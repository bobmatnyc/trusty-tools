//! The one test that touches the real OS keychain.
//!
//! Why: every other test runs against a double; this proves the Keychain
//! backend and the index compose against the real login Keychain. Ignored by
//! default because it writes to the user's Keychain. Run it explicitly:
//! `cargo test -p trusty-secrets --test keychain_roundtrip -- --ignored`.
//! What: store → list → read → remove in a throwaway vault unique to this
//! run, with a temp-dir index. A drop guard deletes the Keychain entry even
//! when an assertion fails.
//! Test: itself.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use trusty_secrets::store::{KeychainBackend, NamesIndex, ScopeSet, SecretBackend, SecretStore};
use trusty_secrets::{SecretKey, SecretRef, SecretValue, VaultName};

/// Deletes the Keychain entry on drop, so a failed assertion leaves nothing.
struct Cleanup {
    vault: VaultName,
    key: SecretKey,
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = KeychainBackend::new().delete(&self.vault, &self.key);
    }
}

/// Why: see the module docs.
/// Test: itself.
#[test]
#[ignore = "touches the real OS keychain; run explicitly with --ignored"]
fn keychain_real_roundtrip_store_list_remove() {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .expect("clock after 1970");
    let owner = format!("trusty-secrets-test-{}-{nanos}", std::process::id());
    let vault = VaultName::new(&format!("trusty/{owner}/roundtrip")).expect("valid vault");
    let key = SecretKey::new("TRUSTY_SECRETS_ROUNDTRIP").expect("valid key");
    let _cleanup = Cleanup {
        vault: vault.clone(),
        key: key.clone(),
    };

    let tmp = tempfile::TempDir::new().expect("temp dir");
    let store = SecretStore::new(
        Arc::new(KeychainBackend::new()),
        NamesIndex::at(tmp.path().join("index")),
    );
    let value = SecretValue::new("roundtrip-value-not-a-secret");

    store.set(&vault, &key, &value).expect("store");
    let names: Vec<String> = store
        .list(&vault)
        .expect("list")
        .into_iter()
        .map(|m| m.name.to_string())
        .collect();
    assert_eq!(names, [key.to_string()]);

    let scopes = ScopeSet::new(vault.clone(), None);
    let reference = SecretRef::parse(&format!("secret://{key}")).expect("reference");
    assert_eq!(
        store.read(&reference, &scopes).expect("read").expose(),
        value.expose()
    );

    assert!(store.delete(&vault, &key).expect("remove").removed);
    assert!(store.list(&vault).expect("list").is_empty());
    assert!(
        KeychainBackend::new()
            .get(&vault, &key)
            .expect("get after remove")
            .is_none()
    );
}
