//! The cancel signal the two destructive prune loops read between candidates
//! (#7619).
//!
//! Why: `tm session prune-worktrees` ran the orphan sweep and the merged-PR
//! reclaim to the end of their candidate lists after the client that asked for
//! them had gone, so a killed CLI kept deleting worktrees for the rest of the
//! pass. Both loops now ask this signal before each candidate.
//! What: [`PruneCancel`], a readable stop state with one fail-closed rule:
//! anything but a clear "keep going" stops the pass. Cancel is read only
//! BETWEEN candidates, never inside a removal, so a stop never leaves a new
//! half-removed tree. The progress sink of #7619's streaming method lands here
//! too, in a later change.
//! Test: `worktree_7619_a_cancelled_orphan_sweep_stops_after_the_first_removal`,
//! `worktree_7619_a_cancelled_reclaim_stops_after_the_first_removal`,
//! `worktree_7619_cancel_during_a_removal_lets_that_removal_finish`,
//! `worktree_7619_a_partial_removal_is_never_also_cancelled_unstarted`,
//! `worktree_7619_an_unreadable_or_early_cancel_removes_nothing`.

use std::fmt;
use std::sync::Arc;

/// The reason a candidate the survey never reached carries when the pass was
/// cancelled before classifying it (#7619). Fail-closed like the deadline arm:
/// listed, blocked, never approved.
pub(crate) const CANCELLED_BEFORE_INSPECTION: &str =
    "the pass was cancelled before inspection (#7619)";

/// A prune pass's stop signal, read before each candidate (#7619).
///
/// Why: the pass outlives its caller. The HTTP route moves it into a spawned
/// task so the maintenance permit stays held (#8994), which also means a
/// dropped request no longer stops anything. This type is how the caller's
/// departure reaches the loops.
/// What: wraps a probe answering `Some(true)` (stop), `Some(false)` (keep
/// going) or `None` (the state could not be read). The probe is a closure, not
/// a `tokio_util` token, because `session_manager` builds without the
/// `daemon` feature that brings `tokio-util` in; the route adapts its token.
/// Test: `worktree_7619_an_unreadable_or_early_cancel_removes_nothing`.
#[derive(Clone)]
pub struct PruneCancel {
    state: Arc<dyn Fn() -> Option<bool> + Send + Sync>,
}

impl PruneCancel {
    /// A signal that never stops the pass: the unattended timer sweep, the
    /// PM-pause prune, the MCP tool and every pre-#7619 caller.
    pub fn never() -> Self {
        Self::from_probe(|| Some(false))
    }

    /// A signal read through `probe`; `None` from it means "unreadable".
    pub fn from_probe(probe: impl Fn() -> Option<bool> + Send + Sync + 'static) -> Self {
        Self {
            state: Arc::new(probe),
        }
    }

    /// Whether the pass must stop before its next candidate.
    ///
    /// Why: the pass deletes. A cancel state it cannot read, or any answer
    /// other than a clear "keep going", must stop it rather than let it run
    /// on (Fail-Open Check, #7619).
    /// What: `false` only for `Some(false)`; `Some(true)` and `None` stop.
    /// Test: `worktree_7619_an_unreadable_or_early_cancel_removes_nothing`.
    pub fn stop_requested(&self) -> bool {
        (self.state)() != Some(false)
    }
}

impl fmt::Debug for PruneCancel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PruneCancel")
            .field("stop_requested", &self.stop_requested())
            .finish()
    }
}

/// Log one line naming how many candidates a cancelled pass left untouched.
pub(crate) fn log_cancelled(pass: &str, unstarted: usize) {
    if unstarted > 0 {
        tracing::warn!(
            pass,
            unstarted,
            "prune-worktrees: cancelled — the removal in flight finished; the unstarted \
             candidates were left untouched (#7619)"
        );
    }
}

#[cfg(test)]
#[path = "prune_progress_tests.rs"]
mod prune_progress_tests;
