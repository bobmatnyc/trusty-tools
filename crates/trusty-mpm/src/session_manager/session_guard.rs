//! Owning RAII guard that reaps a tmux session on drop.
//!
//! Why: every tmux session the daemon spawns must have exactly ONE owner that is
//! responsible for killing it. Before this guard existed, a spawn site that
//! created a tmux session and then failed (or returned early) before recording
//! ownership leaked the session — 159 such orphans once accumulated and
//! saturated the host fork limit (epic #1452, #1453). [`TmuxSessionGuard`] makes
//! the ownership explicit and reaps the session via `Drop` UNLESS ownership is
//! explicitly transferred to a long-lived registry by calling [`disarm`].
//! What: holds the tmux session name plus an `Arc` clone of the
//! [`ManagedTmuxDriver`]. While armed, its `Drop` impl issues a best-effort
//! `kill_session` (logging at warn on failure; stderr only via `tracing`).
//! [`disarm`] flips the guard to a no-op so the tracked store/registry — not the
//! request scope — owns the session's lifetime.
//! Test: `guard_kills_on_drop_when_armed`, `guard_does_not_kill_after_disarm`,
//! `guard_kills_only_named_session` in this module's test section.
//!
//! [`disarm`]: TmuxSessionGuard::disarm

use std::sync::Arc;

use tracing::{debug, warn};

use super::manager::ManagedTmuxDriver;

/// An owning guard that kills its tmux session on drop unless disarmed.
///
/// Why: a tmux session created by a spawn site has no owner until it is recorded
/// in the persistent store. During that window — between `create_session` and a
/// successful `store.upsert` — any early return would orphan the session. This
/// guard closes that window: construct it immediately after `create_session`,
/// and the session is guaranteed to be reaped if anything fails before the
/// record is durably persisted. Once persisted, [`disarm`](Self::disarm)
/// transfers ownership to the registry so the request scope does not reap a
/// session the daemon is meant to keep tracking.
/// What: wraps the session name and an `Arc<dyn ManagedTmuxDriver>`. The `armed`
/// flag is the disarm latch: `true` (default) means `Drop` reaps; `false` means
/// `Drop` is a no-op. The driver is shared (`Arc`) so holding a guard never
/// blocks the manager from using the same driver concurrently.
/// Test: see the module test section.
pub struct TmuxSessionGuard {
    /// The tmux session name this guard owns (e.g. `tmpm-<slug>`).
    name: String,
    /// Shared handle to the tmux driver used to reap the session on drop.
    driver: Arc<dyn ManagedTmuxDriver>,
    /// While `true`, `Drop` reaps the session. [`disarm`](Self::disarm) clears it
    /// once ownership passes to the persistent registry.
    armed: bool,
}

impl TmuxSessionGuard {
    /// Construct an ARMED guard owning `name`, reaped via `driver` on drop.
    ///
    /// Why: callers create the guard at the exact moment they take ownership of a
    /// freshly-created tmux session, so the reaping window opens immediately and
    /// no early return can leak the session.
    /// What: stores `name` and the shared `driver`, with `armed = true`.
    /// Test: `guard_kills_on_drop_when_armed`.
    pub fn new(name: impl Into<String>, driver: Arc<dyn ManagedTmuxDriver>) -> Self {
        Self {
            name: name.into(),
            driver,
            armed: true,
        }
    }

    /// The tmux session name this guard owns.
    ///
    /// Why: tests and callers occasionally need to confirm which session a guard
    /// is responsible for without consuming it.
    /// What: returns the owned name as a `&str`.
    /// Test: `guard_exposes_name`.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Transfer ownership away from the guard so `Drop` will NOT reap.
    ///
    /// Why: once the session has been durably recorded in the persistent store /
    /// registry, that registry — not the request-scoped guard — owns the
    /// session's lifetime. Reaping it here would kill a session the daemon must
    /// keep tracking. Disarming makes the guard's `Drop` a no-op so ownership
    /// passes cleanly to the long-lived store.
    /// What: clears the `armed` flag. Idempotent: disarming an already-disarmed
    /// guard is harmless. After this call the guard may be dropped freely.
    /// Test: `guard_does_not_kill_after_disarm`.
    pub fn disarm(&mut self) {
        self.armed = false;
    }
}

impl std::fmt::Debug for TmuxSessionGuard {
    /// Why: `Arc<dyn ManagedTmuxDriver>` is not `Debug`, so the derive cannot be
    /// used; enclosing types may still want to print a guard.
    /// What: prints the name and armed flag, omitting the opaque driver handle.
    /// Test: compile-time only.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TmuxSessionGuard")
            .field("name", &self.name)
            .field("armed", &self.armed)
            .finish_non_exhaustive()
    }
}

