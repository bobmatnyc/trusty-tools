//! Tests for `SessionRegistry::await_cancelled` (#8207). A `_tests.rs` sibling
//! of `registry_cancel.rs`, per the same convention `registry_tests.rs`
//! follows — see that file's header.

use super::*;
use crate::binding::ProjectBinding;
use std::sync::atomic::{AtomicBool, Ordering};

/// Create a session and reserve its execution slot, returning both the session
/// id and the shared cancel flag the "run" below watches.
fn executing_session(registry: &SessionRegistry) -> (String, Arc<AtomicBool>) {
    let session = registry.create("t".to_string(), None, ProjectBinding::None);
    let flag = registry
        .begin_execution(&session.id)
        .expect("fresh session accepts an execution");
    (session.id, flag)
}

/// `await_cancelled` must return only after the spawned run has actually
/// finished — the property `session.cancel` reports to the client (#8207).
///
/// Why: the pre-#8207 path returned as soon as the cooperative flag was set,
/// so the client was told a task had stopped while it was still running. This
/// asserts on the run's OWN completion marker, not on the registry's status
/// field, because only the former distinguishes "signalled" from "stopped".
/// Test: this test.
#[tokio::test]
async fn await_cancelled_returns_once_the_task_has_stopped() {
    let registry = Arc::new(SessionRegistry::new());
    let (id, flag) = executing_session(&registry);
    let stopped = Arc::new(AtomicBool::new(false));

    let registry_for_task = Arc::clone(&registry);
    let id_for_task = id.clone();
    let stopped_for_task = Arc::clone(&stopped);
    let handle = tokio::spawn(async move {
        while !flag.load(Ordering::Relaxed) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        // The real executor takes a while to unwind after observing the flag;
        // this stands in for that window.
        tokio::time::sleep(Duration::from_millis(40)).await;
        stopped_for_task.store(true, Ordering::Relaxed);
        registry_for_task.finish_execution(&id_for_task);
    });
    registry.attach_execution_handle(&id, handle);

    registry.request_cancel(&id).expect("session exists");
    registry
        .await_cancelled(&id, Duration::from_secs(5))
        .await
        .expect("the task stops well inside the grace");

    assert!(
        stopped.load(Ordering::Relaxed),
        "await_cancelled returned before the task had stopped"
    );
    assert!(
        !registry.is_executing(&id),
        "the stopped run must have released its execution slot"
    );
}

/// A run that outlives the grace must ERROR, not report a stop (#8207,
/// fail-open check).
///
/// Why: a timeout downgraded to success is the original bug with extra
/// latency. This also pins the handle being put BACK, so graceful shutdown
/// still has something to await.
/// Test: this test.
#[tokio::test]
async fn await_cancelled_errors_when_the_task_outlives_the_grace() {
    let registry = Arc::new(SessionRegistry::new());
    let (id, _flag) = executing_session(&registry);

    let handle = tokio::spawn(async {
        tokio::time::sleep(Duration::from_secs(30)).await;
    });
    registry.attach_execution_handle(&id, handle);

    registry.request_cancel(&id).expect("session exists");
    let err = registry
        .await_cancelled(&id, Duration::from_millis(30))
        .await
        .expect_err("a task that did not stop must not report a stop");

    assert_eq!(err.code, -32603, "unexpected error shape: {err:?}");
    assert!(
        registry.is_executing(&id),
        "the still-running execution must stay tracked"
    );
    // The handle must have been restored, or `shutdown_executions` would have
    // nothing left to await for this session.
    registry
        .shutdown_executions(Duration::from_millis(10))
        .await;
}

/// A session with nothing in flight has nothing to wait for (#8207).
///
/// Why: `session.cancel` reaches this when the run finished between the
/// `is_executing` check and the wait. That is a stop, not a failure.
/// Test: this test.
#[tokio::test]
async fn await_cancelled_is_ok_when_nothing_is_executing() {
    let registry = SessionRegistry::new();
    let session = registry.create("t".to_string(), None, ProjectBinding::None);

    registry
        .await_cancelled(&session.id, Duration::from_millis(10))
        .await
        .expect("an idle session is already stopped");
}

/// An execution tracked with no `JoinHandle` yet cannot be confirmed, so it
/// must error (#8207, fail-open check).
///
/// Why: this is the window between `begin_execution` and
/// `attach_execution_handle`. There is nothing to await, so reporting a stop
/// would be a guess — and the guess is wrong exactly when the run is youngest.
/// Test: this test.
#[tokio::test]
async fn await_cancelled_errors_when_the_execution_has_no_handle() {
    let registry = SessionRegistry::new();
    let (id, _flag) = executing_session(&registry);

    let err = registry
        .await_cancelled(&id, Duration::from_millis(10))
        .await
        .expect_err("an unjoinable run must not report a stop");

    assert_eq!(err.code, -32603, "unexpected error shape: {err:?}");
}

/// An unknown session id errors the same way every other registry method does.
/// Test: this test.
#[tokio::test]
async fn await_cancelled_unknown_session_errors() {
    let registry = SessionRegistry::new();
    let err = registry
        .await_cancelled("nope", Duration::from_millis(10))
        .await
        .expect_err("unknown session");
    assert_eq!(err.code, -32007);
}
