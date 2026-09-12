//! Unit tests for the MCP share store (#7672).
//!
//! Why: the store is a grant record, so the default and the fail-closed arms
//! matter more than the happy path — a test suite that only round-tripped a
//! share would pass against a store that returned "shared" for everything.
//! What: the empty default, round-tripping, idempotent mutation, owner-only
//! permissions, and the non-fatal accessor's fail-closed behaviour.
//! Test: this file.

use tempfile::TempDir;

use super::*;

#[test]
fn share_store_new_is_empty() {
    let tmp = TempDir::new().unwrap();
    let store = McpShareStore::load(tmp.path()).unwrap();

    assert!(
        !store.is_shared("slack-mcp"),
        "nothing is shared until the operator says so"
    );
    assert!(store.names().is_empty());
}

#[test]
fn share_inserts_and_is_idempotent() {
    let tmp = TempDir::new().unwrap();
    let mut store = McpShareStore::load(tmp.path()).unwrap();

    assert!(store.share("slack-mcp"));
    assert!(!store.share("slack-mcp"), "a re-share changes nothing");
    assert!(store.is_shared("slack-mcp"));
    assert!(!store.is_shared("other"));
}

#[test]
fn unshare_removes_and_is_idempotent() {
    let tmp = TempDir::new().unwrap();
    let mut store = McpShareStore::load(tmp.path()).unwrap();
    store.share("slack-mcp");

    assert!(store.unshare("slack-mcp"));
    assert!(!store.unshare("slack-mcp"));
    assert!(!store.is_shared("slack-mcp"));
}

#[test]
fn save_and_reload_round_trip() {
    let tmp = TempDir::new().unwrap();
    let mut store = McpShareStore::load(tmp.path()).unwrap();
    store.share("duetto-memory");
    store.share("slack-mcp");
    store.save().unwrap();

    let reloaded = McpShareStore::load(tmp.path()).unwrap();

    assert_eq!(
        reloaded.names().into_iter().collect::<Vec<_>>(),
        vec!["duetto-memory".to_owned(), "slack-mcp".to_owned()]
    );
}

/// This file grants access, so it is never group- or world-readable.
#[cfg(unix)]
#[test]
fn save_is_owner_only() {
    use std::os::unix::fs::PermissionsExt as _;

    let tmp = TempDir::new().unwrap();
    let mut store = McpShareStore::load(tmp.path()).unwrap();
    store.share("slack-mcp");
    store.save().unwrap();
    // Twice: the rewrite arm must hold the same mode as the create arm.
    store.save().unwrap();

    let mode = std::fs::metadata(tmp.path().join("mcp-shared.json"))
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600, "mode {mode:o}");
}

#[test]
fn load_refuses_a_malformed_store() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join("mcp-shared.json"), "{ not json").unwrap();

    assert!(
        McpShareStore::load(tmp.path()).is_err(),
        "a malformed grant record is an error the CLI must surface, never an \
         empty set it silently overwrites"
    );
}

/// The launch accessor must never abort a spawn, and must never guess "shared".
#[test]
#[serial_test::serial]
fn shared_servers_fails_closed_on_a_missing_root() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let prev = std::env::var_os("HOME");
    // SAFETY: this test is `#[serial]`, so no other test thread races the
    // set/restore, and `$HOME` is put back before it returns.
    unsafe { std::env::set_var("HOME", &home) };

    let names = shared_servers();

    match prev {
        Some(v) => unsafe { std::env::set_var("HOME", v) },
        None => unsafe { std::env::remove_var("HOME") },
    }
    assert!(
        names.is_empty(),
        "a home with no store shares nothing: {names:?}"
    );
}

#[test]
#[serial_test::serial]
fn shared_servers_reads_a_real_store() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let prev = std::env::var_os("HOME");
    // SAFETY: see `shared_servers_fails_closed_on_a_missing_root`.
    unsafe { std::env::set_var("HOME", &home) };

    let root = share_store_root().expect("HOME is set");
    let mut store = McpShareStore::load(&root).unwrap();
    store.share("slack-mcp");
    let saved = store.save();
    let names = shared_servers();

    match prev {
        Some(v) => unsafe { std::env::set_var("HOME", v) },
        None => unsafe { std::env::remove_var("HOME") },
    }
    saved.unwrap();
    assert!(
        names.contains("slack-mcp"),
        "the production accessor must resolve the real store: {names:?}"
    );
}
