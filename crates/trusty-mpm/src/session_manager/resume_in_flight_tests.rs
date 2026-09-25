//! Tests for the in-flight resume guard (#8233 acceptance item 4).
//!
//! Why: the guard's whole value is what it refuses, and its whole risk is a
//! claim that is never released — a stuck id makes a session permanently
//! unresumable. Both halves are error arms, so both are pinned here.
//! What: a GATED relauncher parks inside `resume_auto` until the test releases
//! it, which puts a second resume exactly in the window the live race hit. The
//! interleaving is scripted with `tokio::sync` primitives, never a sleep or a
//! wall-clock budget, so the test is deterministic under any host load.
//! Test: this file.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use tokio::sync::{Notify, Semaphore};

use crate::session_manager::manager::ManagedError;
use crate::session_manager::record::ManagedSessionId;
use crate::session_manager::relaunch::RuntimeRelauncher;
use crate::session_manager::{
    FakeNoopTmuxDriver, ManagedSessionState, SessionManager, SessionRecord,
};

/// A relauncher that parks on entry until released, counting its spawns.
///
/// `entered` fires as the relaunch begins; `release` unparks it. A test can
/// therefore hold a resume open for as long as it needs with no timing
/// assumption at all.
struct GatedRelauncher {
    spawns: AtomicUsize,
    entered: Notify,
    release: Semaphore,
    verdict: Result<(), String>,
}

impl GatedRelauncher {
    fn new(verdict: Result<(), String>) -> Arc<Self> {
        Arc::new(Self {
            spawns: AtomicUsize::new(0),
            entered: Notify::new(),
            release: Semaphore::new(0),
            verdict,
        })
    }

    /// Let one parked relaunch (or the next one to arrive) proceed.
    fn release_one(&self) {
        self.release.add_permits(1);
    }
}

#[async_trait::async_trait]
impl RuntimeRelauncher for GatedRelauncher {
    async fn relaunch(&self, _record: &SessionRecord) -> Result<(), String> {
        self.spawns.fetch_add(1, Ordering::SeqCst);
        self.entered.notify_one();
        // `forget` because the permit is a one-shot release token, not a
        // resource to hand back.
        match self.release.acquire().await {
            Ok(p) => p.forget(),
            Err(_) => return Err("gate closed".to_owned()),
        }
        self.verdict.clone()
    }
}

/// A hermetic manager plus one `Stopped`, auto-resumable session.
async fn stopped_session(
    dir: &tempfile::TempDir,
    name: &str,
) -> (Arc<SessionManager>, ManagedSessionId) {
    let workdir = dir.path().to_path_buf();
    let mgr = Arc::new(
        SessionManager::new(dir.path(), Arc::new(FakeNoopTmuxDriver))
            .await
            .expect("session manager"),
    );
    let id = seed_session(&mgr, workdir, name).await;
    (mgr, id)
}

/// Add a second `Stopped` session to an existing manager.
async fn seed_session(
    mgr: &Arc<SessionManager>,
    workdir: std::path::PathBuf,
    name: &str,
) -> ManagedSessionId {
    let record = mgr
        .create(name.into(), Some(workdir.clone()), None, None, None, None)
        .await
        .expect("create");
    let id = record.id;
    mgr.set_workspace(&id, workdir, ManagedSessionState::Active)
        .await
        .expect("set Active");
    mgr.mark_runtime_exited_stopped(&id)
        .await
        .expect("mark stopped");
    id
}

/// FAIL-OPEN CHECK (#8233 item 4): the error arm the guard exists to produce.
/// Before it, the second caller re-entered the whole resume — a second launch
/// line into the same pane and an `[error: …]` appended to the record the first
/// caller was about to mark `Active`.
#[tokio::test]
async fn a_second_concurrent_resume_of_one_session_is_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (mgr, id) = stopped_session(&dir, "inflight-refuse").await;
    let gate = GatedRelauncher::new(Ok(()));
    assert!(mgr.install_relauncher(gate.clone()));

    let first = tokio::spawn({
        let mgr = Arc::clone(&mgr);
        async move { mgr.resume_auto(&id).await }
    });
    // Deterministic: the second attempt starts only once the first is proven
    // to be inside the relaunch, holding the guard.
    gate.entered.notified().await;

    let err = mgr
        .resume_auto(&id)
        .await
        .expect_err("a second resume of an in-flight session must be refused");
    assert!(
        matches!(err, ManagedError::ResumeInFlight(ref s) if s == &id.to_string()),
        "the refusal must be typed and name the session, not a stringly-typed failure: {err}"
    );

    gate.release_one();
    first
        .await
        .expect("the first resume task joins")
        .expect("the first resume succeeds");

    assert_eq!(
        gate.spawns.load(Ordering::SeqCst),
        1,
        "the refused attempt must deliver no second launch into the pane"
    );
    assert_eq!(
        mgr.get(&id).await.expect("record").state,
        ManagedSessionState::Active,
        "the winner's outcome stands; the refusal changed no state"
    );
}

/// FAIL-OPEN CHECK (#8233 item 4): a guard that survived a failing resume would
/// make the session permanently unresumable — worse than the race it prevents.
#[tokio::test]
async fn a_failed_resume_releases_the_guard() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (mgr, id) = stopped_session(&dir, "inflight-fail").await;
    let gate = GatedRelauncher::new(Err("no runtime came up in pane 'tm-x'".to_owned()));
    assert!(mgr.install_relauncher(gate.clone()));

    gate.release_one();
    let err = mgr
        .resume_auto(&id)
        .await
        .expect_err("the scripted relaunch fails");
    assert!(err.to_string().contains("did not take"), "{err}");

    // The record is now `Errored`, which `resume_inner` accepts — so anything
    // refusing here is the guard, not the state machine.
    gate.release_one();
    let second = mgr.resume_auto(&id).await;
    assert!(
        !matches!(second, Err(ManagedError::ResumeInFlight(_))),
        "the error path must release the guard: {second:?}"
    );
}

