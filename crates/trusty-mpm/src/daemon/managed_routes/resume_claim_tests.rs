//! The resume claim's SPAN across `resume_managed` (#8233 item 1).
//!
//! Why: r5 held the claim only for the record transition — `SessionManager::
//! resume` took it and dropped it on return, so the self-heal, the prompt
//! refresh, the pane handshake, the spawn and the post-send check all ran
//! unclaimed over a record already written `Active` with a bare shell behind
//! it. The reaper then stopped that record and the next supervisor tick
//! launched it a second time. The property is about a WINDOW, so every test
//! here observes the claim from inside it.
//! What: a tmux double that parks on a chosen call and reports the claim's
//! state at that instant, scripted with `tokio::sync` primitives — no sleep and
//! no wall-clock budget.
//! Test: this file.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use tokio::sync::{Notify, Semaphore};

use crate::daemon::state::DaemonState;
use crate::session_manager::{
    ManagedError, ManagedSessionState, ManagedTmuxDriver, SessionManager,
};

/// A tmux double that parks the caller on its Nth call and logs every call.
///
/// Why: `resume_managed`'s own progress is the clock. Parking inside a driver
/// call puts the test exactly at one point of the route with no timer at all.
/// What: `calls` names each method in order; the `park_at`th call (1-based)
/// fires `entered` and blocks on `release`. Blocking is `blocking_acquire` on a
/// std primitive because `ManagedTmuxDriver` is synchronous.
struct GatedTmux {
    calls: std::sync::Mutex<Vec<String>>,
    seen: AtomicUsize,
    park_at: usize,
    entered: Notify,
    release: Semaphore,
}

impl GatedTmux {
    fn new(park_at: usize) -> Arc<Self> {
        Arc::new(Self {
            calls: std::sync::Mutex::new(Vec::new()),
            seen: AtomicUsize::new(0),
            park_at,
            entered: Notify::new(),
            release: Semaphore::new(0),
        })
    }

    fn log(&self, what: &str) {
        self.calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(what.to_owned());
        let n = self.seen.fetch_add(1, Ordering::SeqCst) + 1;
        if n == self.park_at {
            self.entered.notify_one();
            // A synchronous trait method cannot await; the release is a permit
            // the test adds from its own task.
            while self.release.try_acquire().is_err() {
                std::thread::yield_now();
            }
        }
    }

