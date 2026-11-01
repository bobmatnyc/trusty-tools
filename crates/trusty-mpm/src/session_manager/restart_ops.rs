//! Graceful single-session teardown for the session manager.
//!
//! Why: `manager.rs` is near the 500-SLOC production cap; placing the
//! teardown in a sibling file keeps both files under the limit while keeping
//! related lifecycle logic co-located in the `session_manager` module tree.
//! #10597 (owner ruling D82): the daemon-shutdown `shutdown` that stopped
//! every managed session is deleted; sessions survive a daemon stop.
//! What: `impl SessionManager { graceful_terminate_runtime,
//! terminate_proven_runtime }` — one record's pane gets ONE signal — SIGTERM
//! when its `claude` PID is known (#1595 PR B), else a single Ctrl-C — then a
//! 2 s async grace window, then a kill by `$N` session id after a second
//! ownership check (#9101).
//! Test: `graceful_terminate_runtime_signals_then_kills` (restart_tests.rs).

use std::time::Duration;

use tracing::warn;

use super::manager::{ManagedError, SessionManager};
use super::record::SessionRecord;
use super::runtime_identity::{self, RuntimeOwnership, RuntimeTeardown};

/// Grace window given to each session between signal and tmux kill.
///
/// Why: 2 s is the industry convention (matches systemd's default
/// `TimeoutStopSec` and launchd's `ExitTimeOut`) and is enough for claude to
/// flush its in-memory state to disk without stalling the stop.
/// In test builds, the grace window is 0 s so unit tests are not blocked by
/// the sleep (eliminating the need for tokio's `test-util` feature).
#[cfg(not(test))]
const SIGTERM_GRACE_SECS: u64 = 2;
#[cfg(test)]
const SIGTERM_GRACE_SECS: u64 = 0;

