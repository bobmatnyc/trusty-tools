//! Unit tests for `SessionManager::set_deliverable_id` (DOC-35 §10.6, #2379).
//!
//! Why: split out of `tests.rs` rather than grown further, mirroring the
//! established `set_source_id_tests.rs`/`restart_tests.rs`/`reactivate_tests.rs`
//! sibling-test-file pattern.
//! What: the happy path (persists and round-trips through `get`) and the
//! missing-session path (a typed `SessionNotFound`, not a panic or silent no-op).
//! Test: this file IS the test module; run with `cargo test -p trusty-mpm`.

use std::path::PathBuf;

use tempfile::TempDir;

use super::manager::ManagedError;
use super::record::ManagedSessionId;
use super::tests::make_manager;
use crate::deliverable::DeliverableId;

#[tokio::test]
async fn set_deliverable_id_persists() {
    let dir = TempDir::new().unwrap();
    let (mgr, _fake) = make_manager(&dir).await;

    let record = mgr
        .create(
            "task".into(),
            Some(PathBuf::from("/tmp/wt")),
            None,
            None,
            None,
            None,
        )
        .await
        .expect("create");

    let did = DeliverableId::new();
    mgr.set_deliverable_id(&record.id, did)
        .await
        .expect("set_deliverable_id");

    let reloaded = mgr.get(&record.id).await.expect("get after set");
    assert_eq!(reloaded.deliverable_id, Some(did));
}

#[tokio::test]
async fn set_deliverable_id_missing_session_errors() {
    let dir = TempDir::new().unwrap();
    let (mgr, _fake) = make_manager(&dir).await;
    let unknown_id = ManagedSessionId::new();

    let result = mgr
        .set_deliverable_id(&unknown_id, DeliverableId::new())
        .await;
    assert!(
        matches!(result, Err(ManagedError::SessionNotFound(_))),
        "expected SessionNotFound for a session that does not exist, got {result:?}"
    );
}