    fn calls(&self) -> Vec<String> {
        self.calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

impl ManagedTmuxDriver for GatedTmux {
    fn create_session(&self, _n: &str, _w: &str) -> Result<(), ManagedError> {
        self.log("create_session");
        Ok(())
    }
    fn kill_session(&self, _n: &str) -> Result<(), ManagedError> {
        self.log("kill_session");
        Ok(())
    }
    fn send_line(&self, _n: &str, _t: &str) -> Result<(), ManagedError> {
        self.log("send_line");
        Ok(())
    }
    fn capture(&self, _n: &str, _l: usize) -> Result<String, ManagedError> {
        self.log("capture");
        Ok(String::new())
    }
    fn list_sessions(&self) -> Result<Vec<String>, ManagedError> {
        self.log("list_sessions");
        Ok(Vec::new())
    }
}

/// A hermetic daemon state on `driver`, plus one `Stopped` session.
async fn stopped_session(
    dir: &tempfile::TempDir,
    driver: Arc<GatedTmux>,
) -> (
    Arc<DaemonState>,
    Arc<SessionManager>,
    crate::session_manager::ManagedSessionId,
) {
    let state = Arc::new(
        DaemonState::with_root_isolated_managed_and_driver(dir.path().to_path_buf(), driver).await,
    );
    let mgr = state.session_manager().await;
    let ws = dir.path().join("ws");
    std::fs::create_dir_all(&ws).expect("workspace dir");
    let record = mgr
        .create("claim-span".into(), Some(ws.clone()), None, None, None, None)
        .await
        .expect("create");
    let id = record.id;
    mgr.set_workspace(&id, ws, ManagedSessionState::Active)
        .await
        .expect("set Active");
    mgr.stop(&id).await.expect("stop");
    (state, mgr, id)
}

/// #8233 item 1: the claim must still be held once the record is `Active` and
/// the route is working on the pane.
///
/// Why: this is the whole regression. On r5 the claim ended when the record
/// transition returned, so everything after it — including the spawn — ran with
/// the session advertised as resumable to every other writer.
/// What: parks `resume_managed` inside the driver call the route makes AFTER
/// the record reaches `Active`, then asserts, from that instant, that the
/// session is still claimed and that the reaper refuses to touch it.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_claim_is_still_held_once_the_record_is_active() {
    let dir = tempfile::tempdir().expect("tempdir");
    // Call 1 is `resume_inner`'s own `list_sessions` probe, which both the
    // pre-fix and post-fix shapes make inside a claim. Call 2 is the first one
    // the route makes AFTER the record transition returned — the instant r5
    // had already released the claim.
    let tmux = GatedTmux::new(2);
    let (state, mgr, id) = stopped_session(&dir, Arc::clone(&tmux)).await;

    let route = tokio::spawn({
        let state = Arc::clone(&state);
        async move { super::lifecycle::resume_managed(&state, &id).await }
    });
    tmux.entered.notified().await;

    assert_eq!(
        mgr.get(&id).await.expect("record").state,
        ManagedSessionState::Active,
        "the park must be past the record transition, or this proves nothing: {:?}",
        tmux.calls()
    );
    assert!(
        mgr.is_resume_in_flight(&id),
        "the claim must span the whole route, not end with the record transition \
         (#8233 item 1); calls so far: {:?}",
        tmux.calls()
    );
    // And the consequence that made it a P0: the reaper must decline.
    let reaped = mgr.mark_runtime_exited_stopped(&id).await;
    assert!(
        matches!(reaped, Err(ManagedError::ResumeInFlight(_))),
        "the reaper must not stop a session mid-resume — that is what handed the \
         next supervisor tick a second launch: {reaped:?}"
    );
    assert_eq!(
        mgr.get(&id).await.expect("record").state,
        ManagedSessionState::Active,
        "and it must not have changed the state either"
    );

    tmux.release.add_permits(1);
    let _ = route.await.expect("the route task joins");
    assert!(
        !mgr.is_resume_in_flight(&id),
        "and the claim is released when the route ends, on its failure arm too"
    );
}

/// #8233 item 1: a second operator resume arriving mid-route is refused as
/// "already resuming", not reported as a launch failure.
///
/// What: parks the first `resume_managed` and drives a second one into the same
/// session, asserting the typed conflict. Then releases the first and asserts a
/// LATER resume is admitted — a claim that outlived its route would make the
/// session permanently unresumable.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_second_operator_resume_is_refused_while_one_is_in_flight() {
    let dir = tempfile::tempdir().expect("tempdir");
    let tmux = GatedTmux::new(2);
    let (state, _mgr, id) = stopped_session(&dir, Arc::clone(&tmux)).await;

    let route = tokio::spawn({
        let state = Arc::clone(&state);
        async move { super::lifecycle::resume_managed(&state, &id).await }
    });
    tmux.entered.notified().await;

    let second = super::lifecycle::resume_managed(&state, &id).await;
    let err = second.expect_err("a second operator resume must be refused");
    assert!(
        matches!(err, super::ResumeManagedError::AlreadyResuming(ref s) if s == &id.to_string()),
        "a refused claim must read as 'already resuming', not as a launch failure: {err}"
    );

    tmux.release.add_permits(1);
    let _ = route.await.expect("the route task joins");

    // The record is `Errored` or `Active` depending on how far the hermetic
    // adapter got; either way the claim is gone, so this is admitted past it.
    let later = super::lifecycle::resume_managed(&state, &id).await;
    assert!(
        !matches!(later, Err(super::ResumeManagedError::AlreadyResuming(_))),
        "a completed resume must leave the session resumable again: {later:?}"
    );
}
