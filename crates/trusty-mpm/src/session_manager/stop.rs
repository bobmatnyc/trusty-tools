//! Stopping a managed session's runtime, and recording why it stopped (#6194).
//!
//! Why: extracted from `manager.rs` when adding [`SessionManager::stop_with_cause`]
//! pushed that file to 505 SLOC, past its 500 cap. This follows the precedent
//! `create.rs` / `reactivate.rs` / `reconcile.rs` already set for this file: a
//! cohesive pair of methods moves to its own sibling module rather than the cap
//! being gamed or raised. The pair is cohesive because one delegates to the
//! other — `stop` IS `stop_with_cause` with the cause every "end this session"
//! request implies.
//! What: [`SessionManager::stop`] and [`SessionManager::stop_with_cause`]. No
//! behavior change — a pure relocation.
//! Test: `manager_stop_keeps_workspace` in `tests.rs`;
//! `stop_refuses_terminal_record` in `delete_tests.rs`;
//! `stop_records_deliberate_cause` in `stop_cause_tests.rs`;
//! `reap_marks_a_targeted_kill_deliberate`,
//! `reap_leaves_a_whole_server_loss_auto_resumable` in `daemon::state`'s tests.

use tracing::info;

use super::manager::{ManagedError, SessionManager};
use super::record::{ManagedSessionId, ManagedSessionState, SessionRecord, StopCause};

impl SessionManager {
    /// Stop the runtime of a managed session, keeping the workspace intact.
    ///
    /// Why: a session ENDURES beyond its running runtime. `stop` terminates the
    /// tmux session and the `claude` process inside it, but PRESERVES the
    /// workspace directory on disk and the session record so the session can
    /// be resumed later via `resume`.
    /// What: captures a pane snapshot, then GRACEFULLY terminates the runtime via
    /// [`Self::graceful_terminate_runtime`] (SIGTERM the `claude` process, grace
    /// window, then reclaim the pane — #1975) so the process can flush state
    /// before it dies, marks the record `Stopped` (workspace path untouched), and
    /// persists.
    /// A TERMINAL record (`Decommissioned`/`Deleted`) is REFUSED with
    /// [`ManagedError::InvalidState`] — mirroring [`resume`](Self::resume)'s
    /// state guard — so a stale zombie-reconcile path (`runtime-stop` then
    /// `resume`) can never flip a deleted/decommissioned tombstone back to a
    /// live `Stopped` state and resurrect it (code-critic CRITICAL).
    /// Records [`StopCause::Deliberate`] (#6194): a caller of `stop` is asking
    /// for this session to end, so no automatic path may relaunch what it
    /// stopped — see [`SessionRecord::is_auto_resumable`]. A caller that
    /// observes a stop rather than requesting one uses
    /// [`Self::stop_with_cause`].
    /// Test: `manager_stop_keeps_workspace`; `stop_refuses_terminal_record`
    /// (a Deleted record cannot be stopped) in `delete_tests`;
    /// `stop_records_deliberate_cause` in `stop_cause_tests`.
    pub async fn stop(&self, id: &ManagedSessionId) -> Result<SessionRecord, ManagedError> {
        self.stop_with_cause(id, StopCause::Deliberate).await
    }

    /// [`Self::stop`], with the caller naming why the session is stopping.
    ///
    /// Why (#6194): `stop` reads as "somebody asked for this", and for five of
    /// its six callers that is exactly right. The sixth is the tmux-gone reaper
    /// ([`crate::daemon::state::DaemonState::reap_managed_against`]), which
    /// observes a fact rather than carrying a request — and the fact it sees is
    /// not always attributable. `TmuxDriver::list_sessions` maps "no server
    /// running" to an empty list, so a whole-server loss (`tmux kill-server`, a
    /// crash, a logout) is indistinguishable from an operator killing one named
    /// session, and stamping every record in that sweep `Deliberate` would
    /// leave the entire fleet permanently un-auto-resumable — the behavior the
    /// supervisor used to recover from. The reaper decides which it saw and
    /// says so here; every other caller keeps `stop`'s plain contract.
    /// What: identical to [`Self::stop`] — same terminal-record refusal, same
    /// pane snapshot, same graceful runtime teardown, same `Stopped`
    /// transition — except that `cause` is recorded instead of an assumed
    /// [`StopCause::Deliberate`]. Unlike the boot reconciler's `get_or_insert`,
    /// this ASSIGNS: a session that was running until this call has no earlier
    /// cause worth preserving.
    /// Test: `stop_records_deliberate_cause` in `stop_cause_tests`;
    /// `reap_marks_a_targeted_kill_deliberate`,
    /// `reap_leaves_a_whole_server_loss_auto_resumable`,
    /// `reap_dead_managed_sessions_marks_stopped` in
    /// [`crate::daemon::state`]'s tests.
    pub async fn stop_with_cause(
        &self,
        id: &ManagedSessionId,
        cause: StopCause,
    ) -> Result<SessionRecord, ManagedError> {
        let mut record = self.get(id).await?;
        if record.state.is_terminal() {
            return Err(ManagedError::InvalidState(
                id.to_string(),
                format!(
                    "cannot stop a session in terminal state '{}'; \
                     a decommissioned/deleted record is gone for good",
                    record.state
                ),
            ));
        }
        super::snapshot::capture_into(&mut record, &*self.tmux).await;
        // Graceful teardown (#1975): give the claude process a SIGTERM + grace
        // window to checkpoint before its tmux pane is reclaimed, instead of an
        // abrupt `kill_session`. The snapshot above already preserved the pane.
        self.graceful_terminate_runtime(&record.tmux_name).await;
        record.state = ManagedSessionState::Stopped;
        // #6194: the caller names the cause; `stop` supplies Deliberate for
        // every "end this session" request, and an automatic resume must not
        // undo one of those.
        record.stop_cause = Some(cause);
        self.store.write().await.upsert(record.clone()).await?;
        info!(id = %id, name = %record.tmux_name, cause = ?cause, "managed session stopped (workspace intact)");
        Ok(record)
    }
}
