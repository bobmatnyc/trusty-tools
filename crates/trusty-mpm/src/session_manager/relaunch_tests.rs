//! Tests for the automatic-resume runtime relaunch seam (#8233, owner ruling
//! 2026-09-18).
//!
//! Why: the defect is that `resume_auto` marked a record `Active` without ever
//! putting a runtime in the pane. These tests pin the two halves of the fix —
//! the relauncher IS called, and a relaunch that reports no runtime leaves the
//! record in a non-`Active` state with the failure recorded.
//! What: a scripted relauncher whose verdict the test chooses, over the same
//! hermetic manager the rest of `session_manager`'s tests use.
//! Test: this file.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::RuntimeRelauncher;
use crate::session_manager::record::ManagedSessionId;
use crate::session_manager::{
    FakeNoopTmuxDriver, ManagedSessionState, SessionManager, SessionRecord,
};

/// A relauncher with a fixed verdict that counts its calls.
struct ScriptedRelauncher {
    verdict: Result<(), String>,
    calls: AtomicUsize,
}

impl ScriptedRelauncher {
    fn ok() -> Arc<Self> {
        Arc::new(Self {
            verdict: Ok(()),
            calls: AtomicUsize::new(0),
        })
    }
    fn failing(msg: &str) -> Arc<Self> {
        Arc::new(Self {
            verdict: Err(msg.to_owned()),
            calls: AtomicUsize::new(0),
        })
    }
}

#[async_trait::async_trait]
impl RuntimeRelauncher for ScriptedRelauncher {
    async fn relaunch(&self, _record: &SessionRecord) -> Result<(), String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.verdict.clone()
    }
}

/// A hermetic manager plus one `Stopped`, auto-resumable session whose workdir
/// exists on disk (`resolve_existing_workdir` existence-checks it, #2250).
async fn manager_with_stopped_session() -> (tempfile::TempDir, Arc<SessionManager>, ManagedSessionId)
{
    let dir = tempfile::tempdir().expect("tempdir");
    let workdir = dir.path().to_path_buf();
    let mgr = Arc::new(
        SessionManager::new(dir.path(), Arc::new(FakeNoopTmuxDriver))
            .await
            .expect("session manager"),
    );
    let record = mgr
        .create(
            "relaunch-test".into(),
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
    // #6194: the runtime exited on its own, so the session is auto-resumable.
    mgr.mark_runtime_exited_stopped(&id)
        .await
        .expect("mark stopped");
    (dir, mgr, id)
}

#[tokio::test]
async fn installing_a_relauncher_twice_keeps_the_first() {
    let (_d, mgr, _id) = manager_with_stopped_session().await;
    let first = ScriptedRelauncher::ok();
    assert!(mgr.install_relauncher(first.clone()));
    assert!(
        !mgr.install_relauncher(ScriptedRelauncher::failing("second")),
        "a second install must be refused, not swap the launch path under a running sweep"
    );
}

#[tokio::test]
async fn an_auto_resume_relaunches_the_runtime() {
    // #8233: `resume_inner` prepares only the PANE. Without this call the
    // session came back as a bare shell behind an `Active` record.
    let (_d, mgr, id) = manager_with_stopped_session().await;
    let relauncher = ScriptedRelauncher::ok();
    assert!(mgr.install_relauncher(relauncher.clone()));

    mgr.resume_auto(&id).await.expect("the resume must succeed");

    assert_eq!(
        relauncher.calls.load(Ordering::SeqCst),
        1,
        "an auto-resume must go through the runtime adapter, exactly as an interactive one does"
    );
    let record = mgr.get(&id).await.expect("the record survives");
    assert_eq!(record.state, ManagedSessionState::Active);
}

/// FAIL-OPEN CHECK (#8233): the relaunch can fail, and before this fix the
/// failure was downgraded to nothing at all while the record advanced to
/// `Active`. This is the error arm.
#[tokio::test]
async fn an_auto_resume_whose_relaunch_finds_no_runtime_is_not_left_active() {
    let (_d, mgr, id) = manager_with_stopped_session().await;
    assert!(mgr.install_relauncher(ScriptedRelauncher::failing(
        "no runtime came up in pane 'tmpm-x'"
    )));

    let err = mgr
        .resume_auto(&id)
        .await
        .expect_err("a relaunch that produced no runtime must fail the resume");
    assert!(err.to_string().contains("did not take"), "{err}");

    let record = mgr.get(&id).await.expect("the record survives");
    assert_ne!(
        record.state,
        ManagedSessionState::Active,
        "a record must never be Active behind a pane with no runtime in it"
    );
    assert_eq!(record.state, ManagedSessionState::Errored);
    assert!(
        record.task.contains("no runtime came up"),
        "the failure must be recorded on the record: {}",
        record.task
    );
}

#[tokio::test]
async fn an_auto_resume_without_a_relauncher_behaves_as_before() {
    // Every hermetic test and any embedder that drives its own resumes lives
    // here; installing nothing must change nothing.
    let (_d, mgr, id) = manager_with_stopped_session().await;
    assert!(mgr.relauncher().is_none());
    mgr.resume_auto(&id).await.expect("the resume must succeed");
    assert_eq!(
        mgr.get(&id).await.expect("record").state,
        ManagedSessionState::Active
    );
}
