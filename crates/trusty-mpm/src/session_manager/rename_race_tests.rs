//! Two concurrent renames of ONE live record (#9101).
//!
//! Why: since #9101 a live rename addresses tmux by the session's `$N` id, so
//! the `=old` name target no longer makes the second of two racing renames
//! fail at tmux. Unserialized, both renames retitle the session, one loses
//! Guard 2, and its rollback leaves tmux and the record under different names.
//! What: [`ByIdDriver`] wraps [`FakeTmuxDriver`] so `rename_session_id`
//! renames whatever name the session holds NOW, as `tmux rename-session -t $N`
//! does, and holds the first rename until a second arrives (or a timeout), so
//! an unserialized pair is forced to interleave.
//! Test: `two_concurrent_renames_of_one_live_record_leave_tmux_and_the_record_in_step`.

use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use tempfile::TempDir;

use super::manager::{ManagedError, ManagedTmuxDriver, SessionManager};
use super::pane_identity::PaneIdentity;
use super::record::{ManagedSessionId, ManagedSessionState};
use super::tests::{FakeTmuxDriver, bind_pane, seed_record};

/// How long the first rename waits for a racing second one to reach tmux.
const RENDEZVOUS: Duration = Duration::from_millis(500);

/// A [`FakeTmuxDriver`] whose one live session is renamed by id (#9101).
struct ByIdDriver {
    inner: Arc<FakeTmuxDriver>,
    /// The name the single fake session holds right now.
    current: Mutex<Option<String>>,
    /// How many `rename_session_id` calls have arrived.
    arrivals: Mutex<usize>,
    arrived: Condvar,
}

impl ByIdDriver {
    /// Count this caller in, then wait (bounded) until two callers are in.
    fn rendezvous(&self) {
        let mut n = self.arrivals.lock().unwrap();
        *n += 1;
        self.arrived.notify_all();
        let _ = self
            .arrived
            .wait_timeout_while(n, RENDEZVOUS, |n| *n < 2)
            .unwrap();
    }
}

impl ManagedTmuxDriver for ByIdDriver {
    fn create_session(&self, name: &str, workdir: &str) -> Result<(), ManagedError> {
        *self.current.lock().unwrap() = Some(name.to_owned());
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

    /// By id: the caller's `name` is ignored; the session's CURRENT name moves.
    fn rename_session_id(
        &self,
        _name: &str,
        session_id: &str,
        new: &str,
    ) -> Result<(), ManagedError> {
        self.rendezvous();
        let mut current = self.current.lock().unwrap();
        let now = current.clone().unwrap_or_default();
        self.inner.rename_session_id(&now, session_id, new)?;
        *current = Some(new.to_owned());
        Ok(())
    }
}

/// #9101 code-critic r2 HIGH: two renames of one live record, racing, must
/// leave the live tmux session under the name the record persists.
///
/// Before the fix both pass Guard 1, both rename `$0` by id, one loses
/// Guard 2 and rolls `$0` back to the ORIGINAL name while the record keeps
/// the winner's name. Serialized, the second rename starts from the first's
/// result and both succeed.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_concurrent_renames_of_one_live_record_leave_tmux_and_the_record_in_step() {
    let dir = TempDir::new().unwrap();
    let inner = FakeTmuxDriver::new();
    *inner.pane_id_override.lock().unwrap() = Some("%1".into());
    let driver = Arc::new(ByIdDriver {
        inner: inner.clone(),
        current: Mutex::new(None),
        arrivals: Mutex::new(0),
        arrived: Condvar::new(),
    });
    let mgr = Arc::new(
        SessionManager::new(dir.path(), driver.clone())
            .await
            .expect("manager"),
    );
    let id = ManagedSessionId::new();
    seed_record(&mgr, &dir, id, ManagedSessionState::Active, false).await;
    bind_pane(&mgr, &id).await;

    let (m1, m2) = (Arc::clone(&mgr), Arc::clone(&mgr));
    let first = tokio::spawn(async move { m1.rename(&id, "tm-race-a").await });
    let second = tokio::spawn(async move { m2.rename(&id, "tm-race-b").await });
    let (r1, r2) = (first.await.expect("join"), second.await.expect("join"));

    let record = mgr.get(&id).await.expect("get").tmux_name;
    let live = inner.list_sessions().expect("list");
    assert_eq!(
        live,
        vec![record.clone()],
        "tmux holds {live:?} but the record says '{record}' (results: {r1:?} / {r2:?})"
    );
    assert!(r1.is_ok() && r2.is_ok(), "{r1:?} / {r2:?}");
}
