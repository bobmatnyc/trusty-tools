//! The managed-session tmux-gone reaper on [`DaemonState`] (#1744).
//!
//! Why: split from `sessions.rs` (SLOC cap) when #10101 made it judge each
//! record on its own tmux server.
//! What: [`DaemonState::reap_dead_managed_sessions`], the reap-loop entry, and
//! [`DaemonState::reap_managed_against`], its testable core.
//! Test: `super::tests`, `managed_reap_tests.rs`.

use std::collections::HashSet;

use super::core::DaemonState;
use crate::daemon::tmux::TmuxDriver;
use crate::session_manager::tmux_route::LiveSet;

impl DaemonState {
    /// Reap managed sessions whose tmux session has disappeared.
    ///
    /// Why (#1744): the 60-second reap loop only handled legacy in-memory
    /// sessions. Managed sessions that exit ungracefully (terminal kill, tmux pane
    /// close) would stay `Active` in the store for up to 60 seconds. Running this
    /// in the reap loop transitions them to `Stopped` as soon as their tmux session
    /// disappears. The `SessionEnd` hook handles the common case immediately;
    /// this is the safety net for the rare cases where the hook did not fire.
    /// What: `driver.list_sessions()` is the default server's live set, then
    /// every live workspace server is listed (#10101, each bounded, a stuck or
    /// unlistable one degrading only its own records to unknown), and
    /// [`Self::reap_managed_against`] runs on the (server, session) set. When
    /// the default server cannot be listed the pass is skipped (fail-safe: a
    /// stale Active record beats stopping a running session).
    /// Test: `reap_dead_managed_sessions_marks_stopped` in `super::tests` via
    /// `reap_managed_against`; `the_reaper_never_stops_a_live_pm_on_a_workspace_server`.
    pub async fn reap_dead_managed_sessions(&self, driver: &TmuxDriver) {
        let default: HashSet<String> = match driver.list_sessions() {
            Ok(s) => s.into_iter().map(|s| s.name).collect(),
            Err(e) => {
                tracing::warn!("managed reap skipped — tmux list-sessions failed: {e}");
                return;
            }
        };
        let real = crate::session_manager::RealTmuxDriver::from_driver(driver.clone());
        // #10101: the workspace-server probes are synchronous subprocesses.
        let observed = tokio::task::spawn_blocking(move || {
            crate::session_manager::tmux_route::observe_workspace_servers(&real, default)
        })
        .await;
        match observed {
            Ok(live) => self.reap_managed_against(&live).await,
            Err(e) => tracing::warn!("managed reap skipped — the tmux probe panicked: {e}"),
        }
    }

    /// Core of the managed-session reaper: stop every Active session not in `live`.
    ///
    /// Why: extracted from [`Self::reap_dead_managed_sessions`] so tests can
    /// exercise the stop logic with a pre-built live set without requiring a real
    /// `TmuxDriver` or tmux binary. The outer function handles the tmux call;
    /// this method is the deterministic, testable heart.
    /// What: for every Active managed session `live` does not hold on its own
    /// server (#10101), calls `SessionManager::stop_with_cause` (best-effort kill + mark
    /// Stopped). Failures are logged and swallowed so the caller's reap loop
    /// continues.
    ///
    /// #6194 — WHICH cause, and why the empty set is its own case, judged per
    /// server ([`LiveSet::server_lost`], #10101). An EMPTY server set means no tmux session of any kind exists on this host, because
    /// `TmuxDriver::list_sessions` maps "no server running" to `Ok(vec![])`.
    /// That is a whole-server loss — `tmux kill-server`, a crash, an upgrade, a
    /// logout — and it is not attributable to anyone's decision about any
    /// individual session, so those records are stopped as
    /// [`StopCause::Unexpected`] and stay auto-resumable, which is how the
    /// supervisor recovered such a fleet before #6194. A NON-EMPTY `live` means
    /// the tmux server is up and other sessions are running, so a record whose
    /// own name is missing was killed on purpose:
    /// [`StopCause::Deliberate`], and no automatic path revives it. The
    /// one-session host is genuinely ambiguous — tmux exits its server when the
    /// last session dies, so killing the only session and killing the server
    /// produce the identical observation — and it is resolved toward restoring
    /// the fleet, the more recoverable of the two mistakes (`tm session stop`
    /// still records Deliberate on that host and still sticks).
    ///
    /// #8233 — a session whose resume is IN FLIGHT is skipped entirely. See the
    /// guard's own comment in the body for why a missing `tmux_name` proves
    /// nothing while a resume is recreating that very session.
    ///
    /// #8942 — a protected-kind record goes `Stopped` record-only
    /// ([`crate::session_manager::SessionManager::mark_stopped_record_only`]).
    ///
    /// Test: `the_tmux_gone_reaper_marks_an_architect_record_stopped_without_teardown`,
    /// `reap_dead_managed_sessions_marks_stopped`,
    /// `reap_marks_a_targeted_kill_deliberate`,
    /// `reap_leaves_a_whole_server_loss_auto_resumable`,
    /// `reap_managed_against_skips_a_session_whose_resume_is_in_flight` in
    /// `super::tests`; `the_reaper_never_stops_a_live_pm_on_a_workspace_server`.
    pub(crate) async fn reap_managed_against<L: LiveSet + Sync>(&self, live: &L) {
        let mgr = self.session_manager().await;
        // #9238: a session renamed in tmux keeps its pane; follow it first,
        // so the name check below never stops it.
        mgr.follow_tmux_renames().await;
        let records = mgr.list().await;
        for r in records {
            // #10101: live and lost are judged on the record's own server.
            if matches!(r.state, crate::session_manager::ManagedSessionState::Active)
                && !live.holds(&r)
            {
                // #6194: a lost tmux SERVER is not N killed sessions.
                let cause = if live.server_lost(&r) {
                    crate::session_manager::StopCause::Unexpected
                } else {
                    crate::session_manager::StopCause::Deliberate
                };
                // #8233 acceptance item 4: `resume_inner`'s recreate branch
                // KILLS and rebuilds this tmux session, so a `live` set snapshot
                // taken before that kill reports the name missing while the
                // resume is still mid-flight. Stopping here kills the pane the
                // resume just made and stamps `Deliberate`, which no automatic
                // path ever revives. The claim is not a lock this sweep can
                // take (it must decline, not queue), so it reads it — the same
                // rule `mark_runtime_exited_stopped` follows.
                if mgr.is_resume_in_flight(&r.id) {
                    tracing::info!(
                        id = %r.id,
                        name = %r.tmux_name,
                        "reaper: a resume is in flight for this session; leaving it alone (#8233)"
                    );
                    continue;
                }
                // #8942: the Architect's pane is gone, so its record says so,
                // but no teardown runs for it.
                let stopped = if r.kind.is_protected() {
                    mgr.mark_stopped_record_only(&r.id, cause).await
                } else {
                    mgr.stop_with_cause(&r.id, cause).await
                };
                match stopped {
                    Ok(_) => tracing::info!(
                        id = %r.id,
                        name = %r.tmux_name,
                        cause = ?cause,
                        "reaper: marked managed session Stopped (tmux gone, #1744)"
                    ),
                    Err(e) => tracing::warn!(
                        id = %r.id,
                        name = %r.tmux_name,
                        "reaper: failed to mark managed session Stopped: {e}"
                    ),
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "managed_reap_tests.rs"]
mod tests;
