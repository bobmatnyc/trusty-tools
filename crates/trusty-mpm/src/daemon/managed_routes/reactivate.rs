//! POST /api/v1/sessions/managed/{id}/reactivate route handler (#2023 C, #2453).
//!
//! Why: `managed_routes/mod.rs` was at the 500-SLOC production cap; this
//! route (and its doc comment) is extracted here, mirroring how `lifecycle.rs`
//! and `activity.rs` already keep `mod.rs` under budget.
//! What: one axum handler, `reactivate_managed_session`, that delegates to
//! [`crate::session_manager::SessionManager::mark_reactivated`], falling back
//! to [`reconcile_stale_active_then_reactivate`] (#2453) when the record is
//! `Active` but the daemon's own periodic reap tick has not yet caught up
//! with the pane's actual runtime-exited state.
//! Test: `mark_reactivated_flips_stopped_to_active`,
//! `mark_reactivated_rejects_non_stopped` in `session_manager::reactivate_tests`;
//! `should_reconcile_stale_active_*` below.

use std::sync::Arc;

use axum::{
    Json,
    extract::{Path as AxumPath, State},
    http::StatusCode,
    response::IntoResponse,
};

use crate::daemon::orphan_gc::{ChildLivenessProbe, PaneInfo, ProcessTreeProbe};
use crate::daemon::runtime_reap::find_runtime_exited;
use crate::daemon::state::DaemonState;
use crate::session_manager::{
    ManagedError, ManagedSessionId, ManagedSessionState, SessionManager, SessionRecord,
};

use super::{parse_id, record_to_summary};

