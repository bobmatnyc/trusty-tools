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
//! behavior change — a pure relocation. #8935 adds
//! [`SessionManager::stop_reporting`], which also says whether the runtime
//! was torn down or left running.
//! Test: `manager_stop_keeps_workspace` in `tests.rs`;
//! `stop_refuses_terminal_record` in `delete_tests.rs`;
//! `stop_records_deliberate_cause` in `stop_cause_tests.rs`;
//! `reap_marks_a_targeted_kill_deliberate`,
//! `reap_leaves_a_whole_server_loss_auto_resumable` in `daemon::state`'s tests.

use tracing::info;

use super::manager::{ManagedError, SessionManager};
use super::record::{ManagedSessionId, ManagedSessionState, SessionRecord, StopCause};
use super::runtime_identity::{self, RuntimeOwnership, RuntimeTeardown};

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
    /// #8942: a live record of a protected kind is refused with
    /// [`ManagedError::InvalidState`] before any tmux call, and a kill-floor
    /// refusal ([`ManagedError::KillRefused`]) returns before the record moves.
    /// Test: `stop_records_deliberate_cause` in `stop_cause_tests`;
    /// `stop_refuses_a_supervisor_record_and_never_signals_it`,
    /// `an_undeterminable_floor_aborts_stop_and_decommission_of_an_ordinary_session`;
    /// `reap_marks_a_targeted_kill_deliberate`,
    /// `reap_leaves_a_whole_server_loss_auto_resumable`,
    /// `reap_dead_managed_sessions_marks_stopped` in
    /// [`crate::daemon::state`]'s tests.
    pub async fn stop_with_cause(
        &self,
        id: &ManagedSessionId,
        cause: StopCause,
    ) -> Result<SessionRecord, ManagedError> {
        Ok(self.stop_reporting(id, cause).await?.record)
    }

    /// [`Self::stop_with_cause`], also returning what happened to the runtime.
    ///
    /// Why (#8935): a stop whose record's tmux name now belongs to another
    /// session moves the record only; the operator-facing surfaces must say
    /// the live session was left running instead of reporting it stopped.
    /// What: the same stop; [`StopReport::runtime`] is the teardown's verdict.
    /// Test: `stopping_a_stale_record_never_signals_the_live_session_that_reused_its_name`.
    pub async fn stop_reporting(
        &self,
        id: &ManagedSessionId,
        cause: StopCause,
    ) -> Result<StopReport, ManagedError> {
        self.stop_checked(id, cause, "SessionManager::stop", false)
            .await
    }

    /// [`Self::stop_reporting`] that refuses when the live session's ownership
    /// cannot be proved, leaving the record as it was.
    ///
    /// Why (#8935 critic round): an automatic stop — the idle reaper — must
    /// not mark a record `Stopped` while a claude that may be its own still
    /// runs; the record would then never be reaped or resumed correctly.
    /// What: the same stop through [`Self::terminate_proven_runtime`]; an
    /// unproven teardown returns [`ManagedError::InvalidState`] before the
    /// record moves.
    /// Test: `the_idle_reaper_stop_skips_an_unproven_runtime`.
    pub async fn stop_proven(
        &self,
        id: &ManagedSessionId,
        cause: StopCause,
        caller: &str,
    ) -> Result<StopReport, ManagedError> {
        self.stop_checked(id, cause, caller, true).await
    }

    /// The body behind [`Self::stop_reporting`] and [`Self::stop_proven`];
    /// `require_proof` picks the teardown.
    async fn stop_checked(
        &self,
        id: &ManagedSessionId,
        cause: StopCause,
        caller: &str,
        require_proof: bool,
    ) -> Result<StopReport, ManagedError> {
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
        super::supervisor::refuse_protected(&record, super::supervisor::ProtectedVerb::Stop)?;
        // #8935: classify before the snapshot. A capture by name would write
        // whichever session holds the name now — possibly another session's,
        // even the Architect's — into this record's workspace. Only a session
        // proved to hold the record's pane is captured, from that pane.
        if let Ok(RuntimeOwnership::Owned { pane_id }) =
            runtime_identity::runtime_ownership(&record, self.tmux.as_ref())
        {
            super::snapshot::capture_into_pane(&mut record, &*self.tmux, Some(&pane_id)).await;
        }
        // Graceful teardown (#1975): give the claude process a SIGTERM + grace
        // window to checkpoint before its tmux pane is reclaimed, instead of an
        // abrupt `kill_session`. The snapshot above already preserved the pane.
        // #8942: a kill-floor refusal aborts here, before the record moves.
        // #8935: a live session that is not this record's is left running;
        // the record still moves, record-only.
        // #8935 critic round: an automatic stop refuses an unproven one.
        let runtime = if require_proof {
            self.terminate_proven_runtime(&record, caller).await?
        } else {
            self.graceful_terminate_runtime(&record, caller).await?
        };
        record.state = ManagedSessionState::Stopped;
        // #6194: the caller names the cause; `stop` supplies Deliberate for
        // every "end this session" request, and an automatic resume must not
        // undo one of those.
        record.stop_cause = Some(cause);
        self.store.write().await.upsert(record.clone()).await?;
        // #7087: a stopped session leaves the active-project set.
        self.bump_residency_generation();
        match runtime.left_running() {
            None => {
                info!(id = %id, name = %record.tmux_name, cause = ?cause, "managed session stopped (workspace intact)")
            }
            // #8935: the log must not claim a stop that tmux never saw.
            Some(why) => {
                info!(id = %id, name = %record.tmux_name, cause = ?cause, "managed session record marked Stopped (record only: {why})")
            }
        }
        Ok(StopReport { record, runtime })
    }
}

/// What [`SessionManager::stop_reporting`] did (#8935).
#[derive(Debug, Clone)]
pub struct StopReport {
    /// The record after the stop, `Stopped`.
    pub record: SessionRecord,
    /// What the teardown did to the tmux runtime.
    pub runtime: RuntimeTeardown,
}