/// FAIL-OPEN CHECK (#8233 item 4): the poller's future is cancellable, so a
/// tick dropped mid-resume must not strand the claim. `Drop` is what covers
/// this, and nothing else could.
#[tokio::test]
async fn a_dropped_resume_future_releases_the_guard() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (mgr, id) = stopped_session(&dir, "inflight-drop").await;
    let gate = GatedRelauncher::new(Ok(()));
    assert!(mgr.install_relauncher(gate.clone()));

    let doomed = tokio::spawn({
        let mgr = Arc::clone(&mgr);
        async move { mgr.resume_auto(&id).await }
    });
    gate.entered.notified().await;
    doomed.abort();
    // Joining the aborted task is what proves the future was dropped — and
    // therefore that the guard's `Drop` has already run — with no timer.
    assert!(
        doomed
            .await
            .expect_err("the aborted task reports")
            .is_cancelled(),
        "the first resume must be cancelled mid-flight, not finished"
    );

    gate.release_one();
    let second = mgr.resume_auto(&id).await;
    assert!(
        !matches!(second, Err(ManagedError::ResumeInFlight(_))),
        "a cancelled resume must not leave the session permanently unresumable: {second:?}"
    );
}

/// #8233 item 1: the runtime reaper must not stop a session another path is
/// resuming.
///
/// Why: a claim only excludes the writers that ASK for one, and the reaper asks
/// for none. `resume_inner` writes `Active` before any runtime exists, so for
/// the rest of that resume the reaper sees "Active, no runtime" and marks the
/// record `Stopped` — handing the next supervisor tick a record it is free to
/// launch a SECOND time. That is the live race in full.
/// What: holds a real claim over an `Active` record and calls the transition the
/// reaper uses, asserting the typed refusal and that the state is untouched.
/// Fails on e1ee6cf52, where the record becomes `Stopped`.
/// Test: this function IS the test.
#[tokio::test]
async fn the_reaper_leaves_a_session_whose_resume_is_in_flight_alone() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mgr = Arc::new(
        SessionManager::new(dir.path(), Arc::new(FakeNoopTmuxDriver))
            .await
            .expect("session manager"),
    );
    let workdir = dir.path().to_path_buf();
    let record = mgr
        .create(
            "reaper-race".into(),
            Some(workdir.clone()),
            None,
            None,
            None,
            None,
        )
        .await
        .expect("create");
    let id = record.id;
    mgr.set_workspace(&id, workdir, ManagedSessionState::Active)
        .await
        .expect("set Active");

    let claim = mgr.begin_resume(&id).expect("the resume takes the claim");
    let reaped = mgr.mark_runtime_exited_stopped(&id).await;

    assert!(
        matches!(reaped, Err(ManagedError::ResumeInFlight(ref s)) if s == &id.to_string()),
        "the reaper must decline, typed, rather than flip the state under the \
         path that holds it: {reaped:?}"
    );
    assert_eq!(
        mgr.get(&id).await.expect("record").state,
        ManagedSessionState::Active,
        "and the record must be exactly as the resume left it"
    );

    // Released, the reaper does its ordinary job again.
    drop(claim);
    assert!(
        mgr.mark_runtime_exited_stopped(&id).await.is_ok(),
        "a released claim must not leave the record permanently unreapable"
    );
}

/// The guard is per-session, not a global resume lock: one slow session must
/// not stall the whole fleet's supervisor tick.
#[tokio::test]
async fn concurrent_resumes_of_different_sessions_both_proceed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (mgr, first_id) = stopped_session(&dir, "inflight-a").await;
    let second_id = seed_session(&mgr, dir.path().to_path_buf(), "inflight-b").await;
    assert_ne!(first_id, second_id);
    let gate = GatedRelauncher::new(Ok(()));
    assert!(mgr.install_relauncher(gate.clone()));

    let a = tokio::spawn({
        let mgr = Arc::clone(&mgr);
        async move { mgr.resume_auto(&first_id).await }
    });
    gate.entered.notified().await;

    // A is parked inside its relaunch holding ITS guard. B must still enter.
    let mut b = tokio::spawn({
        let mgr = Arc::clone(&mgr);
        async move { mgr.resume_auto(&second_id).await }
    });
    // Racing B's ENTRY against B's COMPLETION is what makes a wrongly-global
    // lock fail this test instead of hanging it: a refused B never reaches the
    // relaunch, so it finishes early and this arm reports the refusal. A plain
    // wait on `entered` would block forever — a hung test proves nothing.
    tokio::select! {
        () = gate.entered.notified() => {}
        joined = &mut b => panic!(
            "a resume of a different session must enter the relaunch, not return early — \
             the guard is per-session, not a global resume lock: {joined:?}"
        ),
    }
    assert_eq!(
        gate.spawns.load(Ordering::SeqCst),
        2,
        "a resume of a different session must not be blocked by the first"
    );

    gate.release_one();
    gate.release_one();
    a.await.expect("join a").expect("a resumes");
    b.await.expect("join b").expect("b resumes");
}