/// POST /api/v1/sessions/managed/{id}/reactivate — flip Stopped -> Active IN
/// PLACE, with NO tmux mutation (#2023 component C).
///
/// Why: `resume` (in `lifecycle.rs`) always kills any surviving tmux session
/// and creates a fresh one — correct for the daemon-driven restart path, but
/// WRONG for the bare-`tm` in-pane relaunch: the operator is running `tm` from
/// inside the very pane `SessionManager::mark_runtime_exited_stopped` (#2023
/// A) left alive, and is about to `exec` `claude` directly back into that SAME
/// pane. This route gives that path a dedicated, non-destructive transition —
/// [`crate::session_manager::SessionManager::mark_reactivated`] only flips the
/// record's state.
/// What: 404 when the id is unknown; on a non-`Stopped` record, tries
/// [`reconcile_stale_active_then_reactivate`] (#2453) before giving up — this
/// closes the up-to-60s window where a pane's `claude` process has already
/// exited but the periodic reap tick (or a racing `SessionEnd` hook) has not
/// yet flipped the record's `state` to `Stopped`; still 409 when the record is
/// genuinely not reconcilable (a live runtime, a different lifecycle state, or
/// a sibling pane in the same tmux session that is still active). 200 with the
/// updated summary on success either way.
/// Test: `mark_reactivated_flips_stopped_to_active`,
/// `mark_reactivated_rejects_non_stopped` in `session_manager::reactivate_tests`;
/// `should_reconcile_stale_active_*` below.
pub async fn reactivate_managed_session(
    State(state): State<Arc<DaemonState>>,
    AxumPath(id_str): AxumPath<String>,
) -> impl IntoResponse {
    let id = match parse_id(&id_str) {
        Ok(id) => id,
        Err((code, msg)) => return (code, msg).into_response(),
    };
    let mgr = state.session_manager().await;
    match mgr.mark_reactivated(&id).await {
        Ok(record) => Json(record_to_summary(&record)).into_response(),
        Err(ManagedError::SessionNotFound(_)) => {
            (StatusCode::NOT_FOUND, format!("session {id_str} not found")).into_response()
        }
        Err(ManagedError::InvalidState(_, reason)) => {
            match reconcile_stale_active_then_reactivate(&mgr, &id).await {
                Some(record) => Json(record_to_summary(&record)).into_response(),
                None => (StatusCode::CONFLICT, reason).into_response(),
            }
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// #2453: reconcile a stale-`Active` record before refusing a reactivate.
///
/// Why: `mark_reactivated` above is Stopped-only by design (#2023 C) — but a
/// pane whose `claude` process just exited reads `Active` on the record for up
/// to 60 seconds (the daemon's `REAP_INTERVAL_SECS`, `daemon/mod.rs`) of lag
/// before the periodic `runtime_reap` tick (or a racing `SessionEnd` hook)
/// catches up.
/// Bare `tm`, run in that exact pane within that window, would otherwise be
/// told "not Stopped" and fall through to the destructive guided-picker
/// resume/attach path (#2453's root cause). This closes that gap by
/// independently re-checking the record's OWN pane with the SAME conservative
/// classification [`crate::daemon::runtime_reap::stop_runtime_exited`] uses on
/// its periodic tick, and — only when that confirms the runtime has genuinely
/// exited — folding the `mark_runtime_exited_stopped` transition into this
/// call before reactivating, instead of leaving the operator to wait out the
/// reap tick.
/// What: re-fetches the record; bails (`None`) unless it is `Active` and
/// [`should_reconcile_stale_active`] confirms every pane belonging to its tmux
/// session is idle (a still-live pane — including a genuinely different,
/// still-active sibling window in the SAME tmux session, #2157 — keeps the
/// existing 409 refusal, preserving that safety boundary). On confirmation,
/// calls [`SessionManager::mark_runtime_exited_stopped`] then
/// [`SessionManager::mark_reactivated`]; any failure at either step (tmux
/// discovery, pane listing, or either state transition) also yields `None` so
/// the caller falls back to the ordinary 409.
/// Test: I/O path (shells out to a real `tmux`); not unit-tested — mirrors the
/// rest of this binary's daemon-tmux-discovery call sites. The pure
/// classification it delegates to is `should_reconcile_stale_active_*` below.
async fn reconcile_stale_active_then_reactivate(
    mgr: &SessionManager,
    id: &ManagedSessionId,
) -> Option<SessionRecord> {
    let record = mgr.get(id).await.ok()?;
    let panes = crate::daemon::tmux::TmuxDriver::discover()
        .ok()?
        .list_managed_panes()
        .ok()?;
    if !should_reconcile_stale_active(&record, &panes, &ProcessTreeProbe) {
        return None;
    }
    mgr.mark_runtime_exited_stopped(id).await.ok()?;
    mgr.mark_reactivated(id).await.ok()
}

/// Pure predicate (#2453): should a non-`Stopped` reactivate request be
/// reconciled rather than refused?
///
/// Why: isolating the decision from the tmux round-trip
/// ([`reconcile_stale_active_then_reactivate`]) makes it exhaustively
/// unit-testable without a live daemon or tmux server. Reuses
/// [`find_runtime_exited`] — the SAME session-level classification the
/// periodic `runtime_reap` tick uses — rather than reimplementing pane
/// idleness, so a genuinely live sibling pane in the same tmux session (a
/// different window, #2157) keeps the WHOLE session "live" and this predicate
/// correctly returns `false`, preserving the existing refuse+switch-client
/// contract for that case.
/// What: `true` only when `record.state == Active` AND `find_runtime_exited`
/// (scoped to just this one record) selects it — i.e. its tmux session has at
/// least one present pane and NONE of them are still running an agent.
/// `false` for every other state (Provisioning/Errored/Decommissioned — those
/// fall through to the ordinary 409, matching `mark_reactivated`'s Stopped-only
/// contract) and for a missing/still-live pane.
/// Test: `should_reconcile_stale_active_true_when_active_and_idle`,
/// `should_reconcile_stale_active_false_when_pane_still_live`,
/// `should_reconcile_stale_active_false_when_pane_missing`,
/// `should_reconcile_stale_active_false_when_not_active`.
pub(crate) fn should_reconcile_stale_active(
    record: &SessionRecord,
    panes: &[PaneInfo],
    probe: &dyn ChildLivenessProbe,
) -> bool {
    record.state == ManagedSessionState::Active
        && !find_runtime_exited(std::slice::from_ref(record), panes, probe).is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::orphan_gc::AlwaysIdleProbe;
    use chrono::Utc;
    use std::path::PathBuf;

    /// Build a minimal `Active` record for a given tmux name — every other
    /// field is a placeholder; only `state` and `tmux_name` drive the
    /// predicate under test.
    fn active_record(tmux_name: &str) -> SessionRecord {
        SessionRecord {
            id: ManagedSessionId::new(),
            tmux_name: tmux_name.to_string(),
            cwd: PathBuf::from("/tmp/test"),
            task: "test".into(),
            state: ManagedSessionState::Active,
            created_at: Utc::now(),
            last_activity_at: None,
            workspace_path: None,
            repo_url: None,
            branch: None,
            pending_decision: None,
            proposed_default: None,
            correlation: Default::default(),
            runtime: Default::default(),
            ephemeral: false,
            workspace_owned: false,
            source_id: None,
            claude_session_id: None,
            scrollback_path: None,
            last_cwd: None,
            deliverable_id: None,
            pane_id: None,
            injection_status: Default::default(),
        }
    }

    fn pane(name: &str, cmd: &str) -> PaneInfo {
        PaneInfo {
            session_name: name.to_string(),
            pane_current_command: cmd.to_string(),
            pane_pid: Some(4242),
        }
    }

    #[test]
    fn should_reconcile_stale_active_true_when_active_and_idle() {
        // The #2453 repro case: the record still reads Active, but the pane
        // has genuinely dropped back to a bare shell — reconcile, don't 409.
        let record = active_record("tm-demo");
        let panes = vec![pane("tm-demo", "zsh")];
        assert!(should_reconcile_stale_active(
            &record,
            &panes,
            &AlwaysIdleProbe
        ));
    }

    #[test]
    fn should_reconcile_stale_active_false_when_pane_still_live() {
        // A genuinely running claude in the record's OWN pane must not be
        // reconciled — this is the ordinary "record is correctly Active" case.
        let record = active_record("tm-demo");
        let panes = vec![pane("tm-demo", "claude")];
        assert!(!should_reconcile_stale_active(
            &record,
            &panes,
            &AlwaysIdleProbe
        ));
    }

    #[test]
    fn should_reconcile_stale_active_false_when_sibling_pane_live() {
        // #2157 safety boundary: a DIFFERENT window in the SAME tmux session
        // still running claude must keep the whole session "live" — the
        // existing refuse+switch-client behavior for a genuine sibling window
        // must be preserved, not silently reconciled out from under it.
        let record = active_record("tm-demo");
        let panes = vec![pane("tm-demo", "zsh"), pane("tm-demo", "claude")];
        assert!(!should_reconcile_stale_active(
            &record,
            &panes,
            &AlwaysIdleProbe
        ));
    }

    #[test]
    fn should_reconcile_stale_active_false_when_pane_missing() {
        // No pane at all for this tmux session (session fully gone) is the
        // #1744 reaper's job, not this reconcile path — must not reconcile.
        let record = active_record("tm-demo");
        assert!(!should_reconcile_stale_active(
            &record,
            &[],
            &AlwaysIdleProbe
        ));
    }

    #[test]
    fn should_reconcile_stale_active_false_when_not_active() {
        // Every other lifecycle state must fall through to the ordinary 409 —
        // `mark_reactivated`'s Stopped-only contract is unchanged for these.
        for state in [
            ManagedSessionState::Provisioning,
            ManagedSessionState::Errored,
            ManagedSessionState::Decommissioned,
        ] {
            let mut record = active_record("tm-demo");
            record.state = state.clone();
            let panes = vec![pane("tm-demo", "zsh")];
            assert!(
                !should_reconcile_stale_active(&record, &panes, &AlwaysIdleProbe),
                "state {state} must not be reconciled"
            );
        }
    }
}
