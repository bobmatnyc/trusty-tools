//! Unit tests for §7.4 atomic conversation persistence.
//!
//! Why: the state file is what makes a conversation survive a daemon restart; its
//! round-trip identity, atomic overwrite, and path-safety must be pinned, and the
//! storage root must be a tempdir (never the real home).
//! What: drives [`ConversationStore`] against a `tempfile::tempdir`.
//! Test: this is the test module.

use super::*;
use crate::core::sm::context::model::{Round, ToolTrace};
use chrono::{TimeZone, Utc};
use tempfile::tempdir;

/// Fixed deterministic timestamp for round fixtures.
fn ts() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 3, 4, 5, 6, 7)
        .single()
        .expect("valid ts")
}

/// Build a small, non-trivial conversation fixture.
fn sample() -> SmConversation {
    let mut c = SmConversation::new();
    c.compressed_context = "earlier: goal g-1 spawned s-2".to_string();
    c.recent_rounds.push_back(Round::new(
        "hi",
        "ho",
        ts(),
        vec![ToolTrace::new("session_new", "s-2")],
    ));
    c.recent_rounds
        .push_back(Round::new("again", "sure", ts(), Vec::new()));
    c.total_rounds = 12;
    c.token_estimate = 999;
    c
}

/// Why: the path layout is normative (`<root>/sm/conversation-<id>.json`).
/// What: asserts the dir segment and the filename.
/// Test: this is the test.
#[test]
fn store_path_is_under_sm_subdir() {
    let dir = tempdir().expect("tempdir");
    let store = ConversationStore::new(dir.path());
    assert!(store.dir().ends_with("sm"));
    let p = store.path_for("abc");
    assert!(p.ends_with("conversation-abc.json"));
    assert!(p.starts_with(store.dir()));
}

/// Why: the core §7.4 guarantee — save then load yields an identical value.
/// What: saves a fixture and asserts the loaded conversation equals it.
/// Test: this is the test.
#[test]
fn save_then_load_round_trips() {
    let dir = tempdir().expect("tempdir");
    let store = ConversationStore::new(dir.path());
    let conv = sample();
    store.save("c1", &conv).expect("save");
    let loaded = store.load("c1").expect("load");
    assert_eq!(loaded, conv);
}

/// Why: `save` must create the `sm/` subdir lazily; constructing the store does
/// no I/O, so the first save is responsible for the directory.
/// What: asserts the dir does not exist before save and the file exists after.
/// Test: this is the test.
#[test]
fn save_creates_sm_subdir() {
    let dir = tempdir().expect("tempdir");
    let store = ConversationStore::new(dir.path());
    assert!(!store.dir().exists(), "no I/O on construction");
    store.save("c1", &sample()).expect("save");
    assert!(
        store.path_for("c1").exists(),
        "state file exists after save"
    );
}

/// Why: re-saving must atomically overwrite — the second value must fully replace
/// the first with no leftover temp files.
/// What: saves twice with different content, asserts the latest loads and no
/// `.tmp` files remain in the directory.
/// Test: this is the test.
#[test]
fn save_is_atomic_overwrite() {
    let dir = tempdir().expect("tempdir");
    let store = ConversationStore::new(dir.path());

    let mut first = SmConversation::new();
    first.total_rounds = 1;
    store.save("c1", &first).expect("first save");

    let mut second = SmConversation::new();
    second.total_rounds = 2;
    second.compressed_context = "v2".to_string();
    store.save("c1", &second).expect("second save");

    let loaded = store.load("c1").expect("load");
    assert_eq!(loaded.total_rounds, 2);
    assert_eq!(loaded.compressed_context, "v2");

    // No temp files left behind.
    let leftovers: Vec<_> = std::fs::read_dir(store.dir())
        .expect("read sm dir")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains(".tmp."))
        .collect();
    assert!(
        leftovers.is_empty(),
        "no leftover temp files: {leftovers:?}"
    );
}

/// Why: a brand-new conv_id has no file; loading it must yield a fresh empty
/// conversation, not an error.
/// What: loads an unknown id and asserts it equals `SmConversation::new`.
/// Test: this is the test.
#[test]
fn load_missing_is_fresh() {
    let dir = tempdir().expect("tempdir");
    let store = ConversationStore::new(dir.path());
    let loaded = store.load("never-saved").expect("load missing");
    assert_eq!(loaded, SmConversation::new());
}

/// Why: a present-but-corrupt state file is a real problem the operator should
/// see — it must be a hard error, not a silent reset.
/// What: writes garbage to the state path, then asserts `load` returns `Serde`.
/// Test: this is the test.
#[test]
fn load_rejects_malformed_file() {
    let dir = tempdir().expect("tempdir");
    let store = ConversationStore::new(dir.path());
    std::fs::create_dir_all(store.dir()).expect("mkdir");
    std::fs::write(store.path_for("bad"), b"{ not json").expect("write garbage");
    let err = store.load("bad").expect_err("malformed must error");
    assert!(matches!(err, ConversationStoreError::Serde { .. }));
}

/// Why: an adversarial conv_id with path separators must not escape the `sm/`
/// directory; sanitisation must collapse it to a safe single segment.
/// What: saves with a traversal-y id and asserts the file lands inside the `sm/`
/// dir with a sanitised name.
/// Test: this is the test.
#[test]
fn conv_id_is_sanitised() {
    let dir = tempdir().expect("tempdir");
    let store = ConversationStore::new(dir.path());
    let nasty = "../../etc/passwd";
    store
        .save(nasty, &SmConversation::new())
        .expect("save sanitised");
    let p = store.path_for(nasty);
    assert!(p.starts_with(store.dir()), "path stays under sm dir: {p:?}");
    let name = p
        .file_name()
        .expect("filename")
        .to_string_lossy()
        .into_owned();
    assert!(!name.contains('/'), "no separators in filename: {name}");
    assert!(!name.contains(".."), "no traversal in filename: {name}");
    // And it round-trips under the sanitised id.
    let loaded = store.load(nasty).expect("load sanitised");
    assert_eq!(loaded, SmConversation::new());
}
