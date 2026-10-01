//! Graceful-shutdown helpers for the session manager.
//!
//! Why: `manager.rs` is near the 500-SLOC production cap; placing the
//! shutdown operation in a sibling file keeps both files well under the limit
//! while keeping related lifecycle logic co-located in the `session_manager`
//! module tree.
//! What: `impl SessionManager { shutdown }` — gracefully stops every live
//! (non-terminal) managed session. For each session it resolves the live
//! `claude` PID (a single quick `find_claude_pid_in_tmux` probe), waits 2 s
//! asynchronously so the claude process can flush state, then calls
//! `graceful_stop(name, pid)` ONCE — SIGTERM when the PID is known (#1595 PR B),
//! else a single Ctrl-C fallback. There is exactly one signal per session: the
//! earlier double-Ctrl-C (a pre-signal loop *plus* `graceful_stop`'s own
//! fallback interrupt) is gone. The 2 s wait is done with `tokio::time::sleep` —
//! never a blocking `std::thread::sleep` — so it does not starve the Tokio
//! thread pool.
//! Test: `shutdown_calls_graceful_stop_for_active_sessions` (integration test
//! in session_manager/tests.rs), `fake_driver_graceful_stop_with_pid`,
//! `fake_driver_graceful_stop_without_pid`, `default_graceful_stop_sends_sigterm`.

use std::time::Duration;

use tracing::{info, warn};

use super::manager::{ManagedError, SessionManager};
use super::record::{ManagedSessionState, SessionRecord};
use super::runtime_identity::{self, RuntimeOwnership, RuntimeTeardown};

/// Grace window given to each session between signal and tmux kill.
///
/// Why: 2 s is the industry convention (matches systemd's default
/// `TimeoutStopSec` and launchd's `ExitTimeOut`) and is enough for claude to
/// flush its in-memory state to disk without stalling the overall shutdown.
/// In test builds, the grace window is 0 s so unit tests are not blocked by
/// the sleep (eliminating the need for tokio's `test-util` feature).
#[cfg(not(test))]
const SIGTERM_GRACE_SECS: u64 = 2;
#[cfg(test)]
const SIGTERM_GRACE_SECS: u64 = 0;

impl SessionManager {
    /// Gracefully stop all live managed sessions on daemon shutdown.
    ///
    /// Why: the graceful-shutdown path in `daemon/mod.rs` needs to give every
    /// running session a chance to persist its state before the process exits.
    /// `kill_session` is abrupt; `shutdown` separates signal from kill: it
    /// resolves each session's live `claude` PID, awaits a [`SIGTERM_GRACE_SECS`]
    /// async grace window so the processes can flush state, then calls
    /// `graceful_stop(name, pid)` ONCE per session. `graceful_stop` sends SIGTERM
    /// when the PID is known (#1595 PR B) and falls back to a single Ctrl-C only
    /// when it is not — so each session receives exactly ONE termination signal.
    /// The previous implementation pre-signalled every session with Ctrl-C *and*
    /// then let `graceful_stop(None)` send another, double-interrupting the pane;
    /// that pre-signal loop is removed. Using `tokio::time::sleep` (never
    /// `std::thread::sleep`) keeps the Tokio thread pool free; the one-sleep-for-
    /// all approach bounds total shutdown time at O(1).
    /// What: collects all Active/Provisioning records; resolves each session's
    /// PID via a single [`crate::core::process::find_claude_pid_in_tmux`] probe
    /// (one attempt — at shutdown we do not retry); awaits the grace window once;
    /// calls `graceful_stop(name, pid)` per session, failing open. Logs a summary.
    /// #8942: live protected-kind records, and names the kill floor refuses,
    /// are left out.
    /// Test: `shutdown_calls_graceful_stop_for_active_sessions` in tests.rs;
    /// `shutdown_skips_a_supervisor_session`.
    pub async fn shutdown(&self) {
        let records = self.list().await;
        // Only stop sessions that have a live runtime: Active and Provisioning.
        // Stopped, Errored, and Decommissioned sessions have no running process.
        // #8942: the Architect's sessions are left out, and so is any name the
        // floor cannot clear.
        let live: Vec<_> = records
            .into_iter()
            .filter(|r| {
                matches!(
                    r.state,
                    ManagedSessionState::Active | ManagedSessionState::Provisioning
                ) && !super::supervisor::skip_protected(r, "shutdown")
                    && self
                        .kill_gate(&r.tmux_name, "SessionManager::shutdown")
                        .is_ok()
            })
            .collect();

        if live.is_empty() {
            info!("shutdown: no live managed sessions to stop");
            return;
        }

        // Resolve each session's live `claude` PID up front (a single quick probe
        // per session — no retry budget at shutdown). A known PID lets
        // `graceful_stop` send SIGTERM directly instead of a tmux Ctrl-C.
        let pids: Vec<Option<u32>> = live
            .iter()
            .map(|r| {
                crate::core::process::find_claude_pid_in_tmux(
                    &r.tmux_name,
                    1,
                    Duration::from_millis(0),
                )
            })
            .collect();

        // Single async grace window for ALL sessions — bounds shutdown at O(1)
        // and lets the claude processes flush state before the kill.
        tokio::time::sleep(Duration::from_secs(SIGTERM_GRACE_SECS)).await;

        // Exactly one signal + kill per session via `graceful_stop`: SIGTERM when
        // the PID is known, else a single Ctrl-C fallback (no double interrupt).
        let mut stopped = 0usize;
        for (record, pid) in live.iter().zip(pids) {
            match self.tmux.graceful_stop(&record.tmux_name, pid) {
                Ok(()) => {
                    stopped += 1;
                }
                Err(e) => {
                    warn!(
                        name = %record.tmux_name,
                        "shutdown: graceful_stop failed (may already be gone): {e}"
                    );
                }
            }
        }
        info!("shutdown: gracefully stopped {stopped} live managed session(s)");
    }

