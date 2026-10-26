//! Tests for `store_credential_env` (#10041). Every store is a
//! `MemoryKeyStore` or a fake over one; every value is synthetic.

use std::path::PathBuf;

use serde_json::{Map, Value, json};
use trusty_common::credentials::{KeyStore, KeyStoreError, MemoryKeyStore};

use super::*;

/// An obviously fake Slack bot token.
const FAKE_TOKEN: &str = "xoxb-10041-not-a-real-token";

/// A store whose writes fail, or whose read-back lies.
struct BrokenStore {
    inner: MemoryKeyStore,
    fail_writes: bool,
    wrong_read_back: bool,
}

impl KeyStore for BrokenStore {
    fn get(&self, provider: &str) -> Option<String> {
        self.try_get(provider).ok().flatten()
    }
    fn try_get(&self, provider: &str) -> Result<Option<String>, KeyStoreError> {
        if self.wrong_read_back {
            return Ok(Some("xoxb-10041-something-else".to_string()));
        }
        Ok(self.inner.get(provider))
    }
    fn set(&self, provider: &str, value: &str) -> Result<(), KeyStoreError> {
        if self.fail_writes {
            return Err(KeyStoreError::Io {
                path: PathBuf::from("store.toml"),
                source: std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied"),
            });
        }
        self.inner.set(provider, value)
    }
    fn unset(&self, provider: &str) -> Result<(), KeyStoreError> {
        self.inner.unset(provider)
    }
    fn list(&self) -> Vec<String> {
        self.inner.list()
    }
}

fn env_of(pairs: &[(&str, &str)]) -> Map<String, Value> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), json!(v)))
        .collect()
}

#[test]
fn a_mapped_credential_moves_to_the_store_and_out_of_env() {
    let store = MemoryKeyStore::new();
    let mut env = env_of(&[("SLACK_BOT_TOKEN", FAKE_TOKEN), ("RUST_LOG", "info")]);

    let moved = store_credential_env(&mut env, &store).expect("stored");

    assert_eq!(moved, vec!["SLACK_BOT_TOKEN".to_string()]);
    assert!(!env.contains_key("SLACK_BOT_TOKEN"), "the value stayed in env");
    assert_eq!(env.get("RUST_LOG"), Some(&json!("info")));
    assert_eq!(store.get("slack").as_deref(), Some(FAKE_TOKEN));
}

/// #10041 failure path: a store miss must never fall back to the file.
#[test]
fn a_store_write_failure_keeps_the_value_out_of_env_and_errors() {
    let store = BrokenStore {
        inner: MemoryKeyStore::new(),
        fail_writes: true,
        wrong_read_back: false,
    };
    let mut env = env_of(&[("SLACK_BOT_TOKEN", FAKE_TOKEN)]);
    let before = env.clone();

    let err = store_credential_env(&mut env, &store).expect_err("must fail closed");

    let msg = format!("{err:#}");
    assert!(msg.contains("SLACK_BOT_TOKEN"), "{msg}");
    assert!(!msg.contains(FAKE_TOKEN), "value leaked: {msg}");
    // The env map is untouched AND the caller got Err, so it writes nothing.
    assert_eq!(env, before);
}

#[test]
fn a_read_back_mismatch_is_an_error() {
    let store = BrokenStore {
        inner: MemoryKeyStore::new(),
        fail_writes: false,
        wrong_read_back: true,
    };
    let mut env = env_of(&[("SLACK_BOT_TOKEN", FAKE_TOKEN)]);

    let err = store_credential_env(&mut env, &store).expect_err("mismatch must fail");

    assert!(format!("{err:#}").contains("mismatch"));
}

#[test]
fn an_unmapped_credential_key_is_refused_before_any_write() {
    let store = MemoryKeyStore::new();
    let mut env = env_of(&[
        ("SLACK_BOT_TOKEN", FAKE_TOKEN),
        ("ACME_API_KEY", "acme-10041-not-real"),
    ]);

    let err = store_credential_env(&mut env, &store).expect_err("unmapped must refuse");

    assert!(format!("{err:#}").contains("ACME_API_KEY"));
    assert!(store.list().is_empty(), "a refused add wrote the store");
    assert!(env.contains_key("SLACK_BOT_TOKEN"));
}

#[test]
fn ordinary_env_pairs_are_left_alone() {
    let store = MemoryKeyStore::new();
    let mut env = env_of(&[("RUST_LOG", "debug"), ("SLACK_CHANNEL", "C123")]);
    let before = env.clone();

    let moved = store_credential_env(&mut env, &store).expect("nothing to move");

    assert!(moved.is_empty());
    assert_eq!(env, before);
}
