//! Unit tests for the MCP share store (#7672).
//!
//! Why: the store is a grant record, so the default and the fail-closed arms
//! matter more than the happy path — a test suite that only round-tripped a
//! share would pass against a store that returned "shared" for everything.
//! What: the empty default, round-tripping, idempotent mutation, the content
//! digest a grant is bound to, a malformed digest, owner-only permissions, and
//! the non-fatal accessor's fail-closed behaviour.
//! Test: this file.

use tempfile::TempDir;

use super::*;

/// A well-formed digest that is no real server's — the store checks the SHAPE
/// only, and the tests that need a real one compute it from an entry.
fn a_digest() -> String {
    "a".repeat(64)
}

/// A second well-formed digest, for the "its content changed" cases.
fn another_digest() -> String {
    "b".repeat(64)
}

#[test]
fn share_store_new_is_empty() {
    let tmp = TempDir::new().unwrap();
    let store = McpShareStore::load(tmp.path()).unwrap();

    assert!(
        store.digest_for("slack-mcp").is_none(),
        "nothing is shared until the operator says so"
    );
    assert!(store.grants().is_empty());
}

#[test]
fn share_records_the_digest_and_is_idempotent() {
    let tmp = TempDir::new().unwrap();
    let mut store = McpShareStore::load(tmp.path()).unwrap();

    assert!(store.share("slack-mcp", &a_digest()));
    assert!(
        !store.share("slack-mcp", &a_digest()),
        "re-sharing the same content changes nothing"
    );
    assert_eq!(store.digest_for("slack-mcp"), Some(a_digest().as_str()));
    assert!(
        store.share("slack-mcp", &another_digest()),
        "sharing the server again after its spec changed is a NEW decision \
         about new content, so it replaces the recorded digest"
    );
    assert_eq!(
        store.digest_for("slack-mcp"),
        Some(another_digest().as_str())
    );
    assert!(store.digest_for("other").is_none());
}

#[test]
fn unshare_removes_and_is_idempotent() {
    let tmp = TempDir::new().unwrap();
    let mut store = McpShareStore::load(tmp.path()).unwrap();
    store.share("slack-mcp", &a_digest());

    assert!(store.unshare("slack-mcp"));
    assert!(!store.unshare("slack-mcp"));
    assert!(store.digest_for("slack-mcp").is_none());
}

#[test]
fn save_and_reload_round_trip() {
    let tmp = TempDir::new().unwrap();
    let mut store = McpShareStore::load(tmp.path()).unwrap();
    store.share("duetto-memory", &a_digest());
    store.share("slack-mcp", &another_digest());
    store.save().unwrap();

    let reloaded = McpShareStore::load(tmp.path()).unwrap();

    assert_eq!(
        reloaded.grants(),
        BTreeMap::from([
            ("duetto-memory".to_owned(), a_digest()),
            ("slack-mcp".to_owned(), another_digest()),
        ]),
        "a grant is the name AND the content it was granted for"
    );
}

/// PR #7692 re-review, Fail-Open Check: the sidecar sits under `$HOME` and can
/// be hand-edited, so a digest that is not the shape `spec_digest` emits must
/// read as NOT SHARED — never as a value that might compare equal to something.
#[test]
fn a_malformed_digest_is_dropped_at_load() {
    let tmp = TempDir::new().unwrap();
    let good = a_digest();
    let body = format!(
        r#"{{"shared_servers":{{"nonsense":"not-a-digest","short":"abc123","upper":"{}","good":"{good}"}}}}"#,
        "A".repeat(64)
    );
    std::fs::write(tmp.path().join("mcp-shared.json"), body).unwrap();

    let store = McpShareStore::load(tmp.path()).unwrap();

    assert!(
        store.digest_for("nonsense").is_none(),
        "a non-hex digest grants nothing"
    );
    assert!(
        store.digest_for("short").is_none(),
        "a truncated digest grants nothing"
    );
    assert!(
        store.digest_for("upper").is_none(),
        "uppercase hex is not the shape spec_digest emits"
    );
    assert_eq!(
        store.digest_for("good"),
        Some(good.as_str()),
        "a well-formed neighbour still loads"
    );
}

/// This file grants access, so it is never group- or world-readable.
#[cfg(unix)]
#[test]
fn save_is_owner_only() {
    use std::os::unix::fs::PermissionsExt as _;

    let tmp = TempDir::new().unwrap();
    let mut store = McpShareStore::load(tmp.path()).unwrap();
    store.share("slack-mcp", &a_digest());
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

    let grants = shared_servers();

    match prev {
        Some(v) => unsafe { std::env::set_var("HOME", v) },
        None => unsafe { std::env::remove_var("HOME") },
    }
    assert!(
        grants.is_empty(),
        "a home with no store shares nothing: {grants:?}"
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
    store.share("slack-mcp", &a_digest());
    let saved = store.save();
    let grants = shared_servers();

    match prev {
        Some(v) => unsafe { std::env::set_var("HOME", v) },
        None => unsafe { std::env::remove_var("HOME") },
    }
    saved.unwrap();
    assert_eq!(
        grants.get("slack-mcp").map(String::as_str),
        Some(a_digest().as_str()),
        "the production accessor must resolve the real store: {grants:?}"
    );
}
