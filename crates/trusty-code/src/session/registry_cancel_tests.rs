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

/// A run that outlives the grace must ERROR, not report a stop, and must leave
/// its handle behind (#8207, fail-open check).
///
/// Why: a timeout downgraded to success is the original bug with extra
/// latency. The restore matters just as much — a dropped `JoinHandle` detaches
/// its task, so graceful shutdown would have nothing left to await. The first
/// cut of this test asserted nothing about the restore (deleting it left the
/// test green), so the second confirm below is the assertion: it can only
/// reclaim a handle that is actually back in the slot, and answers with the
/// "did not stop" message rather than "not yet joinable" when it is.
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

    assert_eq!(err.code, -32010, "unexpected error shape: {err:?}");
    assert!(
        registry.is_executing(&id),
        "the still-running execution must stay tracked"
    );
    let again = registry
        .await_cancelled(&id, Duration::from_millis(30))
        .await
        .expect_err("the run is still going");
    assert!(
        again.message.contains("did not stop within"),
        "the handle must have been restored, so a second confirm joins it \
         again rather than finding nothing to await; got {again:?}"
    );
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

    assert_eq!(err.code, -32010, "unexpected error shape: {err:?}");
    assert!(
        err.message.contains("not yet joinable"),
        "this arm must name the window it is in, not a peer cancel: {err:?}"
    );
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

// ── #8207 code-critic round: the three fail-open arms ─────────────────────────

/// Spawn a stand-in for `task::executor::run_and_record`: a run that watches
/// the cooperative-cancel flag and, like the real one, clears its own execution
/// slot as its last act.
///
/// Why: the wait is a claim about a REAL spawned task's lifetime, so a double
/// that only flips registry fields would prove nothing.
fn spawn_flag_watching_run(registry: &Arc<SessionRegistry>, id: &str, flag: Arc<AtomicBool>) {
    let registry_for_task = Arc::clone(registry);
    let id_for_task = id.to_string();
    let handle = tokio::spawn(async move {
        while !flag.load(Ordering::Relaxed) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        registry_for_task.finish_execution(&id_for_task);
    });
    registry.attach_execution_handle(id, handle);
}

/// A run that PANICKED must release its slot AND say so over the wire (#8207,
/// fail-open check).
///
/// Why: two fail-open arms, one after the other. A panicked task joins
/// instantly, so `timeout(..).await.is_err()` — the first cut's only test — was
/// false and the cancel answered `Ok`; `run_and_record` never reached
/// `finish_execution`, so the slot stayed occupied and every later prompt was
/// rejected with `-32003`. Releasing the slot fixed that but landed `Cancelled`
/// with no result, and `Cancelled` is resumable — so a crashed run became
/// indistinguishable from a clean Esc and the next prompt resumed it. The status
/// and the `TaskResult` are the only two things a client reads off
/// `session.status`, so those are what this asserts.
/// What: spawn a task that panics, confirm the cancel, then read the session the
/// way the RPC surface does.
/// Test: this test.
#[tokio::test]
async fn a_panicked_run_lands_failed_and_releases_its_slot() {
    let registry = Arc::new(SessionRegistry::new());
    let (id, _flag) = executing_session(&registry);

    let handle = tokio::spawn(async {
        panic!("the run died mid-flight");
    });
    registry.attach_execution_handle(&id, handle);
    registry.request_cancel(&id).expect("session exists");

    registry
        .await_cancelled(&id, Duration::from_secs(5))
        .await
        .expect("a panicked run is a stopped run");

    assert!(
        !registry.is_executing(&id),
        "the dead task's execution slot must not stay held"
    );

    let snapshot = registry.status(&id).expect("session exists");
    assert_eq!(
        snapshot.status,
        SessionStatus::Failed,
        "a crashed run must not land in the same status a clean cancel does"
    );
    let result = snapshot
        .result
        .expect("a crashed run must record a result, not only a log line");
    assert_eq!(
        result.status,
        crate::session::task_result::TaskResultStatus::Failed
    );
    assert!(
        result
            .summary
            .as_deref()
            .is_some_and(|s| s.contains("did not exit cleanly")),
        "the result must say WHY the run ended: {result:?}"
    );
    assert_eq!(
        registry.begin_execution(&id).unwrap_err().code,
        -32003,
        "a crashed session must not resume as if it had been cleanly cancelled"
    );
}

