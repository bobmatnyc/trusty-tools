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
    extract::{Path as AxumPath, Query, State},
    http::StatusCode,
    response::IntoResponse,
};
use serde::Deserialize;

use crate::daemon::orphan_gc::{ChildLivenessProbe, PaneInfo, ProcessTreeProbe};
use crate::daemon::runtime_reap::{find_runtime_exited, session_has_live_pane};
use crate::daemon::state::DaemonState;
use crate::session_manager::{
    ManagedError, ManagedSessionId, ManagedSessionState, SessionManager, SessionRecord,
};

use super::{parse_id, record_to_summary};

/// Query parameters for the reactivate route.
///
/// Why: an in-place-relaunch client (`bin/tm`'s `guided_inplace`) runs `tm`
/// as the foreground process of the very pane it is relaunching. At the exact
/// moment it POSTs here, `tmux list-panes` reports THAT pane's
/// `pane_current_command` as `tm` (and its shell PID has `tm` as a live child)
/// — so the daemon's own idleness classification would see the pane as "still
/// running a runtime" and refuse the stale-`Active` reconcile with a 409, even
/// though the agent has genuinely exited (#2789, the reported dead-end). The
/// client therefore tells the daemon which pane is ITS OWN via the stable tmux
/// `pane_id`, so [`reconcile_stale_active_then_reactivate`] can exclude that
/// one self-referential pane from the liveness check.
/// What: an optional `caller_pane_id` (tmux's `%N`). Absent for non-tmux or
/// pre-#2789 clients, in which case reconciliation behaves exactly as before.
/// The `pane_confirmed_dead` flag (#2790-follow-up) carries the CLI's
/// self-asserted proof-of-death claim — see its field doc for the trust-model
/// discussion (accepted, not independently verified) — and
/// [`should_reconcile_stale_active`].
/// Test: `should_reconcile_stale_active_true_when_only_caller_pane_busy`,
/// `should_reconcile_stale_active_true_when_confirmed_dead_and_pane_missing`.
#[derive(Debug, Default, Deserialize)]
pub struct ReactivateQuery {
    /// The stable tmux `pane_id` of the pane the reactivate request originates
    /// from (the pane about to `exec` `claude` in place).
    #[serde(default)]
    pub caller_pane_id: Option<String>,
    /// Set by an in-place-relaunch client that has matched tmux's stable
    /// `pane_id` (`bin/tm`'s `guided::pane_identity_confirmed`) against the
    /// record and is asserting — a CLIENT SELF-ASSERTION, not something the
    /// daemon independently verifies (no process/ancestry check) — that it is
    /// running inside THIS record's OWN pane and observed the agent exit. Like
    /// every other managed-session state transition this daemon accepts from a
    /// caller (resume/stop/decommission included), the assertion is trusted
    /// under the localhost/single-operator threat model this daemon runs
    /// under, not treated as a cryptographically-verified fact. It is still
    /// bounded, not a free pass: the pane match can be wrong in ways the
    /// daemon's own tmux-pane liveness probe cannot see (the requesting `tm` is
    /// that pane's foreground command; a just-exited agent may leave a
    /// not-yet-reaped child; a racing `list-panes` may momentarily omit the
    /// pane), so when `true`, [`should_reconcile_stale_active`] trusts it OVER
    /// the probe only for the caller's OWN pane, while an INDEPENDENT
    /// sibling-pane liveness check still refuses if a genuinely live SIBLING
    /// pane keeps the tmux session busy (#2157). Absent/false for every
    /// pre-existing client, preserving the prior probe-only behavior.
    #[serde(default)]
    pub pane_confirmed_dead: bool,
}

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
/// What: 404 when the id is unknown; `mark_reactivated` accepts `Stopped` or
/// `Decommissioned` (#2777 — reviving a decommissioned session's lingering pane
/// in place, see `mark_reactivated`'s doc); on any OTHER non-reactivatable
/// state, tries
/// [`reconcile_stale_active_then_reactivate`] (#2453) before giving up — this
/// closes the up-to-60s window where a pane's `claude` process has already
/// exited but the periodic reap tick (or a racing `SessionEnd` hook) has not
/// yet flipped the record's `state` to `Stopped`; still 409 when the record is
/// genuinely not reconcilable (a live runtime, a different lifecycle state, or
/// a sibling pane in the same tmux session that is still active). The optional
/// `caller_pane_id` query param (#2789) lets an in-place-relaunch client name
/// its OWN pane so the reconcile does not count the requesting `tm` process
/// itself as a live runtime. 200 with the updated summary on success either way.
/// Test: `mark_reactivated_flips_stopped_to_active`,
/// `mark_reactivated_rejects_non_stopped` in `session_manager::reactivate_tests`;
/// `should_reconcile_stale_active_*` below.
pub async fn reactivate_managed_session(
    State(state): State<Arc<DaemonState>>,
    AxumPath(id_str): AxumPath<String>,
    Query(params): Query<ReactivateQuery>,
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
        Err(ManagedError::InvalidState(_, reason)) => match reconcile_stale_active_then_reactivate(
            &mgr,
            &id,
            params.caller_pane_id.as_deref(),
            params.pane_confirmed_dead,
        )
        .await
        {
            Some(record) => Json(record_to_summary(&record)).into_response(),
            None => (StatusCode::CONFLICT, reason).into_response(),
        },
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
/// session — EXCEPT the caller's own pane (`caller_pane_id`, #2789) — is idle
/// (a still-live pane — including a genuinely different, still-active sibling
/// window in the SAME tmux session, #2157 — keeps the existing 409 refusal,
/// preserving that safety boundary). `pane_confirmed_dead` forwards the CLI's
/// self-asserted claim (see [`ReactivateQuery::pane_confirmed_dead`] for the
/// trust-model discussion — it is accepted, not independently re-verified,
/// same as this daemon accepts any other client-originated state-transition
/// request) that it is inside the record's OWN pane and observed the agent
/// exit, so a caller matching that description reconciles even when the
/// tmux-pane probe would false-positive on that pane — bounded by the SAME
/// independent sibling-pane liveness check every other path here uses. On
/// confirmation, calls
/// [`SessionManager::mark_runtime_exited_stopped`] then
/// [`SessionManager::mark_reactivated`]; any failure at either step (tmux
/// discovery, pane listing, or either state transition) also yields `None` so
/// the caller falls back to the ordinary 409.
/// Test: I/O path (shells out to a real `tmux`); not unit-tested — mirrors the
/// rest of this binary's daemon-tmux-discovery call sites. The pure
/// classification it delegates to is `should_reconcile_stale_active_*` below.
async fn reconcile_stale_active_then_reactivate(
    mgr: &SessionManager,
    id: &ManagedSessionId,
    caller_pane_id: Option<&str>,
    pane_confirmed_dead: bool,
) -> Option<SessionRecord> {
    let record = mgr.get(id).await.ok()?;
    let panes = crate::daemon::tmux::TmuxDriver::discover()
        .ok()?
        .list_managed_panes()
        .ok()?;
    if !should_reconcile_stale_active(
        &record,
        &panes,
        caller_pane_id,
        pane_confirmed_dead,
        &ProcessTreeProbe,
    ) {
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
///
/// #2789: the caller's OWN pane is first neutralized to an idle shell via
/// [`neutralize_caller_pane`]. Without this, an in-place-relaunch request is
/// self-defeating: the requesting `tm` process is the foreground command of
/// the pane being relaunched, so tmux reports that pane as running `tm` (a
/// non-shell command, and its shell PID has `tm` as a live child) — which the
/// unmodified liveness check reads as "runtime still alive", refusing the
/// reconcile with a 409 and stranding the operator in a bare shell. Because a
/// pane cannot simultaneously run `tm` in the foreground AND a live interactive
/// `claude`, treating the caller's own pane as idle is SOUND, not merely a
/// trust assumption — while every OTHER (sibling) pane is checked unchanged, so
/// a genuinely live sibling window still blocks the reconcile.
/// #2794: `caller_pane_confirmed_dead` (from
/// [`ReactivateQuery::pane_confirmed_dead`]) is the CLI's self-asserted claim
/// — NOT independently verified by the daemon (no process/ancestry check) —
/// that it is this record's OWN pane and the agent has exited, accepted under
/// the same localhost/single-operator trust model this daemon already extends
/// to every other client-originated state transition (resume/stop/
/// decommission), and bounded by an INDEPENDENT sibling-pane liveness check
/// (below), not taken as a bare capability grant. `find_runtime_exited`
/// requires the record's tmux session to be PRESENT in `panes` AND every one
/// of its panes to read idle — a conservative rule that is correct for the
/// periodic reaper but leaves a residual gap for the in-place-relaunch caller:
/// its OWN pane can read busy in ways [`neutralize_caller_pane`] cannot always
/// repair (a racing `list-panes` omitting the pane entirely, or a not-yet-reaped
/// agent child the probe still counts). Bob's #2794 repro (a zombie-`Active`
/// record that only reconciled minutes later once the reaper independently
/// caught up) is exactly that gap. When the CLI asserts, via stable `pane_id`
/// (`bin/tm`'s `guided::pane_identity_confirmed`), that it is this record's own
/// pane and observed the agent exit, that assertion is trusted OVER the probe
/// for the caller's OWN pane: reconcile as long as no genuinely different
/// SIBLING pane keeps the session live (the #2157 boundary, independently
/// re-checked here regardless of the assertion). This never requires the
/// session to be "present", so the missing-pane race no longer strands the
/// operator, while the sibling check keeps a stale or mistaken assertion from
/// silently overriding a genuinely live different window.
///
/// What: `true` when `record.state == Active` AND either — (a)
/// `caller_pane_confirmed_dead` is set with a non-blank `caller_pane_id` and no
/// SIBLING pane in the record's tmux session is still live (over the
/// caller-neutralized list); or (b) the ordinary `find_runtime_exited` (scoped
/// to just this record, over the caller-neutralized pane list) selects it — its
/// tmux session has at least one present pane and NONE (other than the caller's
/// own) are still running an agent. `false` for every other state
/// (Provisioning/Errored/Decommissioned — those fall through to the ordinary
/// 409, matching `mark_reactivated`'s Stopped-only contract) and, on the
/// probe-only path (b), for a missing/still-live-sibling pane.
/// Test: `should_reconcile_stale_active_true_when_active_and_idle`,
/// `should_reconcile_stale_active_false_when_pane_still_live`,
/// `should_reconcile_stale_active_false_when_pane_missing`,
/// `should_reconcile_stale_active_false_when_not_active`,
/// `should_reconcile_stale_active_true_when_only_caller_pane_busy`,
/// `should_reconcile_stale_active_false_when_sibling_pane_live_despite_caller`,
/// `should_reconcile_stale_active_true_when_confirmed_dead_and_pane_missing`,
/// `should_reconcile_stale_active_true_when_confirmed_dead_and_caller_pane_busy`,
/// `should_reconcile_stale_active_false_when_confirmed_dead_but_sibling_live`,
/// `should_reconcile_stale_active_ignores_confirmed_dead_without_caller_pane`.
pub(crate) fn should_reconcile_stale_active(
    record: &SessionRecord,
    panes: &[PaneInfo],
    caller_pane_id: Option<&str>,
    caller_pane_confirmed_dead: bool,
    probe: &dyn ChildLivenessProbe,
) -> bool {
    if record.state != ManagedSessionState::Active {
        return false;
    }
    let neutralized = neutralize_caller_pane(panes, caller_pane_id);
    if caller_pane_confirmed_dead && caller_pane_id.is_some_and(|s| !s.is_empty()) {
        // Self-asserted proof-of-death path: the caller's own pane is trusted
        // as idle (already neutralized above) on the caller's say-so, accepted
        // under this daemon's localhost/single-operator trust model; reconcile
        // unless an INDEPENDENTLY checked SIBLING pane keeps the session live.
        // Deliberately does NOT gate on session presence, so a pane momentarily
        // missing from `list-panes` cannot block a caller asserting it is
        // inside the record's pane.
        return !session_has_live_pane(&record.tmux_name, &neutralized, probe);
    }
    !find_runtime_exited(std::slice::from_ref(record), &neutralized, probe).is_empty()
}

/// #2789: return `panes` with the caller's OWN pane rewritten to look like an
/// idle shell, so the liveness aggregation does not count the requesting `tm`
/// process against its own session.
///
/// Why: see [`should_reconcile_stale_active`]. The caller's pane is kept in the
/// list (its session must still count as "present" for `find_runtime_exited` to
/// consider it) but its command/PID are replaced so it reads as idle — as if
/// the pane had already dropped back to the bare shell it will become the
/// instant `tm` `exec`s `claude`. Matching is by the stable tmux `pane_id`
/// (never reused across panes), so exactly one pane can match.
/// What: when `caller_pane_id` is `None`/blank, or no pane matches it, returns
/// the panes unchanged (behaviorally identical to the pre-#2789 path). When a
/// pane matches, that entry's `pane_current_command` becomes a bare shell and
/// its `pane_pid` becomes `None` (so [`ChildLivenessProbe`] reports no live
/// child); all other panes are copied verbatim.
/// Test: exercised via `should_reconcile_stale_active_true_when_only_caller_pane_busy`
/// and `..._false_when_sibling_pane_live_despite_caller`.
fn neutralize_caller_pane(panes: &[PaneInfo], caller_pane_id: Option<&str>) -> Vec<PaneInfo> {
    let Some(caller) = caller_pane_id.filter(|s| !s.is_empty()) else {
        return panes.to_vec();
    };
    panes
        .iter()
        .map(|p| {
            if p.pane_id.as_deref() == Some(caller) {
                PaneInfo {
                    pane_current_command: "zsh".to_string(),
                    pane_pid: None,
                    ..p.clone()
                }
            } else {
                p.clone()
            }
        })
        .collect()
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
            pane_id: None,
        }
    }

    /// A pane carrying a stable tmux `pane_id` — needed to exercise the #2789
    /// caller-pane neutralization.
    fn pane_with_id(name: &str, cmd: &str, pane_id: &str) -> PaneInfo {
        PaneInfo {
            session_name: name.to_string(),
            pane_current_command: cmd.to_string(),
            pane_pid: Some(4242),
            pane_id: Some(pane_id.to_string()),
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
            None,
            false,
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
            None,
            false,
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
            None,
            false,
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
            None,
            false,
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
                !should_reconcile_stale_active(&record, &panes, None, false, &AlwaysIdleProbe),
                "state {state} must not be reconciled"
            );
        }
    }

    #[test]
    fn should_reconcile_stale_active_true_when_only_caller_pane_busy() {
        // #2789 core repro: bare `tm` typed in the just-vacated pane runs as
        // that pane's foreground command, so tmux reports it as running `tm`
        // (NOT an idle shell). Without the caller-pane exclusion this reads as
        // "runtime still alive" and 409s — the reported dead-end. Naming the
        // caller's own pane_id neutralizes exactly that one pane, so the
        // session reconciles.
        let record = active_record("tm-cto-01");
        let panes = vec![pane_with_id("tm-cto-01", "tm", "%3")];
        assert!(
            !should_reconcile_stale_active(&record, &panes, None, false, &AlwaysIdleProbe),
            "pre-#2789 behavior (no caller_pane_id): the caller's own `tm` pane \
             is still (wrongly) seen as live"
        );
        assert!(
            should_reconcile_stale_active(&record, &panes, Some("%3"), false, &AlwaysIdleProbe),
            "#2789: naming the caller's own pane must let its session reconcile"
        );
    }

    #[test]
    fn should_reconcile_stale_active_false_when_sibling_pane_live_despite_caller() {
        // #2789 genuine-conflict guard: neutralizing the caller's OWN pane must
        // NOT hide a genuinely live claude in a DIFFERENT pane of the same tmux
        // session (#2157). The sibling keeps the session live → 409 stands.
        let record = active_record("tm-cto-01");
        let panes = vec![
            pane_with_id("tm-cto-01", "tm", "%3"),     // caller's own pane
            pane_with_id("tm-cto-01", "claude", "%4"), // genuinely live sibling
        ];
        assert!(
            !should_reconcile_stale_active(&record, &panes, Some("%3"), false, &AlwaysIdleProbe),
            "a live sibling pane must still block reconcile even when the \
             caller's own pane is excluded"
        );
    }

    #[test]
    fn should_reconcile_stale_active_ignores_unmatched_caller_pane_id() {
        // A caller_pane_id that matches no pane in the record's session must
        // leave the liveness check unchanged — no accidental neutralization of
        // an unrelated pane.
        let record = active_record("tm-demo");
        let panes = vec![pane_with_id("tm-demo", "claude", "%1")];
        assert!(
            !should_reconcile_stale_active(&record, &panes, Some("%99"), false, &AlwaysIdleProbe),
            "an unmatched caller_pane_id must not force any pane idle"
        );
    }

    #[test]
    fn should_reconcile_stale_active_true_when_confirmed_dead_and_pane_missing() {
        // #2794 core repro: the caller has PROVEN (CLI-side pane_identity_
        // confirmed) it is inside this record's OWN pane and the agent exited,
        // but `list-panes` momentarily reports NO pane for the session (a race
        // the probe-only path treats as "session gone" → 409, the reported
        // dead-end). Proof-of-death must reconcile anyway — there is no sibling
        // to protect.
        let record = active_record("tm-mcp-services-01");
        assert!(
            !should_reconcile_stale_active(&record, &[], Some("%5"), false, &AlwaysIdleProbe),
            "without proof-of-death, a missing pane keeps the ordinary 409"
        );
        assert!(
            should_reconcile_stale_active(&record, &[], Some("%5"), true, &AlwaysIdleProbe),
            "#2794: a pane-confirmed-dead caller reconciles even when \
             list-panes omits its pane"
        );
    }

    #[test]
    fn should_reconcile_stale_active_true_when_confirmed_dead_and_caller_pane_busy() {
        // The exact zombie-Active shape: the record still reads Active and the
        // caller's own pane reports `tm` (its foreground command), which the
        // probe reads as live. Proof-of-death overrides the probe for the
        // caller's OWN pane.
        let record = active_record("tm-mcp-services-01");
        let panes = vec![pane_with_id("tm-mcp-services-01", "tm", "%5")];
        assert!(
            should_reconcile_stale_active(&record, &panes, Some("%5"), true, &AlwaysIdleProbe),
            "#2794: pane-confirmed-dead reconciles its own busy `tm` pane"
        );
    }

    #[test]
    fn should_reconcile_stale_active_false_when_confirmed_dead_but_sibling_live() {
        // Proof-of-death for the caller's OWN pane must NOT override a genuinely
        // live claude in a DIFFERENT window of the same tmux session (#2157).
        let record = active_record("tm-mcp-services-01");
        let panes = vec![
            pane_with_id("tm-mcp-services-01", "tm", "%5"), // caller's own pane
            pane_with_id("tm-mcp-services-01", "claude", "%6"), // live sibling
        ];
        assert!(
            !should_reconcile_stale_active(&record, &panes, Some("%5"), true, &AlwaysIdleProbe),
            "a live sibling window must still block reconcile despite proof-of-death"
        );
    }

    #[test]
    fn should_reconcile_stale_active_ignores_confirmed_dead_without_caller_pane() {
        // The proof-of-death shortcut requires a concrete caller_pane_id — the
        // flag alone (no pane id, or a blank one) must fall back to the ordinary
        // probe-based path, never a blanket reconcile.
        let record = active_record("tm-demo");
        let panes = vec![pane("tm-demo", "claude")];
        assert!(
            !should_reconcile_stale_active(&record, &panes, None, true, &AlwaysIdleProbe),
            "pane_confirmed_dead without a caller_pane_id must not reconcile a live pane"
        );
        assert!(
            !should_reconcile_stale_active(&record, &panes, Some(""), true, &AlwaysIdleProbe),
            "a blank caller_pane_id must not trigger the proof-of-death shortcut"
        );
    }
}
