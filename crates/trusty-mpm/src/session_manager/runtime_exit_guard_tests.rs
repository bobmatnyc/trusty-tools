//! `mark_runtime_exited_stopped` holds no store guard across tmux (#9034).
//!
//! Why: the runtime-exit reconcile ran its pane gate, scrollback capture and
//! pane-id read — synchronous tmux subprocesses with no timeout — while it
//! held the store's write guard. One stalled `tmux capture-pane` queued every
//! `list()`/`get()` behind it, so the daemon's session routes hung.
//! What: [`StallingDriver`] wraps [`FakeTmuxDriver`] and parks `capture_pane`
//! until the test releases it. The tests read or write the store while the
//! reap is parked there, then check the reap's re-validation.
//! Test: `list_not_blocked_by_stalled_runtime_exit_capture_9034`,
//! `a_record_changed_during_the_runtime_exit_capture_is_not_written_9034`.

use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use tempfile::TempDir;

use super::manager::{ManagedError, ManagedTmuxDriver, SessionManager};
use super::pane_identity::PaneIdentity;
use super::record::{ManagedSessionId, ManagedSessionState, SessionRecord, StopCause};
use super::tests::{FakeTmuxDriver, bind_pane, seed_record};

/// How long a store access may wait while the reap is parked in tmux.
const STORE_BOUND: Duration = Duration::from_millis(500);

/// Backstop so a test that fails never leaves a worker parked forever.
const PARK_BOUND: Duration = Duration::from_secs(10);

/// `(entered capture_pane, released by the test)`.
#[derive(Default)]
struct Park {
    flags: Mutex<(bool, bool)>,
    changed: Condvar,
}

/// A [`FakeTmuxDriver`] whose `capture_pane` blocks until released (#9034).
struct StallingDriver {
    inner: Arc<FakeTmuxDriver>,
    park: Park,
}

impl StallingDriver {
    /// Wait (bounded) until the reap is inside `capture_pane`.
    fn wait_entered(&self) -> bool {
        let flags = self.park.flags.lock().unwrap();
        let (flags, _) = self
            .park
            .changed
            .wait_timeout_while(flags, PARK_BOUND, |f| !f.0)
            .unwrap();
        flags.0
    }

    /// Let the parked `capture_pane` return.
    fn release(&self) {
        self.park.flags.lock().unwrap().1 = true;
        self.park.changed.notify_all();
    }
}

impl ManagedTmuxDriver for StallingDriver {
    fn create_session(&self, name: &str, workdir: &str) -> Result<(), ManagedError> {
        self.inner.create_session(name, workdir)
    }

    fn kill_session(&self, name: &str) -> Result<(), ManagedError> {
        self.inner.kill_session(name)
    }

    fn send_line(&self, name: &str, text: &str) -> Result<(), ManagedError> {
        self.inner.send_line(name, text)
    }

    fn capture(&self, name: &str, lines: usize) -> Result<String, ManagedError> {
        self.inner.capture(name, lines)
    }

    fn list_sessions(&self) -> Result<Vec<String>, ManagedError> {
        self.inner.list_sessions()
    }

    fn pane_exists(&self, name: &str, pane_id: &str) -> bool {
        self.inner.pane_exists(name, pane_id)
    }

    fn pane_identity(&self, pane_id: &str) -> Result<PaneIdentity, ManagedError> {
        self.inner.pane_identity(pane_id)
    }

    /// Parks like a stalled `tmux capture-pane`, then answers as the fake.
    fn capture_pane(
        &self,
        name: &str,
        pane_id: &str,
        lines: usize,
    ) -> Result<String, ManagedError> {
        let mut flags = self.park.flags.lock().unwrap();
        flags.0 = true;
        self.park.changed.notify_all();
        let flags = self
            .park
            .changed
            .wait_timeout_while(flags, PARK_BOUND, |f| !f.1)
            .unwrap();
        drop(flags);
        self.inner.capture_pane(name, pane_id, lines)
    }
}

/// An `Active` record on an owned pane `%1` with a real workspace, so the
/// reap passes `owned_pane` and reaches the capture.
async fn stalled_reap_fixture() -> (
    TempDir,
    Arc<SessionManager>,
    Arc<StallingDriver>,
    ManagedSessionId,
) {
    let dir = TempDir::new().unwrap();
    let inner = FakeTmuxDriver::new();
    *inner.pane_id_override.lock().unwrap() = Some("%1".into());
    let driver = Arc::new(StallingDriver {
        inner,
        park: Park::default(),
    });
    let mgr = Arc::new(
        SessionManager::new(dir.path(), driver.clone())
            .await
            .expect("manager"),
    );
    let id = ManagedSessionId::new();
    seed_record(&mgr, &dir, id, ManagedSessionState::Active, false).await;
    bind_pane(&mgr, &id).await;
    (dir, mgr, driver, id)
}

