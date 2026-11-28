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
//! [`AnswerDriver`] hook `restore_on_kill` (or `restore_on_list`, for a
//! teardown that kills nothing) makes the directory writable again, so the
//! teardown's own write lands. A second manager over the same directory reads
//! what reached disk, which is what a daemon restart's reconcile sees.
//! Test: this file IS the test module; run with `cargo test -p trusty-mpm`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tempfile::TempDir;

use super::decision_tests::{AnswerDriver, seed_answerable, set_mode};
use super::manager::{ManagedError, ManagedTmuxDriver, SessionManager};
use super::record::{ManagedSessionId, ManagedSessionState, SessionRecord};
use super::setters::RefusedSpawn;
use super::tests::FAKE_SESSION_ID;
use super::worktree_git_fixture::GitWorktreeFixture;
use super::worktree_ownership::sentinel_payload_bytes;
use super::worktree_ownership_location::write_sentinel_bytes;

const NAME: &str = "tm-refused-12077";
const TASK: &str = "refused spawn";

/// When the store directory becomes writable again during the teardown.
#[derive(Clone, Copy)]
enum Heal {
    /// At the teardown's kill.
    OnKill,
    /// At the teardown's first session-list probe, before any kill.
    OnList,
}

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
        self.created_as(ManagedSessionId::new(), ws, owned).await
    }

    /// [`Self::created`] under a known `id`, for a worktree sentinel naming it.
    async fn created_as(&self, id: ManagedSessionId, ws: &Path, owned: bool) -> SessionRecord {
        let record = self
            .mgr
            .create_with_reserved_name(
                id,
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

    /// The opt-in write, with the store read-only from that call until `heal`.
    async fn refuse(
        &self,
        record: SessionRecord,
        heal: Heal,
    ) -> Result<SessionRecord, RefusedSpawn> {
        let dir: PathBuf = self.data.path().to_path_buf();
        {
            let mut hooks = self.driver.hooks.lock().unwrap();
            match heal {
                Heal::OnKill => hooks.restore_on_kill = Some(dir.clone()),
                Heal::OnList => hooks.restore_on_list = Some(dir.clone()),
            }
        }
        set_mode(&dir, 0o555);
        let out = self.mgr.set_claude_sandbox_noted(record, true).await;
        set_mode(&dir, 0o755);
        out
    }

    /// The record as a restarted daemon finds it: a second manager over the
    /// same directory reads the disk, runs its boot reconcile, and answers.
    async fn after_restart(&self, id: &ManagedSessionId) -> SessionRecord {
        let reborn = SessionManager::new(self.data.path(), self.driver.clone())
            .await
            .expect("second manager");
        reborn.reconcile_on_boot(false).await.expect("reconcile");
        reborn.get(id).await.expect("record")
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

    let out = s.refuse(record.clone(), Heal::OnKill).await;

    assert!(
        matches!(
            out,
            Err(RefusedSpawn {
                source: ManagedError::Store(_),
                ..
            })
        ),
        "{out:?}"
    );
    assert!(!s.live(&record.tmux_name), "a bare shell is still running");
    // #12077: a proven teardown kills by the `$N` id, never by name.
    assert!(
        s.driver
            .inner
            .kill_calls
            .lock()
            .unwrap()
            .iter()
            .any(|k| k == FAKE_SESSION_ID),
        "the session was not killed"
    );
    assert_eq!(
        std::fs::read_to_string(main.path().join("README.md")).expect("main kept"),
        "main\n"
    );
    // #12077: no `claude_sandbox` assertion. The failed `upsert` already put
    // `true` in memory, so the tombstone saves `true`; a terminal record never
    // resumes, so the value is inert there.
    let stored = s.mgr.get(&record.id).await.expect("record");
    assert_eq!(stored.state, ManagedSessionState::Decommissioned);
}

/// #12077: a refused in-project spawn also removes the worktree it reserved.
/// Red on 2e27940463, where the tree stayed.
#[tokio::test]
async fn a_refused_opt_in_removes_the_worktree_the_spawn_reserved_12077() {
    let s = spawn().await;
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("refused-12077");
    // #12077: the sentinel `create_daemon_session_worktree` writes, naming the
    // session the tree is reserved for.
    let id = ManagedSessionId::new();
    write_sentinel_bytes(&wt, &sentinel_payload_bytes(id)).expect("ownership marker");
    std::fs::write(wt.join("TASK.md"), TASK).expect("TASK.md");
    let record = s.created_as(id, &wt, false).await;

    let out = s.refuse(record.clone(), Heal::OnKill).await;

    assert!(
        matches!(
            out,
            Err(RefusedSpawn {
                source: ManagedError::Store(_),
                ..
            })
        ),
        "{out:?}"
    );
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
    s.refuse(record.clone(), Heal::OnKill)
        .await
        .expect_err("refused");

    s.mgr.reconcile_on_boot(false).await.expect("reconcile");

    let stored = s.mgr.get(&record.id).await.expect("record");
    assert_eq!(stored.state, ManagedSessionState::Decommissioned);
    assert!(!s.live(&record.tmux_name));
    assert!(s.mgr.resume_inner(&record.id).await.is_err());
}

/// #12077: when the teardown itself fails, the caller still gets the
/// original refusal, the pane is still killed, and the record a restart reads
/// is a tombstone, never `Active`. The teardown is refused by an Active
/// sibling claiming the same owned workspace. Red on ddd5dc7df0, where the
/// refusal left the shell up and a restart re-adopted the record.
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

    let out = s.refuse(record.clone(), Heal::OnKill).await;

    assert!(
        matches!(
            out,
            Err(RefusedSpawn {
                source: ManagedError::Store(_),
                ..
            })
        ),
        "{out:?}"
    );
    // #12077: a killed pane is not "left running".
    let text = out.expect_err("refused").to_string();
    assert!(!text.contains("pane left running"), "{text}");
    assert!(!s.live(&record.tmux_name), "a bare shell is still running");
    assert!(ws.path().exists(), "the fallback removed a workspace");
    let stored = s.after_restart(&record.id).await;
    assert_eq!(stored.state, ManagedSessionState::Decommissioned);
    assert!(s.mgr.resume_inner(&record.id).await.is_err());
}

/// #12077: a refused session whose pane cannot be proven (no `pane_id`) keeps
/// its shell, since nothing may kill an unproven pane, but the record a
/// restart reads is a tombstone, so reconcile never adopts it and nothing
/// resumes it. Red on ddd5dc7df0, where a restart re-adopted it as `Active`.
#[tokio::test]
async fn an_unproven_refused_pane_still_leaves_an_unadoptable_record_12077() {
    let s = spawn().await;
    *s.driver.inner.pane_id_override.lock().unwrap() = None;
    let main = TempDir::new().expect("main checkout");
    let record = s.created(main.path(), false).await;
    assert!(
        record.pane_id.is_none(),
        "the fixture must leave no pane id"
    );

    let out = s.refuse(record.clone(), Heal::OnList).await;

    assert!(
        matches!(
            out,
            Err(RefusedSpawn {
                source: ManagedError::Store(_),
                ..
            })
        ),
        "{out:?}"
    );
    // #12077: the refusal names the pane it left up, and keeps the original text.
    let text = out.expect_err("refused").to_string();
    assert!(text.contains("pane left running"), "{text}");
    assert!(text.contains(&record.tmux_name), "{text}");
    assert!(text.contains("store error"), "{text}");
    assert!(
        s.driver.inner.kill_calls.lock().unwrap().is_empty(),
        "an unproven pane was killed"
    );
    let stored = s.after_restart(&record.id).await;
    assert_eq!(stored.state, ManagedSessionState::Decommissioned);
    assert!(s.mgr.resume_inner(&record.id).await.is_err());
}
