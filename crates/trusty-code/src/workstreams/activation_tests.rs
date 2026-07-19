//! Tests for `activation::activate`/`activation::deactivate` (DOC-48 §6,
//! issue #3294; the `Event::WorkstreamActivationChanged`/
//! `WorkstreamStateInferred` publication tests are issue #3297).

use std::time::Duration;

use super::*;
use tempfile::tempdir;

/// Read from the process-global event bus until a `WorkstreamActivationChanged`
/// naming `id` (as either `new_active_id` or `prior_id`) arrives, ignoring
/// anything else.
///
/// Why: `crate::events::bus()` is a single process-wide singleton shared by
/// every test in this binary — `cargo test` runs tests concurrently by
/// default, so a raw `rx.recv().await` can observe another test's unrelated
/// envelope interleaved on the same subscription (confirmed by PR #3343 CI:
/// a foreign workstream's `WorkstreamActivationChanged` leaked into this
/// file's tests under parallel execution). Unlike
/// `session::registry_tests::next_event_for` (which filters on the
/// envelope's OWN `session_id` field), these events are daemon-scoped — the
/// envelope's `session_id` is always the empty string (see
/// `Event::WorkstreamActivationChanged`'s docs) — so the filter reads into
/// the EVENT's own id fields instead. Bounded by a 2s timeout so a genuine
/// bug (event never published) still fails fast instead of hanging.
async fn next_activation_changed_for(
    rx: &mut tokio::sync::broadcast::Receiver<SessionEventEnvelope>,
    id: WorkstreamId,
) -> (Option<String>, Option<String>) {
    let id = id.to_string();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let envelope = rx.recv().await.expect("event bus channel closed");
            if let Event::WorkstreamActivationChanged {
                new_active_id,
                prior_id,
            } = envelope.event
                && (new_active_id.as_deref() == Some(id.as_str())
                    || prior_id.as_deref() == Some(id.as_str()))
            {
                return (new_active_id, prior_id);
            }
        }
    })
    .await
    .expect("timed out waiting for WorkstreamActivationChanged naming this workstream")
}

/// Read from the process-global event bus until a `WorkstreamStateInferred`
/// naming `id` arrives, ignoring anything else — see
/// [`next_activation_changed_for`]'s docs for the shared-bus-interleaving
/// rationale.
async fn next_state_inferred_for(
    rx: &mut tokio::sync::broadcast::Receiver<SessionEventEnvelope>,
    id: WorkstreamId,
) -> (String, String) {
    let id = id.to_string();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let envelope = rx.recv().await.expect("event bus channel closed");
            if let Event::WorkstreamStateInferred {
                workstream_id,
                state,
                reason,
            } = envelope.event
                && workstream_id == id
            {
                return (state, reason);
            }
        }
    })
    .await
    .expect("timed out waiting for WorkstreamStateInferred naming this workstream")
}

/// Assert that NO `WorkstreamActivationChanged`/`WorkstreamStateInferred`
/// naming `id` arrives within a short window — used to prove the idempotent
/// no-op branches of `activate`/`deactivate` do not publish. Drains (and
/// discards) any unrelated envelope from another concurrently-running test
/// rather than treating one as evidence of publication (see
/// [`next_activation_changed_for`]'s docs).
async fn assert_no_event_for(
    rx: &mut tokio::sync::broadcast::Receiver<SessionEventEnvelope>,
    id: WorkstreamId,
) {
    let id = id.to_string();
    let result = tokio::time::timeout(Duration::from_millis(300), async {
        loop {
            let envelope = rx.recv().await.expect("event bus channel closed");
            let matches = match &envelope.event {
                Event::WorkstreamActivationChanged {
                    new_active_id,
                    prior_id,
                } => {
                    new_active_id.as_deref() == Some(id.as_str())
                        || prior_id.as_deref() == Some(id.as_str())
                }
                Event::WorkstreamStateInferred { workstream_id, .. } => workstream_id == &id,
                _ => false,
            };
            if matches {
                return;
            }
            // Unrelated envelope from another concurrently-running test —
            // ignore and keep draining until the timeout below fires.
        }
    })
    .await;
    assert!(
        result.is_err(),
        "expected no event naming workstream {id}, but got one"
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

    let (new_active_id, prior_id) = next_activation_changed_for(&mut rx, id).await;
    assert_eq!(new_active_id, Some(id.to_string()));
    assert_eq!(prior_id, None);
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

    let (new_active_id, prior_id) = next_activation_changed_for(&mut rx, b).await;
    assert_eq!(new_active_id, Some(b.to_string()));
    assert_eq!(prior_id, Some(a.to_string()));
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

    assert_no_event_for(&mut rx, id).await;
}

/// Deactivating the active workstream must publish `WorkstreamActivationChanged{new_active_id: None, prior_id: Some(id)}`.
#[tokio::test]
async fn deactivate_active_publishes_activation_changed() {
    let (store, _dir) = shared_store().await;
    let id = create(&store, "a").await;
    activate(&store, id, false).await.expect("activate");

    let mut rx = crate::events::subscribe();
    deactivate(&store, id).await.expect("deactivate");

    let (new_active_id, prior_id) = next_activation_changed_for(&mut rx, id).await;
    assert_eq!(new_active_id, None);
    assert_eq!(prior_id, Some(id.to_string()));
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

    assert_no_event_for(&mut rx, b).await;
}

/// (issue #3297) A fresh activation must ALSO publish
/// `WorkstreamStateInferred{workstream_id: id, state: "active"}`.
#[tokio::test]
async fn activate_with_no_prior_active_publishes_state_inferred() {
    let (store, _dir) = shared_store().await;
    let id = create(&store, "a").await;
    let mut rx = crate::events::subscribe();

    activate(&store, id, false).await.expect("activate");

    let (state, _reason) = next_state_inferred_for(&mut rx, id).await;
    assert_eq!(state, "active");
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

    let (b_state, _) = next_state_inferred_for(&mut rx, b).await;
    assert_eq!(b_state, "active");
    let (a_state, _) = next_state_inferred_for(&mut rx, a).await;
    assert_eq!(a_state, "idle");
}

/// Deactivating the active workstream must ALSO publish
/// `WorkstreamStateInferred{state: "idle"}`.
#[tokio::test]
async fn deactivate_active_publishes_state_inferred_idle() {
    let (store, _dir) = shared_store().await;
    let id = create(&store, "a").await;
    activate(&store, id, false).await.expect("activate");

    let mut rx = crate::events::subscribe();
    deactivate(&store, id).await.expect("deactivate");

    let (state, _reason) = next_state_inferred_for(&mut rx, id).await;
    assert_eq!(state, "idle");
}
