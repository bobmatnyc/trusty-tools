//! Project-binding tests for `session::protocol::create` (AC-2.1,
//! AC-16.2). Split out of `protocol.rs` per the crate's `_tests.rs`
//! sibling-file convention so this production file stays under its
//! 500-SLOC cap.

use super::*;
use tokio::sync::mpsc;

fn ctx() -> ConnectionContext {
    let (tx, _rx) = mpsc::unbounded_channel();
    ConnectionContext::new(tx)
}

/// Omitting `project` is VALID and means projectless — a supported state,
/// never an error (AC-2.1). This is the entry state screen 7a renders.
#[tokio::test]
async fn create_without_project_is_projectless() {
    let registry = SessionRegistry::new();
    let workstreams = crate::workstreams::test_shared_store().await;
    let value = create(&registry, &workstreams, json!({"task": "just chat"}), ctx())
        .await
        .expect("omitting project must be valid");

    assert_eq!(value["binding"]["state"], "projectless");
    assert!(value["binding"]["root"].is_null());
    assert!(
        value["project"].is_null(),
        "projectless must derive no label"
    );
}

/// A real directory binds, and the derived label matches the binding — the
/// reconciliation in one assertion (AC-16.2).
#[tokio::test]
async fn create_binds_a_real_directory() {
    let registry = SessionRegistry::new();
    let workstreams = crate::workstreams::test_shared_store().await;
    let dir = tempfile::tempdir().expect("tempdir");
    let value = create(
        &registry,
        &workstreams,
        json!({"task": "t", "project": dir.path().to_string_lossy()}),
        ctx(),
    )
    .await
    .expect("a real directory must bind");

    // A non-git tempdir binds as `directory` — NOT projectless (#2728).
    assert_eq!(value["binding"]["state"], "directory");
    assert_eq!(
        value["project"], value["binding"]["root"],
        "the label must be derived from the binding root, not independent of it"
    );
}

/// The old free-form label is now rejected: `"my-app"` names no directory,
/// so it can bind nothing and index nothing. Erroring is the point — see
/// `create`'s docs. This is the deliberate breaking change.
#[tokio::test]
async fn create_rejects_a_label_that_is_not_a_directory() {
    let registry = SessionRegistry::new();
    let workstreams = crate::workstreams::test_shared_store().await;
    let err = create(
        &registry,
        &workstreams,
        json!({"task": "t", "project": "my-app"}),
        ctx(),
    )
    .await
    .expect_err("a decorative label must no longer be silently accepted");

    assert_eq!(err.code, -32003, "expected invalid_argument, got {err:?}");
}
