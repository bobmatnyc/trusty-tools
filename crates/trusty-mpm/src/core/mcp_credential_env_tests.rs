//! Tests for `store_credential_env` (#10041). Every store is a
//! `MemoryKeyStore` or a fake over one; every value is synthetic.

use std::path::PathBuf;

use serde_json::{Map, Value, json};
use trusty_common::credentials::{KeyStore, KeyStoreError, MemoryKeyStore};

use super::*;

/// An obviously fake Slack bot token.
const FAKE_TOKEN: &str = "xoxb-10041-not-a-real-token";

/// A store whose reads or writes fail, or whose read-back after a write lies.
struct BrokenStore {
    inner: MemoryKeyStore,
    fail_writes: bool,
    wrong_read_back: bool,
    fail_reads: bool,
}

impl KeyStore for BrokenStore {
    fn get(&self, provider: &str) -> Option<String> {
        self.try_get(provider).ok().flatten()
    }
    fn try_get(&self, provider: &str) -> Result<Option<String>, KeyStoreError> {
        if self.fail_reads {
            return Err(KeyStoreError::Io {
                path: PathBuf::from("store.toml"),
                source: std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied"),
            });
        }
        // Lies only once something was written, so the pre-write read sees
        // an empty slot.
        if self.wrong_read_back && self.inner.get(provider).is_some() {
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

/// A store-reading trusty server, no `--replace`.
const SLACK: CredentialTarget<'static> = CredentialTarget {
    server: "slack-mcp",
    command: "slack-mcp",
    replace: false,
};

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

    let moved = store_credential_env(&mut env, &store, SLACK).expect("stored");

    assert_eq!(moved, vec!["SLACK_BOT_TOKEN".to_string()]);
    assert!(
        !env.contains_key("SLACK_BOT_TOKEN"),
        "the value stayed in env"
    );
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
        fail_reads: false,
    };
    let mut env = env_of(&[("SLACK_BOT_TOKEN", FAKE_TOKEN)]);
    let before = env.clone();

    let err = store_credential_env(&mut env, &store, SLACK).expect_err("must fail closed");

    let msg = format!("{err:#}");
    assert!(msg.contains("SLACK_BOT_TOKEN"), "{msg}");
    assert!(!msg.contains(FAKE_TOKEN), "value leaked: {msg}");
    // The env map is untouched AND the caller got Err, so it writes nothing.
    assert_eq!(env, before);
}

/// #10041: a store that cannot be read refuses the add; it is never treated
/// as empty, so a stored credential cannot be overwritten unseen.
#[test]
fn a_store_read_failure_refuses_and_writes_nothing() {
    let store = BrokenStore {
        inner: MemoryKeyStore::new(),
        fail_writes: false,
        wrong_read_back: false,
        fail_reads: true,
    };
    let mut env = env_of(&[("SLACK_BOT_TOKEN", FAKE_TOKEN)]);
    let before = env.clone();

    let err = store_credential_env(&mut env, &store, SLACK).expect_err("unreadable store refuses");

    let msg = format!("{err:#}");
    assert!(msg.contains("could not read"), "{msg}");
    assert!(!msg.contains(FAKE_TOKEN), "value leaked: {msg}");
    assert!(store.inner.get("slack").is_none(), "nothing may be stored");
    assert_eq!(env, before);
}

#[test]
fn a_read_back_mismatch_is_an_error() {
    let store = BrokenStore {
        inner: MemoryKeyStore::new(),
        fail_writes: false,
        wrong_read_back: true,
        fail_reads: false,
    };
    let mut env = env_of(&[("SLACK_BOT_TOKEN", FAKE_TOKEN)]);

    let err = store_credential_env(&mut env, &store, SLACK).expect_err("mismatch must fail");

    assert!(format!("{err:#}").contains("mismatch"));
}

#[test]
fn an_unmapped_credential_key_is_refused_before_any_write() {
    let store = MemoryKeyStore::new();
    let mut env = env_of(&[
        ("SLACK_BOT_TOKEN", FAKE_TOKEN),
        ("ACME_API_KEY", "acme-10041-not-real"),
    ]);

    let err = store_credential_env(&mut env, &store, SLACK).expect_err("unmapped must refuse");

    assert!(format!("{err:#}").contains("ACME_API_KEY"));
    assert!(store.list().is_empty(), "a refused add wrote the store");
    assert!(env.contains_key("SLACK_BOT_TOKEN"));
}

