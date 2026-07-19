//! Tests for `activation::activate`/`activation::deactivate` (DOC-48 §6,
//! issue #3294; the `Event::WorkstreamActivationChanged` publication tests
//! are issue #3297).

use std::time::Duration;

use super::*;
use tempfile::tempdir;

/// Read the next event off `rx`, failing the test if none arrives within 2s.
async fn recv_event(rx: &mut tokio::sync::broadcast::Receiver<SessionEventEnvelope>) -> Event {
    tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .expect("timed out waiting for a published event")
        .expect("event bus channel closed")
        .event
}

/// Assert NO event arrives on `rx` within a short window — used to prove the
/// idempotent no-op branches of `activate`/`deactivate` do not publish.
async fn assert_no_event(rx: &mut tokio::sync::broadcast::Receiver<SessionEventEnvelope>) {
    let result = tokio::time::timeout(Duration::from_millis(200), rx.recv()).await;
    assert!(
        result.is_err(),
        "expected no event to be published, but got one"
    );
}

/// Build a fresh [`SharedWorkstreamStore`] backed by a tempfile path (never
/// created ahead of time — `WorkstreamStore::load` treats a missing file as
/// a fresh, empty store).
async fn shared_store() -> (SharedWorkstreamStore, tempfile::TempDir) {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("workstreams-test.json");
    let store = WorkstreamStore::load(path).await.expect("load fresh store");
    (Arc::new(Mutex::new(store)), dir)
}

/// Create a workstream directly through the store and return its id.
async fn create(store: &SharedWorkstreamStore, name: &str) -> WorkstreamId {
    store.lock().await.create(name).await.expect("create")
}

/// Activating with no prior active workstream must succeed with `prior_id:
/// None` and persist the pointer.
#[tokio::test]
async fn activate_with_no_prior_active_succeeds() {
    let (store, _dir) = shared_store().await;
    let id = create(&store, "a").await;

    let outcome = activate(&store, id, false).await.expect("activate");
    assert_eq!(outcome.active_id, id);
    assert_eq!(outcome.prior_id, None);
    assert_eq!(
        store.lock().await.active_workstream_id().await.unwrap(),
        Some(id)
    );
}

/// Re-activating the ALREADY-active workstream (force: false) must be
/// idempotent — success, `prior_id: None`, never `ActiveConflict` (§6.1: the
/// conflict only fires for a DIFFERENT workstream).
#[tokio::test]
async fn activate_already_active_is_idempotent() {
    let (store, _dir) = shared_store().await;
    let id = create(&store, "a").await;
    activate(&store, id, false).await.expect("first activate");

    let outcome = activate(&store, id, false)
        .await
        .expect("re-activate must succeed");
    assert_eq!(outcome.active_id, id);
    assert_eq!(outcome.prior_id, None);
}

/// Activating a DIFFERENT workstream without `force` while one is already
/// active must fail with `ActiveConflict(active_id)`.
#[tokio::test]
async fn activate_without_force_returns_active_conflict() {
    let (store, _dir) = shared_store().await;
    let a = create(&store, "a").await;
    let b = create(&store, "b").await;
    activate(&store, a, false).await.expect("activate a");

    let err = activate(&store, b, false).await.expect_err("must conflict");
    match err {
        ActivationError::ActiveConflict(active) => assert_eq!(active, a),
        other => panic!("expected ActiveConflict, got {other:?}"),
    }
    // The conflicting activation must not have changed the pointer.
    assert_eq!(
        store.lock().await.active_workstream_id().await.unwrap(),
        Some(a)
    );
}

/// `force: true` must deactivate the prior active workstream, activate the
/// new one, and report both ids.
#[tokio::test]
async fn activate_with_force_switches_and_reports_prior() {
    let (store, _dir) = shared_store().await;
    let a = create(&store, "a").await;
    let b = create(&store, "b").await;
    activate(&store, a, false).await.expect("activate a");

    let outcome = activate(&store, b, true).await.expect("force switch");
    assert_eq!(outcome.active_id, b);
    assert_eq!(outcome.prior_id, Some(a));
    assert_eq!(
        store.lock().await.active_workstream_id().await.unwrap(),
        Some(b)
    );
}

/// Activating an id that names no existing workstream must fail with
/// `NotFound`, regardless of `force`.
#[tokio::test]
async fn activate_unknown_id_returns_not_found() {
    let (store, _dir) = shared_store().await;
    let unknown = WorkstreamId::new();

    let err = activate(&store, unknown, false)
        .await
        .expect_err("must not find id");
    assert!(matches!(err, ActivationError::NotFound(id) if id == unknown));
}