/// #9034: `list()` answers while the runtime-exit reap is stalled in
/// `tmux capture-pane`. Before the fix the reap held the store's write guard
/// across the capture, so `list()` waited the whole stall.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn list_not_blocked_by_stalled_runtime_exit_capture_9034() {
    let (_dir, mgr, driver, id) = stalled_reap_fixture().await;
    let m = Arc::clone(&mgr);
    let reap = tokio::spawn(async move { m.mark_runtime_exited_stopped(&id).await });
    assert!(driver.wait_entered(), "the reap never reached capture_pane");

    let listed = tokio::time::timeout(STORE_BOUND, mgr.list()).await;
    driver.release();
    let reaped = reap.await.expect("join");

    assert!(
        listed.is_ok(),
        "list() waited {STORE_BOUND:?} behind the reap's stalled capture-pane (#9034)"
    );
    let reaped = reaped.expect("reap");
    assert_eq!(reaped.state, ManagedSessionState::Stopped);
    let stored = mgr.get(&id).await.expect("get");
    assert_eq!(stored.state, ManagedSessionState::Stopped);
    assert!(
        stored.scrollback_path.is_some(),
        "the capture result was not kept"
    );
}

/// What a concurrent writer does to the record while the reap is in tmux.
#[derive(Debug, Clone, Copy)]
enum Change {
    /// A deliberate stop lands first.
    Stopped,
    /// The record is deleted.
    Removed,
    /// Still `Active`, but rebound to another pane.
    Rebound,
}

/// Apply `change` to `id`'s record under a fresh store guard.
async fn apply(mgr: &SessionManager, id: &ManagedSessionId, change: Change) {
    let mut store = mgr.store.write().await;
    let mut record: SessionRecord = store.cached_get(id).expect("record");
    match change {
        Change::Stopped => {
            record.state = ManagedSessionState::Stopped;
            record.stop_cause = Some(StopCause::Deliberate);
        }
        Change::Removed => {
            store.remove(id).await.expect("remove");
            return;
        }
        Change::Rebound => record.pane_id = Some("%2".into()),
    }
    store.upsert(record).await.expect("upsert");
}

/// #9034: a record that changed state, was removed, or changed underneath
/// while the reap was in tmux is not written. The reap answers as a reap of
/// the already-changed record does, and no residency bump happens.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_record_changed_during_the_runtime_exit_capture_is_not_written_9034() {
    for change in [Change::Stopped, Change::Removed, Change::Rebound] {
        let (_dir, mgr, driver, id) = stalled_reap_fixture().await;
        let generation = mgr.residency_generation();
        let m = Arc::clone(&mgr);
        let reap = tokio::spawn(async move { m.mark_runtime_exited_stopped(&id).await });
        assert!(
            driver.wait_entered(),
            "{change:?}: the reap never reached capture_pane"
        );

        let written = tokio::time::timeout(STORE_BOUND, apply(&mgr, &id, change)).await;
        driver.release();
        let reaped = reap.await.expect("join");
        assert!(
            written.is_ok(),
            "{change:?}: the store guard was held across tmux (#9034)"
        );

        let after = mgr.get(&id).await;
        match change {
            Change::Stopped => {
                assert!(
                    matches!(reaped, Err(ManagedError::InvalidState(..))),
                    "{change:?}: {reaped:?}"
                );
                let after = after.expect("get");
                assert_eq!(after.state, ManagedSessionState::Stopped);
                assert!(
                    matches!(after.stop_cause, Some(StopCause::Deliberate)),
                    "{change:?}: the reap overwrote the deliberate stop: {:?}",
                    after.stop_cause
                );
            }
            Change::Removed => {
                assert!(
                    matches!(reaped, Err(ManagedError::SessionNotFound(_))),
                    "{change:?}: {reaped:?}"
                );
                assert!(
                    matches!(after, Err(ManagedError::SessionNotFound(_))),
                    "{change:?}: the reap resurrected a removed record: {after:?}"
                );
            }
            Change::Rebound => {
                assert!(
                    matches!(reaped, Err(ManagedError::InvalidState(..))),
                    "{change:?}: {reaped:?}"
                );
                let after = after.expect("get");
                assert_eq!(after.state, ManagedSessionState::Active, "{change:?}");
                assert_eq!(after.pane_id.as_deref(), Some("%2"), "{change:?}");
            }
        }
        assert_eq!(
            mgr.residency_generation(),
            generation,
            "{change:?}: a refused reap advanced the residency generation"
        );
    }
}