#[test]
fn ordinary_env_pairs_are_left_alone() {
    let store = MemoryKeyStore::new();
    let mut env = env_of(&[("RUST_LOG", "debug"), ("SLACK_CHANNEL", "C123")]);
    let before = env.clone();

    let moved = store_credential_env(&mut env, &store, SLACK).expect("nothing to move");

    assert!(moved.is_empty());
    assert_eq!(env, before);
}

#[test]
fn store_reading_commands_match_by_basename() {
    assert!(reads_credential_store("slack-mcp"));
    assert!(reads_credential_store("/home/u/.cargo/bin/telegram-mcp"));
    assert!(!reads_credential_store("npx"));
    assert!(!reads_credential_store("/usr/local/bin/slack-mcp-wrapper"));
}

/// #10041: a third-party server never reads tm's store, so its credential is
/// refused — not moved where it would never be read.
#[test]
fn a_third_party_server_credential_is_refused_and_not_stored() {
    let store = MemoryKeyStore::new();
    let mut env = env_of(&[("GITHUB_TOKEN", "ghp_10041notarealtoken")]);
    let before = env.clone();
    let target = CredentialTarget {
        server: "github",
        command: "npx",
        replace: false,
    };

    let err = store_credential_env(&mut env, &store, target).expect_err("must refuse");

    let msg = format!("{err:#}");
    assert!(
        msg.contains("GITHUB_TOKEN") && msg.contains("'github'"),
        "{msg}"
    );
    assert!(
        !msg.contains("ghp_10041notarealtoken"),
        "value leaked: {msg}"
    );
    assert!(store.list().is_empty(), "a refused add wrote the store");
    assert_eq!(env, before);
}

/// #10041 error arm: a different stored value is never overwritten silently.
#[test]
fn a_different_stored_value_is_refused_without_replace() {
    let store = MemoryKeyStore::new();
    store
        .set("slack", "xoxb-10041-the-stored-one")
        .expect("seed");
    let mut env = env_of(&[("SLACK_BOT_TOKEN", FAKE_TOKEN)]);
    let before = env.clone();

    let err = store_credential_env(&mut env, &store, SLACK).expect_err("must refuse");

    let msg = format!("{err:#}");
    assert!(
        msg.contains("SLACK_BOT_TOKEN") && msg.contains("--replace"),
        "{msg}"
    );
    assert!(!msg.contains(FAKE_TOKEN), "value leaked: {msg}");
    assert_eq!(
        store.get("slack").as_deref(),
        Some("xoxb-10041-the-stored-one")
    );
    assert_eq!(env, before);
}

#[test]
fn replace_overwrites_a_different_stored_value() {
    let store = MemoryKeyStore::new();
    store
        .set("slack", "xoxb-10041-the-stored-one")
        .expect("seed");
    let mut env = env_of(&[("SLACK_BOT_TOKEN", FAKE_TOKEN)]);
    let target = CredentialTarget {
        replace: true,
        ..SLACK
    };

    let moved = store_credential_env(&mut env, &store, target).expect("replaced");

    assert_eq!(moved, vec!["SLACK_BOT_TOKEN".to_string()]);
    assert_eq!(store.get("slack").as_deref(), Some(FAKE_TOKEN));
    assert!(!env.contains_key("SLACK_BOT_TOKEN"));
}

#[test]
fn an_equal_stored_value_is_a_no_op_success() {
    // A store that fails every write proves no write happens.
    let store = BrokenStore {
        inner: MemoryKeyStore::new(),
        fail_writes: true,
        wrong_read_back: false,
        fail_reads: false,
    };
    store.inner.set("slack", FAKE_TOKEN).expect("seed");
    let mut env = env_of(&[("SLACK_BOT_TOKEN", FAKE_TOKEN)]);

    let moved = store_credential_env(&mut env, &store, SLACK).expect("no write needed");

    assert_eq!(moved, vec!["SLACK_BOT_TOKEN".to_string()]);
    assert!(!env.contains_key("SLACK_BOT_TOKEN"));
}