/// Deactivating the CURRENTLY active workstream must clear the pointer and
/// report the cleared id.
#[tokio::test]
async fn deactivate_active_clears_pointer() {
    let (store, _dir) = shared_store().await;
    let id = create(&store, "a").await;
    activate(&store, id, false).await.expect("activate");

    let cleared = deactivate(&store, id).await.expect("deactivate");
    assert_eq!(cleared, Some(id));
    assert_eq!(
        store.lock().await.active_workstream_id().await.unwrap(),
        None
    );
}

/// Deactivating a workstream that is NOT the active one (idle, or even
/// unknown) must be an idempotent no-op — success, `None`, no pointer
/// change (§4.3).
#[tokio::test]
async fn deactivate_non_active_is_idempotent_noop() {
    let (store, _dir) = shared_store().await;
    let a = create(&store, "a").await;
    let b = create(&store, "b").await;
    activate(&store, a, false).await.expect("activate a");

    // Deactivating the idle workstream `b` must be a no-op.
    let cleared = deactivate(&store, b).await.expect("deactivate idle");
    assert_eq!(cleared, None);
    assert_eq!(
        store.lock().await.active_workstream_id().await.unwrap(),
        Some(a)
    );

    // Deactivating an entirely unknown id must likewise be a no-op, not an
    // error (the caller cannot tell "idle" from "never existed" and the
    // spec does not ask it to).
    let unknown = WorkstreamId::new();
    let cleared = deactivate(&store, unknown)
        .await
        .expect("deactivate unknown");
    assert_eq!(cleared, None);
}

/// The active pointer set by `activate` must survive a fresh
/// `WorkstreamStore::load` of the same file (AC-1.4/boot restoration) — the
/// activation layer must not hold anything back from disk.
#[tokio::test]
async fn activate_persists_across_store_reload() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("workstreams-test.json");
    let store = WorkstreamStore::load(&path)
        .await
        .expect("load fresh store");
    let shared = Arc::new(Mutex::new(store));
    let id = create(&shared, "a").await;
    activate(&shared, id, false).await.expect("activate");

    let reloaded = WorkstreamStore::load(&path).await.expect("reload");
    let mut reloaded = reloaded;
    assert_eq!(
        reloaded.active_workstream_id().await.unwrap(),
        Some(id),
        "active pointer must persist across a fresh load"
    );
}

/// (issue #3297, DOC-48 AC-3.3) Activating with no prior active workstream
/// must publish `WorkstreamActivationChanged{new_active_id: Some(id),
/// prior_id: None}`.
#[tokio::test]
async fn activate_with_no_prior_active_publishes_activation_changed() {
    let (store, _dir) = shared_store().await;
    let id = create(&store, "a").await;
    let mut rx = crate::events::subscribe();

    activate(&store, id, false).await.expect("activate");

    match recv_event(&mut rx).await {
        Event::WorkstreamActivationChanged {
            new_active_id,
            prior_id,
        } => {
            assert_eq!(new_active_id, Some(id.to_string()));
            assert_eq!(prior_id, None);
        }
        other => panic!("expected WorkstreamActivationChanged, got {other:?}"),
    }
}

/// A force-switch must publish `WorkstreamActivationChanged` naming BOTH the
/// new active id and the prior one.
#[tokio::test]
async fn activate_with_force_publishes_activation_changed_with_prior() {
    let (store, _dir) = shared_store().await;
    let a = create(&store, "a").await;
    let b = create(&store, "b").await;
    activate(&store, a, false).await.expect("activate a");

    let mut rx = crate::events::subscribe();
    activate(&store, b, true).await.expect("force switch");

    match recv_event(&mut rx).await {
        Event::WorkstreamActivationChanged {
            new_active_id,
            prior_id,
        } => {
            assert_eq!(new_active_id, Some(b.to_string()));
            assert_eq!(prior_id, Some(a.to_string()));
        }
        other => panic!("expected WorkstreamActivationChanged, got {other:?}"),
    }
}

/// Re-activating the already-active workstream is a true no-op (§6.1) — it
/// must NOT publish an activation-changed event.
#[tokio::test]
async fn activate_already_active_does_not_publish() {
    let (store, _dir) = shared_store().await;
    let id = create(&store, "a").await;
    activate(&store, id, false).await.expect("first activate");

    let mut rx = crate::events::subscribe();
    activate(&store, id, false)
        .await
        .expect("idempotent re-activate");

    assert_no_event(&mut rx).await;
}