/// A dead handle joining LATE must not fail a newer run that already holds the
/// slot (#8207, code-critic round 3).
///
/// Why: landing `Failed` for a crashed run is only safe while that run is still
/// the session's current one. The interleave is the same one
/// `the_grace_timeout_never_clobbers_a_newer_runs_handle` drives, one arm over:
/// the old run clears its own slot, a new `task.run` is accepted, and only THEN
/// does the old task's panic reach the waiting cancel. Failing the session there
/// would kill a live run for its predecessor's crash.
/// What: drives that order explicitly, then asserts the session is neither
/// terminal nor resultful and the new run is still confirmable on its own.
/// Test: this test.
#[tokio::test]
async fn a_dead_handle_never_fails_a_newer_run() {
    let registry = Arc::new(SessionRegistry::new());
    let (id, _old_flag) = executing_session(&registry);

    // The old run releases its slot on cue, then panics a beat later — long
    // enough for the new run below to take the slot first.
    let release = Arc::new(tokio::sync::Notify::new());
    let old_handle = {
        let registry = Arc::clone(&registry);
        let id = id.clone();
        let release = Arc::clone(&release);
        tokio::spawn(async move {
            release.notified().await;
            registry.finish_execution(&id);
            tokio::time::sleep(Duration::from_millis(200)).await;
            panic!("the old run died after releasing its slot");
        })
    };
    registry.attach_execution_handle(&id, old_handle);
    registry.request_cancel(&id).expect("session exists");

    let confirming = {
        let registry = Arc::clone(&registry);
        let id = id.clone();
        tokio::spawn(async move { registry.await_cancelled(&id, Duration::from_secs(5)).await })
    };

    // Let the cancel claim the old handle, then run the interleave.
    tokio::time::sleep(Duration::from_millis(20)).await;
    release.notify_one();
    tokio::time::sleep(Duration::from_millis(20)).await;
    let new_flag = registry
        .begin_execution(&id)
        .expect("the old run released its slot");
    spawn_flag_watching_run(&registry, &id, Arc::clone(&new_flag));

    confirming
        .await
        .expect("the confirming task must not panic")
        .expect("a panicked run is still a stopped run");

    let snapshot = registry.status(&id).expect("session exists");
    assert!(
        !snapshot.status.is_terminal(),
        "the newer run must not be ended by its predecessor's crash, got {:?}",
        snapshot.status
    );
    assert!(
        snapshot.result.is_none(),
        "the newer run has not finished, so it owns no result yet: {:?}",
        snapshot.result
    );
    assert!(
        registry.is_executing(&id),
        "the newer run must still be tracked"
    );

    registry.request_cancel(&id).expect("session exists");
    registry
        .await_cancelled(&id, Duration::from_secs(5))
        .await
        .expect("the newer run confirms on its own handle");
}

/// The grace timeout must never restore a dead handle over a NEWER run's live
/// one (#8207, fail-open check).
///
/// Why: the restore was unconditional, and the race it loses is reachable —
/// the old run clears its own slot just before the grace ends, a new `task.run`
/// is accepted and attaches its live handle, and the restore then overwrites
/// that handle with the finished one. The new run becomes invisible to
/// `shutdown_executions`, and a later cancel joins the dead handle and reports
/// a stop while the new run is still going.
/// What: drives exactly that interleave, then cancels the NEW run — which can
/// only be confirmed if its own handle survived.
/// Test: this test.
#[tokio::test]
async fn the_grace_timeout_never_clobbers_a_newer_runs_handle() {
    let registry = Arc::new(SessionRegistry::new());
    let (id, _old_flag) = executing_session(&registry);

    // The old run releases its slot on cue and then never returns, so the
    // confirming cancel below still reaches its timeout path.
    let release = Arc::new(tokio::sync::Notify::new());
    let old_handle = {
        let registry = Arc::clone(&registry);
        let id = id.clone();
        let release = Arc::clone(&release);
        tokio::spawn(async move {
            release.notified().await;
            registry.finish_execution(&id);
            std::future::pending::<()>().await
        })
    };
    registry.attach_execution_handle(&id, old_handle);
    registry.request_cancel(&id).expect("session exists");

    let confirming = {
        let registry = Arc::clone(&registry);
        let id = id.clone();
        tokio::spawn(async move {
            registry
                .await_cancelled(&id, Duration::from_millis(200))
                .await
        })
    };

    // Let the cancel claim the old handle, then run the interleave.
    tokio::time::sleep(Duration::from_millis(20)).await;
    release.notify_one();
    tokio::time::sleep(Duration::from_millis(20)).await;
    let new_flag = registry
        .begin_execution(&id)
        .expect("the old run released its slot");
    spawn_flag_watching_run(&registry, &id, Arc::clone(&new_flag));

    confirming
        .await
        .expect("the confirming task must not panic")
        .expect_err("the old run never stopped, so its cancel must fail closed");

    registry.request_cancel(&id).expect("session exists");
    registry
        .await_cancelled(&id, Duration::from_secs(5))
        .await
        .expect("the newer run's own handle must still be the tracked one");
    assert!(
        !registry.is_executing(&id),
        "the newer run must have released its slot"
    );
}

