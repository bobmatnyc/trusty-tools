//! A spawn refused for an unstored sandbox opt-in leaves nothing running (#12077).
//!
//! Why: `launch_on_main` and `spawn_managed_inproject` both create the tmux
//! session, then call `set_claude_sandbox` and refuse the spawn when it fails.
//! The refusal returned with the session still up as a bare shell (and, in
//! the in-project path, the reserved worktree still on disk), and reconcile
//! re-adopts a live session as `Active`, so a later resume ran it unsandboxed.
//! What: drives `set_claude_sandbox` on a record `create_with_reserved_name`
//! made, with the store directory read-only so its write fails. Both spawn
//! callers refuse through this one call, so these tests cover each. The
//! [`AnswerDriver`] hook `restore_on_kill` makes the directory writable again
//! when the teardown kills the session, so the teardown's own write lands.
//! Test: this file IS the test module; run with `cargo test -p trusty-mpm`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tempfile::TempDir;

use super::decision_tests::{AnswerDriver, seed_answerable, set_mode};
use super::manager::{ManagedError, ManagedTmuxDriver, SessionManager};
use super::record::{ManagedSessionId, ManagedSessionState, SessionRecord};
use super::worktree_git_fixture::GitWorktreeFixture;
use super::worktree_ownership_location::write_sentinel_bytes;

const NAME: &str = "tm-refused-12077";
const TASK: &str = "refused spawn";

/// A manager over an [`AnswerDriver`] (pane `%1`, so a teardown can prove the
/// session is the record's own).
struct Spawn {
    data: TempDir,
    mgr: Arc<SessionManager>,
    driver: Arc<AnswerDriver>,
}

async fn spawn() -> Spawn {
    let data = TempDir::new().expect("data dir");
    let driver = AnswerDriver::new();
    let mgr = Arc::new(
        SessionManager::new(data.path(), driver.clone())
            .await
            .expect("manager"),
    );
    Spawn { data, mgr, driver }
}

impl Spawn {
    /// What `create_with_reserved_name` hands the caller, rooted at `ws`.
    async fn created(&self, ws: &Path, owned: bool) -> SessionRecord {
        let record = self
            .mgr
            .create_with_reserved_name(
                ManagedSessionId::new(),
                NAME.into(),
                TASK.into(),
                Some(ws.to_path_buf()),
                Some(ws.to_path_buf()),
                None,
                None,
                Default::default(),
                false,
                owned,
            )
            .await
            .expect("create");
        assert!(self.live(&record.tmux_name), "the session was created");
        record
    }

    fn live(&self, name: &str) -> bool {
        self.driver
            .list_sessions()
            .expect("list")
            .iter()
            .any(|n| n == name)
    }

    /// The opt-in write, with the store read-only for exactly that call.
    /// `heal` lets the teardown's kill make the store writable again.
    async fn refuse(
        &self,
        record: SessionRecord,
        heal: bool,
    ) -> Result<SessionRecord, ManagedError> {
        let dir: PathBuf = self.data.path().to_path_buf();
        if heal {
            self.driver.hooks.lock().unwrap().restore_on_kill = Some(dir.clone());
        }
        set_mode(&dir, 0o555);
        let out = self.mgr.set_claude_sandbox(record, true).await;
        set_mode(&dir, 0o755);
        out
    }
}

/// #12077: a refused launch-on-main spawn kills its tmux session, ends the
/// record, and leaves the operator's main checkout alone. Red on 2e27940463,
/// where the session stayed up.
#[tokio::test]
async fn a_refused_opt_in_reaps_the_session_and_keeps_the_main_checkout_12077() {
    let s = spawn().await;
    let main = TempDir::new().expect("main checkout");
    std::fs::write(main.path().join("README.md"), "main\n").expect("write");
    let record = s.created(main.path(), false).await;

    let out = s.refuse(record.clone(), true).await;

    assert!(matches!(out, Err(ManagedError::Store(_))), "{out:?}");
    assert!(!s.live(&record.tmux_name), "a bare shell is still running");
    assert!(
        s.driver
            .inner
            .kill_calls
            .lock()
            .unwrap()
            .contains(&record.tmux_name),
        "the session was not killed"
    );
    assert_eq!(
        std::fs::read_to_string(main.path().join("README.md")).expect("main kept"),
        "main\n"
    );
    let stored = s.mgr.get(&record.id).await.expect("record");
    assert_eq!(stored.state, ManagedSessionState::Decommissioned);
    assert!(!stored.claude_sandbox, "the opt-in was never stored");
}

/// #12077: a refused in-project spawn also removes the worktree it reserved.
/// Red on 2e27940463, where the tree stayed.
#[tokio::test]
async fn a_refused_opt_in_removes_the_worktree_the_spawn_reserved_12077() {
    let s = spawn().await;
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("refused-12077");
    write_sentinel_bytes(&wt, b"").expect("ownership marker");
    std::fs::write(wt.join("TASK.md"), TASK).expect("TASK.md");
    let record = s.created(&wt, false).await;

    let out = s.refuse(record.clone(), true).await;

    assert!(matches!(out, Err(ManagedError::Store(_))), "{out:?}");
    assert!(!s.live(&record.tmux_name), "a bare shell is still running");
    assert!(!wt.exists(), "the reserved worktree was kept");
}

/// #12077: reconcile cannot adopt the refused session and nothing resumes
/// it. Red on 2e27940463: the live session was re-adopted as `Active`.
#[tokio::test]
async fn a_refused_opt_in_session_is_neither_adopted_nor_resumable_12077() {
    let s = spawn().await;
    let main = TempDir::new().expect("main checkout");
    let record = s.created(main.path(), false).await;
    s.refuse(record.clone(), true).await.expect_err("refused");

    s.mgr.reconcile_on_boot(false).await.expect("reconcile");

    let stored = s.mgr.get(&record.id).await.expect("record");
    assert_eq!(stored.state, ManagedSessionState::Decommissioned);
    assert!(!s.live(&record.tmux_name));
    assert!(s.mgr.resume_inner(&record.id).await.is_err());
}

/// #12077: when the teardown itself fails, the caller still gets the
/// original refusal, never the teardown's error and never a success. The
/// teardown is refused by an Active sibling claiming the same owned workspace.
/// Passes on 2e27940463; red on a change that returns the teardown's error.
#[tokio::test]
async fn a_failed_teardown_still_returns_the_original_refusal_12077() {
    let s = spawn().await;
    let ws = TempDir::new().expect("workspace");
    let record = s.created(ws.path(), true).await;
    let sibling = seed_answerable(&s.mgr, &s.data, ManagedSessionState::Active).await;
    s.mgr
        .set_workspace(
            &sibling,
            ws.path().to_path_buf(),
            ManagedSessionState::Active,
        )
        .await
        .expect("sibling claim");

    let out = s.refuse(record, false).await;

    assert!(matches!(out, Err(ManagedError::Store(_))), "{out:?}");
}