impl SessionManager {
    /// Gracefully terminate ONE record's runtime: signal → grace → kill.
    ///
    /// Why: CLI-initiated `stop`/`decommission` tear down a single session at a
    /// time and were previously abrupt — a bare `kill_session` gave the inner
    /// `claude` process no chance to flush state (memory snapshot, git index,
    /// session record) before its pane was reclaimed. This drains the session,
    /// honouring the repo's graceful-shutdown convention (SIGTERM drain, not an
    /// abrupt kill).
    /// #8935: a tmux name is reusable, so the teardown first proves the live
    /// session holds the record's own `%N` pane
    /// ([`runtime_identity::runtime_ownership`]). A session that does not, or
    /// whose ownership cannot be proved, is neither signalled nor killed.
    /// What: classifies the runtime. `Absent` → [`RuntimeTeardown::Absent`], no
    /// signal, no grace sleep. Any live name → [`Self::kill_gate`] first.
    /// `Foreign` → warns under `caller` and returns [`RuntimeTeardown::Foreign`];
    /// `Unverifiable`, or a probe error → [`RuntimeTeardown::Unproven`]. `Owned`
    /// → the `claude` PID of the record's pane, `signal_terminate_pane`
    /// (SIGTERM when the PID is known, else one Ctrl-C to that pane), a
    /// [`SIGTERM_GRACE_SECS`] async grace window (0 s in tests), a second
    /// ownership check, and a kill only when the pane is still the
    /// session's; a session gone by then reports `Terminated`. #9004: the kill
    /// is `kill_session_id` on the `$N` id that re-check read, never the name.
    /// A kill failure is logged, not returned, because
    /// the caller still needs to mark the record `Stopped` / decommissioned.
    /// #8942 (critic HIGH): the one failure that IS returned is a kill-floor
    /// refusal, [`ManagedError::KillRefused`] — from [`Self::kill_gate`] under
    /// `caller`'s name before any signal, or from the driver's own floor at the
    /// kill — so the caller aborts before it removes a workspace or moves the
    /// record.
    /// Test: `graceful_terminate_runtime_signals_then_kills` in restart_tests.rs;
    /// `stopping_a_stale_record_never_signals_the_live_session_that_reused_its_name`,
    /// `an_unlistable_pane_set_leaves_the_runtime_running`,
    /// `stopping_a_record_whose_pane_is_live_still_kills_it`,
    /// `a_pane_that_changes_hands_during_the_grace_window_is_not_killed`,
    /// `a_session_gone_after_the_grace_window_is_terminated_without_a_kill` in
    /// runtime_identity_tests.rs; the refusal by
    /// `an_undeterminable_floor_aborts_stop_and_decommission_of_an_ordinary_session`.
    pub(crate) async fn graceful_terminate_runtime(
        &self,
        record: &SessionRecord,
        caller: &str,
    ) -> Result<RuntimeTeardown, ManagedError> {
        let tmux_name = record.tmux_name.as_str();
        // #8935: prove the live session is this record's before any signal.
        // The fast path (`Absent`) skips the grace sleep, so callers looping
        // over dead sessions do not pay SIGTERM_GRACE_SECS each.
        let (ownership, liveness_known) = self.classify_runtime(record);
        if ownership == RuntimeOwnership::Absent {
            return Ok(RuntimeTeardown::Absent);
        }
        // #8942: a live name the floor cannot clear refuses the teardown,
        // typed, whichever session holds it.
        self.kill_gate(tmux_name, caller)?;
        // #8935: the driver's own floor answers here too, before the ownership
        // verdict can turn a protected name into a quiet record-only stop.
        if let Some(why) = self.tmux.architect_floor().refuse(tmux_name, caller) {
            return Err(ManagedError::KillRefused(why));
        }
        let (pane_id, _) = match owned_pane(record, caller, ownership, liveness_known, false) {
            Ok(owned) => owned,
            Err(teardown) => return Ok(teardown),
        };
        // #8935: the record's own pane's claude, not the session's active pane;
        // with no pid the Ctrl-C fallback targets that pane too.
        let pid = crate::core::process::find_claude_pid_in_pane(tmux_name, &pane_id);
        self.tmux.signal_terminate_pane(tmux_name, &pane_id, pid);
        tokio::time::sleep(Duration::from_secs(SIGTERM_GRACE_SECS)).await;
        // #8935: the grace window is long enough for the name to change hands.
        // #8935 delta critic: an unproven verdict here says the pane was signalled.
        let (ownership, liveness_known) = self.classify_runtime(record);
        let session_id = match owned_pane(record, caller, ownership, liveness_known, true) {
            Ok((_, session_id)) => session_id,
            Err(teardown) => {
                return Ok(match teardown {
                    RuntimeTeardown::Absent => RuntimeTeardown::Terminated,
                    other => other,
                });
            }
        };
        // #9004: kill the `$N` session the re-check just proved holds the
        // record's pane. A by-name kill here would reach a session that took
        // the name after the re-check; a new session never gets this `$N`.
        match self.tmux.kill_session_id(tmux_name, &session_id) {
            Err(refused @ ManagedError::KillRefused(_)) => return Err(refused),
            Err(e) => warn!(
                name = %tmux_name, session_id = %session_id,
                "graceful_terminate_runtime: kill by session id failed (may already be gone): {e}"
            ),
            Ok(()) => {}
        }
        Ok(RuntimeTeardown::Terminated)
    }

