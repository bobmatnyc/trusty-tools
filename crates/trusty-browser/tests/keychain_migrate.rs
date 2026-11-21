//! `migrate-keychain --from-stdin`: verify the key against every pairing, then take over the item.
//!
//! Stores are in-memory; nothing reaches a real Keychain.

mod common;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use trusty_browser::keychain_migrate::{MigrateError, migrate};
use trusty_browser::vault::{KeyStore, MemoryKeyStore, SERVICE, Vault, VaultError};

const OTHER_KEY: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";

/// A private state holding the Python fixture registry (two sealed pairings).
fn state_with_registry() -> tempfile::TempDir {
    let dir = common::state_dir();
    let db = dir.path().join("registry.sqlite3");
    fs::write(&db, common::read("registry.sqlite3")).expect("copy");
    fs::set_permissions(&db, fs::Permissions::from_mode(0o600)).expect("chmod");
    dir
}

fn account(state: &Path) -> String {
    fs::canonicalize(state)
        .expect("canon")
        .to_string_lossy()
        .into_owned()
}

#[test]
fn migrate_keychain_verifies_then_replaces_the_item() {
    let dir = state_with_registry();
    let store = MemoryKeyStore::new();
    let acct = account(dir.path());
    store.set(SERVICE, &acct, "old-python-item").expect("seed");
    let input = format!("{}\n", common::fixture_key());
    let report = migrate(dir.path(), &store, SERVICE, &mut input.as_bytes()).expect("migrate");
    assert_eq!(report.pairings, 2);
    assert!(report.replaced);
    assert_eq!(report.fingerprint.len(), 12);
    assert!(!common::fixture_key().contains(&report.fingerprint));
    assert_eq!(
        store.get(SERVICE, &acct).expect("get"),
        Some(common::fixture_key())
    );
    // The migrated item opens a pairing through the ordinary vault path.
    let reread = MemoryKeyStore::new();
    reread
        .set(SERVICE, &acct, &common::fixture_key())
        .expect("set");
    let vault = Vault::new(dir.path(), Box::new(reread)).expect("vault");
    let registry = trusty_browser::registry::Registry::open(dir.path()).expect("registry");
    let (profile, _) = registry.pairings().expect("rows")[0].clone();
    let row = registry.pairing(&profile).expect("row").expect("some");
    vault
        .unseal(&format!("browser:{profile}"), &row.browser_key)
        .expect("unseal");
}

#[test]
fn migrate_keychain_without_an_old_item_reports_absent() {
    let dir = state_with_registry();
    let store = MemoryKeyStore::new();
    let input = format!("{}\r\n", common::fixture_key());
    let report = migrate(dir.path(), &store, SERVICE, &mut input.as_bytes()).expect("migrate");
    assert!(!report.replaced);
}

#[test]
fn migrate_keychain_refuses_a_wrong_key_without_touching_the_store() {
    let dir = state_with_registry();
    let store = MemoryKeyStore::new();
    let acct = account(dir.path());
    store.set(SERVICE, &acct, "old-python-item").expect("seed");
    let err = migrate(dir.path(), &store, SERVICE, &mut OTHER_KEY.as_bytes()).expect_err("wrong");
    assert!(matches!(err, MigrateError::WrongKey(_)), "{err}");
    assert_eq!(
        store.get(SERVICE, &acct).expect("get").as_deref(),
        Some("old-python-item")
    );
}

#[test]
fn migrate_keychain_refuses_bad_input_and_an_empty_registry() {
    let dir = state_with_registry();
    let store = MemoryKeyStore::new();
    let long = "A".repeat(5000);
    let two = format!("{}\n{}\n", common::fixture_key(), common::fixture_key());
    for bad in ["", "not base64!", "AAAA", two.as_str(), long.as_str()] {
        let err = migrate(dir.path(), &store, SERVICE, &mut bad.as_bytes()).expect_err(bad);
        assert!(matches!(err, MigrateError::BadKey), "{bad}: {err}");
    }
    let empty = common::state_dir();
    let err = migrate(
        empty.path(),
        &store,
        SERVICE,
        &mut common::fixture_key().as_bytes(),
    )
    .expect_err("empty");
    assert!(matches!(err, MigrateError::NothingToVerify(_)), "{err}");
    assert!(!empty.path().join("registry.sqlite3").exists());
}

/// Deletes fine, then fails `set` or answers `get` wrongly.
struct Broken {
    fail_set: bool,
}

impl KeyStore for Broken {
    fn get(&self, _: &str, _: &str) -> Result<Option<String>, VaultError> {
        Ok(Some("something else".into()))
    }
    fn set(&self, _: &str, _: &str, _: &str) -> Result<(), VaultError> {
        if self.fail_set {
            Err(VaultError::Store("denied".into()))
        } else {
            Ok(())
        }
    }
    fn delete(&self, _: &str, _: &str) -> Result<bool, VaultError> {
        Ok(true)
    }
}

#[test]
fn migrate_keychain_reports_a_lost_write() {
    let dir = state_with_registry();
    let key = common::fixture_key();
    let err = migrate(
        dir.path(),
        &Broken { fail_set: true },
        SERVICE,
        &mut key.as_bytes(),
    )
    .expect_err("lost");
    assert!(matches!(err, MigrateError::WriteLost(_)), "{err}");
    assert!(err.to_string().contains("re-run with the same key"));
    let err = migrate(
        dir.path(),
        &Broken { fail_set: false },
        SERVICE,
        &mut key.as_bytes(),
    )
    .expect_err("readback");
    assert!(matches!(err, MigrateError::ReadBack), "{err}");
}
