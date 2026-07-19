//! `session.create`'s workstream-binding tests (DOC-48 §4.1/§4.2, issue
//! #3298). Split out of `protocol.rs` per the crate's `_tests.rs`
//! sibling-file convention (see `registry_tests`/`sessions_write_tests` for
//! precedent) so this production file stays under its 500-SLOC cap.

use super::*;
use tokio::sync::mpsc;

fn ctx() -> ConnectionContext {
    let (tx, _rx) = mpsc::unbounded_channel();
    ConnectionContext::new(tx)
}

/// Seed a workstream and activate it, returning the shared store and the
/// workstream's id.
async fn store_with_active_workstream() -> (
    crate::workstreams::SharedWorkstreamStore,
    crate::workstreams::WorkstreamId,
) {
    let store = crate::workstreams::test_shared_store().await;
    let id = store.lock().await.create("active").await.expect("create");
    crate::workstreams::activation::activate(&store, id, false)
        .await
        .expect("activate");
    (store, id)
}

/// A `session.create` call with no explicit `workstream_id`, while a
/// workstream is active, must bind to the ACTIVE workstream (DOC-48 §4.2's
/// ambient default target).
#[tokio::test]
async fn create_binds_ambient_active_workstream() {
    let registry = SessionRegistry::new();
    let (workstreams, active_id) = store_with_active_workstream().await;

    let value = create(&registry, &workstreams, json!({"task": "t"}), ctx())
        .await
        .expect("create must succeed");

    assert_eq!(value["workstream_id"], active_id.to_string());
    let ws = workstreams.lock().await.get(active_id).await.expect("get");
    assert_eq!(
        ws.session_ids,
        vec![value["id"].as_str().unwrap().to_string()]
    );
}

/// An explicit `workstream_id` param must win over the ambient active
/// workstream (DOC-48 §4.1).
#[tokio::test]
async fn create_binds_explicit_workstream_overriding_ambient() {
    let registry = SessionRegistry::new();
    let (workstreams, _active_id) = store_with_active_workstream().await;
    let explicit_id = workstreams
        .lock()
        .await
        .create("explicit")
        .await
        .expect("create");

    let value = create(
        &registry,
        &workstreams,
        json!({"task": "t", "workstream_id": explicit_id.to_string()}),
        ctx(),
    )
    .await
    .expect("create must succeed");

    assert_eq!(value["workstream_id"], explicit_id.to_string());
}

/// With no explicit param and nothing active, a session must stay
/// projectless — a fully valid state, never an error (DOC-48 §4.2).
#[tokio::test]
async fn create_stays_projectless_without_explicit_or_active() {
    let registry = SessionRegistry::new();
    let workstreams = crate::workstreams::test_shared_store().await;

    let value = create(&registry, &workstreams, json!({"task": "t"}), ctx())
        .await
        .expect("create must succeed");

    assert!(value["workstream_id"].is_null());
}

/// An explicit `workstream_id` naming a CLOSED workstream must reject the
/// whole `session.create` call (DOC-48 §4.1: "new sessions may not bind to
/// it").
#[tokio::test]
async fn create_rejects_closed_explicit_workstream() {
    let registry = SessionRegistry::new();
    let workstreams = crate::workstreams::test_shared_store().await;
    let id = workstreams
        .lock()
        .await
        .create("closed")
        .await
        .expect("create");
    workstreams.lock().await.close(id).await.expect("close");

    let err = create(
        &registry,
        &workstreams,
        json!({"task": "t", "workstream_id": id.to_string()}),
        ctx(),
    )
    .await
    .expect_err("closed workstream must reject the bind");
    assert_eq!(err.code, -32003);
}
