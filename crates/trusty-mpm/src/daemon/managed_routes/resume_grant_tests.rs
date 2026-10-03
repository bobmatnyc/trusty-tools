//! The #8983 resume grant on both daemon resume paths.
//!
//! Why: a resumed `claude` can rebind its session id only while the grant
//! exists, so the grant must be in place when the launch line reaches the
//! pane. A launch that fails puts no `claude` there, so its grant must go.
//! What: a tmux driver that reads the grant at the moment the launch line
//! (`--launch-spec`) is sent, then refuses that line, so `spawn_resume`
//! returns `Err`. Each test drives one real resume path and asserts the grant
//! was held at the send and revoked after the failure.
//! Test: this file.

use std::sync::{Arc, Mutex, OnceLock, Weak};

use crate::core::session::SessionId;
use crate::daemon::state::DaemonState;
use crate::session_manager::{ManagedError, ManagedSessionState, ManagedTmuxDriver};

/// A driver that records, per launch line, whether the session's resume grant
/// existed, and refuses every launch line.
struct GrantAtLaunchDriver {
    state: Arc<OnceLock<Weak<DaemonState>>>,
    session: SessionId,
    seen: Arc<Mutex<Vec<bool>>>,
}

impl ManagedTmuxDriver for GrantAtLaunchDriver {
    fn create_session(&self, _name: &str, _workdir: &str) -> Result<(), ManagedError> {
        Ok(())
    }

    fn kill_session(&self, _name: &str) -> Result<(), ManagedError> {
        Ok(())
    }

    fn send_line(&self, _name: &str, text: &str) -> Result<(), ManagedError> {
        if !text.contains(" --launch-spec ") {
            return Ok(());
        }
        let granted = self
            .state
            .get()
            .and_then(Weak::upgrade)
            .is_some_and(|s| s.session_claudes().resume_grant(self.session).is_some());
        self.seen.lock().expect("observations").push(granted);
        Err(ManagedError::TmuxUnavailable(
            "the pane refused the launch line".into(),
        ))
    }

    fn capture(&self, _name: &str, _lines: usize) -> Result<String, ManagedError> {
        Ok(String::new())
    }

    fn list_sessions(&self) -> Result<Vec<String>, ManagedError> {
        Ok(Vec::new())
    }

    /// #9101: the pane names its session, so the resume spawn proves it is
    /// the record's own and reaches the launch line.
    fn get_pane_id(&self, name: &str) -> Option<String> {
        Some(crate::test_support::self_describing_pane(name))
    }

    fn pane_identity(
        &self,
        pane_id: &str,
    ) -> Result<crate::session_manager::pane_identity::PaneIdentity, ManagedError> {
        Ok(crate::test_support::self_describing_identity(pane_id))
    }
}

/// An isolated daemon whose tmux driver is a [`GrantAtLaunchDriver`] for
/// `session`; returns it and the driver's observations.
async fn daemon_watching(
    root: &std::path::Path,
    session: SessionId,
) -> (Arc<DaemonState>, Arc<Mutex<Vec<bool>>>) {
    let slot = Arc::new(OnceLock::new());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let driver = Arc::new(GrantAtLaunchDriver {
        state: Arc::clone(&slot),
        session,
        seen: Arc::clone(&seen),
    });
    let state = Arc::new(
        DaemonState::with_root_isolated_managed_and_driver(root.join("framework"), driver).await,
    );
    let _ = slot.set(Arc::downgrade(&state));
    (state, seen)
}

/// #8983: an operator resume holds its grant when the launch line is sent,
/// and revokes it when `spawn_resume` fails.
#[cfg(unix)]
#[serial_test::serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_operator_resume_holds_its_grant_through_the_launch_line_8983() {
    let _claude = crate::test_support::fake_claude_on_path();
    let dir = tempfile::tempdir().expect("tempdir");
    let session = SessionId::new();
    let (state, seen) = daemon_watching(dir.path(), session).await;
    let mgr = state.session_manager().await;
    let ws = dir.path().join("ws");
    std::fs::create_dir_all(&ws).expect("workspace dir");
    let record = mgr
        .create(
            "grant-span".into(),
            Some(ws.clone()),
            None,
            None,
            None,
            None,
        )
        .await
        .expect("create");
    let id = record.id;
    mgr.set_workspace(&id, ws, ManagedSessionState::Active)
        .await
        .expect("set Active");
    mgr.stop(&id).await.expect("stop");
    mgr.set_claude_session_id(&id, &session.0.to_string())
        .await
        .expect("claude session id");

    let got = tokio::time::timeout(
        std::time::Duration::from_secs(60),
        super::lifecycle::resume_managed(&state, &id),
    )
    .await
    .expect("resume_managed must finish well inside the budget");

    assert!(got.is_err(), "the refused launch line fails the resume");
    assert_eq!(
        *seen.lock().expect("observations"),
        vec![true],
        "the grant exists when the launch line is sent"
    );
    assert_eq!(
        state.session_claudes().resume_grant(session),
        None,
        "a failed launch revokes the grant"
    );
}

/// #8983: an automatic relaunch holds its grant when the launch line is sent,
/// and revokes it when `spawn_resume` fails.
#[cfg(unix)]
#[serial_test::serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_auto_relaunch_holds_its_grant_through_the_launch_line_8983() {
    use crate::session_manager::relaunch::RuntimeRelauncher as _;
    let _claude = crate::test_support::fake_claude_on_path();
    let dir = tempfile::tempdir().expect("tempdir");
    let session = SessionId::new();
    let (state, seen) = daemon_watching(dir.path(), session).await;
    let ws = dir.path().join("ws");
    std::fs::create_dir_all(&ws).expect("workspace dir");
    let mut record = crate::session_manager::tests::make_active_test_record(
        "tmpm-grant",
        "task",
        &ws.display().to_string(),
    );
    record.claude_session_id = Some(session.0.to_string());
    // #9101: a pane the gate can prove is this record's, on the fake server.
    record.pane_id = Some(crate::test_support::self_describing_pane("tmpm-grant"));
    record.tmux_server = Some(crate::test_support::FAKE_PANE_SERVER.into());

    let got = tokio::time::timeout(
        std::time::Duration::from_secs(60),
        super::auto_relaunch::DaemonRelauncher::new(&state).relaunch(&record),
    )
    .await
    .expect("the relaunch must finish well inside the budget");

    assert!(
        got.as_ref()
            .is_err_and(|e| e.contains("spawn_resume failed")),
        "{got:?}"
    );
    assert_eq!(
        *seen.lock().expect("observations"),
        vec![true],
        "the grant exists when the launch line is sent"
    );
    assert_eq!(
        state.session_claudes().resume_grant(session),
        None,
        "a failed launch revokes the grant"
    );
}