/// Deactivating the active workstream must publish `WorkstreamActivationChanged{new_active_id: None, prior_id: Some(id)}`.
#[tokio::test]
async fn deactivate_active_publishes_activation_changed() {
    let (store, _dir) = shared_store().await;
    let id = create(&store, "a").await;
    activate(&store, id, false).await.expect("activate");

    let mut rx = crate::events::subscribe();
    deactivate(&store, id).await.expect("deactivate");

    match recv_event(&mut rx).await {
        Event::WorkstreamActivationChanged {
            new_active_id,
            prior_id,
        } => {
            assert_eq!(new_active_id, None);
            assert_eq!(prior_id, Some(id.to_string()));
        }
        other => panic!("expected WorkstreamActivationChanged, got {other:?}"),
    }
}

/// Deactivating a non-active (idle or unknown) workstream is a no-op (§4.3)
/// — it must NOT publish an activation-changed event.
#[tokio::test]
async fn deactivate_non_active_does_not_publish() {
    let (store, _dir) = shared_store().await;
    let a = create(&store, "a").await;
    let b = create(&store, "b").await;
    activate(&store, a, false).await.expect("activate a");

    let mut rx = crate::events::subscribe();
    deactivate(&store, b).await.expect("deactivate idle b");

    assert_no_event(&mut rx).await;
}

/// (issue #3297) A fresh activation must ALSO publish
/// `WorkstreamStateInferred{workstream_id: id, state: "active"}` right after
/// `WorkstreamActivationChanged`.
#[tokio::test]
async fn activate_with_no_prior_active_publishes_state_inferred() {
    let (store, _dir) = shared_store().await;
    let id = create(&store, "a").await;
    let mut rx = crate::events::subscribe();

    activate(&store, id, false).await.expect("activate");

    let _ = recv_event(&mut rx).await; // WorkstreamActivationChanged, covered above
    match recv_event(&mut rx).await {
        Event::WorkstreamStateInferred {
            workstream_id,
            state,
            ..
        } => {
            assert_eq!(workstream_id, id.to_string());
            assert_eq!(state, "active");
        }
        other => panic!("expected WorkstreamStateInferred, got {other:?}"),
    }
}

/// A force-switch must publish `WorkstreamStateInferred` for BOTH the
/// newly-active workstream (`"active"`) and the prior one (`"idle"`).
#[tokio::test]
async fn activate_with_force_publishes_state_inferred_for_both() {
    let (store, _dir) = shared_store().await;
    let a = create(&store, "a").await;
    let b = create(&store, "b").await;
    activate(&store, a, false).await.expect("activate a");

    let mut rx = crate::events::subscribe();
    activate(&store, b, true).await.expect("force switch");

    let _ = recv_event(&mut rx).await; // WorkstreamActivationChanged
    match recv_event(&mut rx).await {
        Event::WorkstreamStateInferred {
            workstream_id,
            state,
            ..
        } => {
            assert_eq!(workstream_id, b.to_string());
            assert_eq!(state, "active");
        }
        other => panic!("expected WorkstreamStateInferred for b, got {other:?}"),
    }
    match recv_event(&mut rx).await {
        Event::WorkstreamStateInferred {
            workstream_id,
            state,
            ..
        } => {
            assert_eq!(workstream_id, a.to_string());
            assert_eq!(state, "idle");
        }
        other => panic!("expected WorkstreamStateInferred for a, got {other:?}"),
    }
}

/// Deactivating the active workstream must ALSO publish
/// `WorkstreamStateInferred{state: "idle"}` right after
/// `WorkstreamActivationChanged`.
#[tokio::test]
async fn deactivate_active_publishes_state_inferred_idle() {
    let (store, _dir) = shared_store().await;
    let id = create(&store, "a").await;
    activate(&store, id, false).await.expect("activate");

    let mut rx = crate::events::subscribe();
    deactivate(&store, id).await.expect("deactivate");

    let _ = recv_event(&mut rx).await; // WorkstreamActivationChanged
    match recv_event(&mut rx).await {
        Event::WorkstreamStateInferred {
            workstream_id,
            state,
            ..
        } => {
            assert_eq!(workstream_id, id.to_string());
            assert_eq!(state, "idle");
        }
        other => panic!("expected WorkstreamStateInferred, got {other:?}"),
    }
}