    /// [`Self::graceful_terminate_runtime`] for a caller that destroys state
    /// after the teardown, refusing a teardown whose ownership is unproven.
    ///
    /// Why (#8935 critic round): decommission removes the workspace and
    /// tombstones the record, and the idle reaper marks a record `Stopped`.
    /// Doing either under a live claude that may be this record's own loses
    /// work. A `Foreign` verdict is safe: the live session is another one.
    /// What: runs the teardown; [`RuntimeTeardown::Unproven`] becomes
    /// [`ManagedError::InvalidState`] naming the reason, whether a signal was
    /// sent, and the two ways out (#8935 delta critic), so the caller returns
    /// before it changes anything. Every other verdict passes through.
    /// Test: `decommission_with_an_unlistable_pane_set_changes_nothing`,
    /// `a_pane_list_lost_after_the_signal_refuses_decommission`,
    /// `the_idle_reaper_stop_skips_an_unproven_runtime`.
    pub(crate) async fn terminate_proven_runtime(
        &self,
        record: &SessionRecord,
        caller: &str,
    ) -> Result<RuntimeTeardown, ManagedError> {
        match self.graceful_terminate_runtime(record, caller).await? {
            RuntimeTeardown::Unproven {
                why,
                liveness_known,
                signalled,
            } => Err(ManagedError::InvalidState(
                record.id.to_string(),
                unproven_refusal(record, caller, &why, liveness_known, signalled),
            )),
            teardown => Ok(teardown),
        }
    }

    /// [`runtime_identity::runtime_ownership`] for a teardown, with whether
    /// liveness is known: a probe error counts as unverifiable with liveness
    /// unknown, so a teardown that cannot prove ownership kills nothing (#8935).
    /// Test: `an_unreadable_session_probe_leaves_the_runtime_running`.
    fn classify_runtime(&self, record: &SessionRecord) -> (RuntimeOwnership, bool) {
        match runtime_identity::runtime_ownership(record, self.tmux.as_ref()) {
            Ok(ownership) => (ownership, true),
            Err(e) => (
                RuntimeOwnership::Unverifiable(format!(
                    "the tmux probe for session '{}' failed ({e})",
                    record.tmux_name
                )),
                false,
            ),
        }
    }
}

/// The operator-facing text of an unproven-teardown refusal (#8935 delta
/// critic): the reason, what was done, and the two ways out.
/// Test: `a_pane_list_lost_after_the_signal_refuses_decommission`.
fn unproven_refusal(
    record: &SessionRecord,
    caller: &str,
    why: &str,
    liveness_known: bool,
    signalled: bool,
) -> String {
    let name = &record.tmux_name;
    let doubt = if liveness_known {
        format!("so the live tmux session '{name}' may still be this record's")
    } else {
        format!("so liveness unknown: tmux cannot say whether session '{name}' runs")
    };
    let done = if signalled {
        "This record's pane was signalled; its workspace and record were kept."
    } else {
        "The workspace and the record were left as they are."
    };
    format!(
        "{caller} refused: {why}, {doubt}. {done} If that session is this \
         record's, end it yourself and rerun; otherwise run \
         `tm session delete --force {id}` to drop the record only.",
        id = record.id
    )
}

/// `Ok((pane_id, session_id))` when `ownership` proves `record` owns its live
/// tmux session; otherwise the [`RuntimeTeardown`] to report, with a warning
/// naming `caller` (#8935, #9004).
/// Test: `stopping_a_stale_record_never_signals_the_live_session_that_reused_its_name`.
fn owned_pane(
    record: &SessionRecord,
    caller: &str,
    ownership: RuntimeOwnership,
    liveness_known: bool,
    signalled: bool,
) -> Result<(String, String), RuntimeTeardown> {
    match ownership {
        RuntimeOwnership::Owned {
            pane_id,
            session_id,
        } => Ok((pane_id, session_id)),
        RuntimeOwnership::Absent => Err(RuntimeTeardown::Absent),
        // #8935 critic round: the two verdicts stay apart so a destructive
        // caller can refuse an unproven one.
        RuntimeOwnership::Foreign(why) => {
            warn!(
                id = %record.id, name = %record.tmux_name, caller,
                "{why}; nothing was killed (#8935)"
            );
            Err(RuntimeTeardown::Foreign(why))
        }
        RuntimeOwnership::Unverifiable(why) => {
            warn!(
                id = %record.id, name = %record.tmux_name, caller,
                "{why}; nothing was killed (#8935)"
            );
            Err(RuntimeTeardown::Unproven {
                why,
                liveness_known,
                signalled,
            })
        }
    }
}