impl Drop for TmuxSessionGuard {
    /// Why: this is the whole point of the guard — an armed guard going out of
    /// scope (normal return, `?` early-exit, or panic unwind) must reap the
    /// session it owns so it can never become an orphan.
    /// What: when `armed`, issues a best-effort `kill_session`. A failure is
    /// logged at `warn` (the session may already be gone, which is fine) and
    /// never panics — `Drop` must not unwind. When disarmed, does nothing.
    /// Test: `guard_kills_on_drop_when_armed`, `guard_does_not_kill_after_disarm`.
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        match self.driver.kill_session(&self.name) {
            Ok(()) => debug!(name = %self.name, "TmuxSessionGuard reaped owned session on drop"),
            Err(e) => warn!(
                name = %self.name,
                "TmuxSessionGuard: best-effort reap on drop failed (may already be gone): {e}"
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::session_manager::manager::ManagedError;

    /// A recording tmux driver that captures `kill_session` calls.
    ///
    /// Why: the guard's behavior is observable only through which `kill_session`
    /// calls it makes; this fake records them so tests can assert exact reaping.
    /// What: implements [`ManagedTmuxDriver`]; every method is a no-op except
    /// `kill_session`, which appends the name to `kills`.
    /// Test: used by the guard tests below.
    struct RecordingDriver {
        kills: Mutex<Vec<String>>,
    }

    impl RecordingDriver {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                kills: Mutex::new(Vec::new()),
            })
        }

        fn kills(&self) -> Vec<String> {
            self.kills.lock().unwrap().clone()
        }
    }

    impl ManagedTmuxDriver for RecordingDriver {
        fn create_session(&self, _name: &str, _workdir: &str) -> Result<(), ManagedError> {
            Ok(())
        }
        fn kill_session(&self, name: &str) -> Result<(), ManagedError> {
            self.kills.lock().unwrap().push(name.to_owned());
            Ok(())
        }
        fn send_line(&self, _name: &str, _text: &str) -> Result<(), ManagedError> {
            Ok(())
        }
        fn capture(&self, _name: &str, _lines: usize) -> Result<String, ManagedError> {
            Ok(String::new())
        }
        fn list_sessions(&self) -> Result<Vec<String>, ManagedError> {
            Ok(Vec::new())
        }
    }

    /// A driver whose `kill_session` always errors, to prove `Drop` never panics.
    struct FailingDriver;
    impl ManagedTmuxDriver for FailingDriver {
        fn create_session(&self, _name: &str, _workdir: &str) -> Result<(), ManagedError> {
            Ok(())
        }
        fn kill_session(&self, name: &str) -> Result<(), ManagedError> {
            Err(ManagedError::TmuxUnavailable(format!("boom for {name}")))
        }
        fn send_line(&self, _name: &str, _text: &str) -> Result<(), ManagedError> {
            Ok(())
        }
        fn capture(&self, _name: &str, _lines: usize) -> Result<String, ManagedError> {
            Ok(String::new())
        }
        fn list_sessions(&self) -> Result<Vec<String>, ManagedError> {
            Ok(Vec::new())
        }
    }

    #[test]
    fn guard_kills_on_drop_when_armed() {
        let driver = RecordingDriver::new();
        {
            let _guard = TmuxSessionGuard::new("tmpm-armed", driver.clone());
            // Guard is armed; nothing killed yet.
            assert!(driver.kills().is_empty(), "no kill before drop");
        } // <- drop here must reap.
        assert_eq!(
            driver.kills(),
            vec!["tmpm-armed".to_string()],
            "armed guard must kill its session exactly once on drop"
        );
    }

    #[test]
    fn guard_does_not_kill_after_disarm() {
        let driver = RecordingDriver::new();
        {
            let mut guard = TmuxSessionGuard::new("tmpm-disarmed", driver.clone());
            guard.disarm();
        } // <- drop here must be a no-op.
        assert!(
            driver.kills().is_empty(),
            "disarmed guard must NOT kill its session on drop (ownership transferred)"
        );
    }

    #[test]
    fn guard_kills_only_named_session() {
        let driver = RecordingDriver::new();
        drop(TmuxSessionGuard::new("tmpm-only-this", driver.clone()));
        let kills = driver.kills();
        assert_eq!(kills.len(), 1, "exactly one kill");
        assert_eq!(kills[0], "tmpm-only-this", "kills only the named session");
    }

    #[test]
    fn guard_drop_never_panics_on_kill_error() {
        // A failing driver must not cause `Drop` to unwind. The test passing
        // (no panic/abort) IS the assertion.
        let driver: Arc<dyn ManagedTmuxDriver> = Arc::new(FailingDriver);
        drop(TmuxSessionGuard::new("tmpm-doomed", driver));
    }

    #[test]
    fn guard_exposes_name() {
        let driver = RecordingDriver::new();
        let mut guard = TmuxSessionGuard::new("tmpm-name-probe", driver);
        assert_eq!(guard.name(), "tmpm-name-probe");
        guard.disarm(); // avoid an unrelated kill recording in this assertion test
    }
}
