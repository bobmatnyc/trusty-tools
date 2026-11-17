//! `SessionRecord::parent_session_id` persistence (#10476).
//!
//! Why: the field is additive on a store that older and newer daemons share,
//! so a parentless record and an old `sessions.json` must load and re-save
//! without gaining the key, and a recorded parent must survive a round trip.
//! What: serde round trips, the old-store load/re-save, and the
//! `set_parent_session_id` setter's happy and missing-session paths.
//! Test: this file IS the test module; run `cargo test -p trusty-mpm`.

use std::path::PathBuf;

use tempfile::TempDir;

use super::manager::ManagedError;
use super::record::{ManagedSessionId, SessionRecord};
use super::store::SessionStore;
use super::tests::make_manager;

/// A minimal record as an older daemon wrote it: no `parent_session_id` key.
const OLD_RECORD: &str = r#"{
    "id": "6f1c2b9e-8f3a-4b7d-9c1e-2a3b4c5d6e7f", "task": "t", "tmux_name": "tm-x",
    "cwd": "/tmp", "state": "stopped", "created_at": "2026-09-30T00:00:00Z"
}"#;

/// A pre-#10476 `sessions.json` (the D07 fixture predates this field).
const OLD_STORE: &str = include_str!("testdata/session-record-pre-d07.json");

#[test]
fn old_record_json_without_key_loads_none() {
    let record: SessionRecord = serde_json::from_str(OLD_RECORD).expect("old record loads");
    assert_eq!(record.parent_session_id, None);
}

#[test]
fn parentless_record_serializes_without_key() {
    let record: SessionRecord = serde_json::from_str(OLD_RECORD).expect("old record loads");
    let json = serde_json::to_value(&record).expect("serializes");
    assert!(json.get("parent_session_id").is_none(), "{json}");
}

#[test]
fn parent_round_trips_with_and_without_value() {
    let mut record: SessionRecord = serde_json::from_str(OLD_RECORD).expect("old record loads");
    let parent = ManagedSessionId::new();
    record.parent_session_id = Some(parent);
    let json = serde_json::to_string(&record).expect("serializes");
    let back: SessionRecord = serde_json::from_str(&json).expect("reloads");
    assert_eq!(back.parent_session_id, Some(parent));

    record.parent_session_id = None;
    let json = serde_json::to_string(&record).expect("serializes");
    let back: SessionRecord = serde_json::from_str(&json).expect("reloads");
    assert_eq!(back.parent_session_id, None);
}

/// Why: a rolling upgrade loads a store written by an older daemon; re-saving
/// it must not add the key to any record.
/// Test: itself.
#[tokio::test]
async fn old_sessions_json_resaves_without_new_key() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("sessions.json");
    std::fs::write(&path, OLD_STORE).expect("seed old store");

    let mut store = SessionStore::load(dir.path())
        .await
        .expect("old store loads");
    let records = store.all().await.expect("records");
    assert_eq!(records.len(), 3);
    assert!(records.iter().all(|r| r.parent_session_id.is_none()));
    store.save().await.expect("re-save");

    let written = std::fs::read_to_string(&path).expect("read back");
    assert!(!written.contains("parent_session_id"), "{written}");
}

#[tokio::test]
async fn set_parent_session_id_persists() {
    let dir = TempDir::new().expect("tempdir");
    let (mgr, _fake) = make_manager(&dir).await;
    let mk = || {
        mgr.create(
            "task".into(),
            Some(PathBuf::from("/tmp/wt")),
            None,
            None,
            None,
            None,
        )
    };
    let parent = mk().await.expect("create parent");
    let child = mk().await.expect("create child");

    mgr.set_parent_session_id(&child.id, parent.id)
        .await
        .expect("set_parent_session_id");

    assert_eq!(
        mgr.get(&child.id).await.expect("child").parent_session_id,
        Some(parent.id)
    );
    assert_eq!(
        mgr.get(&parent.id).await.expect("parent").parent_session_id,
        None
    );
    let written = std::fs::read_to_string(dir.path().join("sessions.json")).expect("store");
    assert_eq!(written.matches("parent_session_id").count(), 1, "{written}");
}

#[tokio::test]
async fn set_parent_session_id_missing_session_errors() {
    let dir = TempDir::new().expect("tempdir");
    let (mgr, _fake) = make_manager(&dir).await;
    let result = mgr
        .set_parent_session_id(&ManagedSessionId::new(), ManagedSessionId::new())
        .await;
    assert!(
        matches!(result, Err(ManagedError::SessionNotFound(_))),
        "expected SessionNotFound, got {result:?}"
    );
}

/// Why: setters that read the record without the store guard and upsert it
/// later can carry a `None` parent read before `link_parent` wrote it (#10476).
/// What: read a copy, link the parent, upsert the stale copy; the link stays.
/// A different `Some` parent still replaces the stored one.
/// Test: itself.
#[tokio::test]
async fn a_stale_upsert_keeps_the_stored_parent_link() {
    let dir = TempDir::new().expect("tempdir");
    let mut store = SessionStore::load(dir.path()).await.expect("store");
    let record: SessionRecord = serde_json::from_str(OLD_RECORD).expect("old record loads");
    store.upsert(record.clone()).await.expect("seed");

    let mut stale = store.get(&record.id).await.expect("stale read");
    let parent = ManagedSessionId::new();
    let mut linked = store.get(&record.id).await.expect("read");
    linked.parent_session_id = Some(parent);
    store.upsert(linked).await.expect("link parent");

    stale.task = "edited by a stale writer".into();
    store.upsert(stale).await.expect("stale upsert");
    let after = store.get(&record.id).await.expect("after");
    assert_eq!(
        after.parent_session_id,
        Some(parent),
        "stale copy erased the link"
    );
    assert_eq!(after.task, "edited by a stale writer");

    let other = ManagedSessionId::new();
    let mut relinked = after;
    relinked.parent_session_id = Some(other);
    store.upsert(relinked).await.expect("relink");
    let after = store.get(&record.id).await.expect("after relink");
    assert_eq!(after.parent_session_id, Some(other));
}