/// Two concurrent cancels on one executing session must BOTH end with the truth
/// about that run (#8207, M-1).
///
/// Why: the second one found the handle already taken and answered "tracked but
/// not yet joinable" — a diagnosis of a completely different state. Double-Esc
/// in the TUI and a second `POST /sessions/{id}/cancel` from the GUI both reach
/// it, so the wrong answer is the common case, not the exotic one.
/// What: two cancels in flight over one run that takes a moment to notice;
/// assert both return `Ok` and the slot is released.
/// Test: this test.
#[tokio::test]
async fn two_concurrent_cancels_both_learn_the_run_stopped() {
    let registry = Arc::new(SessionRegistry::new());
    let (id, flag) = executing_session(&registry);

    let handle = {
        let registry = Arc::clone(&registry);
        let id = id.clone();
        let flag = Arc::clone(&flag);
        tokio::spawn(async move {
            while !flag.load(Ordering::Relaxed) {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            tokio::time::sleep(Duration::from_millis(40)).await;
            registry.finish_execution(&id);
        })
    };
    registry.attach_execution_handle(&id, handle);
    registry.request_cancel(&id).expect("session exists");

    let spawn_cancel = || {
        let registry = Arc::clone(&registry);
        let id = id.clone();
        tokio::spawn(async move { registry.await_cancelled(&id, Duration::from_secs(5)).await })
    };
    let (first, second) = tokio::join!(spawn_cancel(), spawn_cancel());

    assert!(
        first.expect("no panic").is_ok(),
        "the owning cancel must confirm the stop"
    );
    assert!(
        second.expect("no panic").is_ok(),
        "the concurrent cancel must learn the run stopped, not report a fault"
    );
    assert!(!registry.is_executing(&id), "the slot must be released");
}

/// A concurrent cancel whose peer never confirms must fail CLOSED, with the
/// same domain code as any other unconfirmed cancel (#8207, M-1).
///
/// Why: waiting on a peer must not become a second way to report a comfortable
/// lie. It also has to be distinguishable from `-32603` so the client can stay
/// in a cancelling state instead of showing a daemon fault.
/// Test: this test.
#[tokio::test]
async fn a_concurrent_cancel_that_never_confirms_fails_closed() {
    let registry = Arc::new(SessionRegistry::new());
    let (id, _flag) = executing_session(&registry);

    let handle = tokio::spawn(async { std::future::pending::<()>().await });
    registry.attach_execution_handle(&id, handle);
    registry.request_cancel(&id).expect("session exists");

    let owner = {
        let registry = Arc::clone(&registry);
        let id = id.clone();
        tokio::spawn(async move { registry.await_cancelled(&id, Duration::from_secs(5)).await })
    };
    tokio::time::sleep(Duration::from_millis(20)).await;

    let err = registry
        .await_cancelled(&id, Duration::from_millis(40))
        .await
        .expect_err("a run that never stopped must not report a stop");

    assert_eq!(err.code, -32010, "unexpected error shape: {err:?}");
    assert!(
        err.message.contains("another cancel"),
        "the message must name the state it is really in: {err:?}"
    );
    owner.abort();
}