    /// Gracefully terminate ONE record's runtime: signal → grace → kill.
    ///
    /// Why: CLI-initiated `stop`/`decommission` tear down a single session at a
    /// time (unlike [`Self::shutdown`], which batches one grace window across the
    /// whole fleet) and were previously abrupt — a bare `kill_session` gave the
    /// inner `claude` process no chance to flush state (memory snapshot, git
    /// index, session record) before its pane was reclaimed. This drains one
    /// session the same way `shutdown` drains the fleet, honouring the repo's
    /// graceful-shutdown convention (SIGTERM drain, not an abrupt kill).
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
    /// ownership check, and `kill_session` only when the pane is still the
    /// session's; a session gone by then reports `Terminated`. A `kill_session` failure is logged, not returned, because
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
        let ownership = self.classify_runtime(record);
        if ownership == RuntimeOwnership::Absent {
            return Ok(RuntimeTeardown::Absent);
        }
        // #8942: a live name the floor cannot clear refuses the teardown,
        // typed, whichever session holds it.
        self.kill_gate(tmux_name, caller)?;
        // #8935: the driver's own floor answers here too, before the ownership
        // verdict can turn a protected name into a quiet record-only stop.
        if let Some(why) = self.tmux.supervisor_floor().refuse(tmux_name, caller) {
            return Err(ManagedError::KillRefused(why));
        }
        let pane_id = match owned_pane(record, caller, ownership) {
            Ok(pane_id) => pane_id,
            Err(teardown) => return Ok(teardown),
        };
        // #8935: the record's own pane's claude, not the session's active pane;
        // with no pid the Ctrl-C fallback targets that pane too.
        let pid = crate::core::process::find_claude_pid_in_pane(tmux_name, &pane_id);
        self.tmux.signal_terminate_pane(tmux_name, &pane_id, pid);
        tokio::time::sleep(Duration::from_secs(SIGTERM_GRACE_SECS)).await;
        // #8935: the grace window is long enough for the name to change hands.
        if let Err(teardown) = owned_pane(record, caller, self.classify_runtime(record)) {
            return Ok(match teardown {
                RuntimeTeardown::Absent => RuntimeTeardown::Terminated,
                other => other,
            });
        }
        match self.tmux.kill_session(tmux_name) {
            Err(refused @ ManagedError::KillRefused(_)) => return Err(refused),
            Err(e) => warn!(
                name = %tmux_name,
                "graceful_terminate_runtime: kill_session failed (may already be gone): {e}"
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
    /// [`ManagedError::InvalidState`] naming the reason, so the caller returns
    /// before it changes anything. Every other verdict passes through.
    /// Test: `decommission_with_an_unlistable_pane_set_changes_nothing`,
    /// `the_idle_reaper_stop_skips_an_unproven_runtime`.
    pub(crate) async fn terminate_proven_runtime(
        &self,
        record: &SessionRecord,
        caller: &str,
    ) -> Result<RuntimeTeardown, ManagedError> {
        match self.graceful_terminate_runtime(record, caller).await? {
            RuntimeTeardown::Unproven(why) => Err(ManagedError::InvalidState(
                record.id.to_string(),
                format!(
                    "{caller} refused: {why}, so the live session may still be \
                     this record's; the workspace and the record were left as they are"
                ),
            )),
            teardown => Ok(teardown),
        }
    }

    /// [`runtime_identity::runtime_ownership`] for a teardown: a probe error
    /// counts as unverifiable, so a teardown that cannot prove ownership kills
    /// nothing (#8935).
    /// Test: `an_unreadable_session_probe_leaves_the_runtime_running`.
    fn classify_runtime(&self, record: &SessionRecord) -> RuntimeOwnership {
        runtime_identity::runtime_ownership(record, self.tmux.as_ref()).unwrap_or_else(|e| {
            RuntimeOwnership::Unverifiable(format!(
                "the tmux probe for session '{}' failed ({e})",
                record.tmux_name
            ))
        })
    }
}

/// `Ok(pane_id)` when `ownership` proves `record` owns its live tmux session;
/// otherwise the [`RuntimeTeardown`] to report, with a warning naming
/// `caller` (#8935).
/// Test: `stopping_a_stale_record_never_signals_the_live_session_that_reused_its_name`.
fn owned_pane(
    record: &SessionRecord,
    caller: &str,
    ownership: RuntimeOwnership,
) -> Result<String, RuntimeTeardown> {
    match ownership {
        RuntimeOwnership::Owned { pane_id } => Ok(pane_id),
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
            Err(RuntimeTeardown::Unproven(why))
        }
    }
}
