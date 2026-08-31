//! Unit tests for managed-routes serializers.
//!
//! Why: isolating these tests into a `tests.rs` sibling keeps the production
//! file under the 500-SLOC cap while giving the test module the generous
//! 1500-SLOC budget.
//! What: focused assertions on `record_to_json` and `record_to_summary`.
//! Test: this file.

use std::path::PathBuf;

use chrono::Utc;

// #5040: every staleness-path entry point is imported in its explicit-base
// `_under` form, so no test here writes the process-global `$HOME`.
use super::summary::{
    checked_summaries_with_under, reconcile_against_tmux, reconcile_live_state,
    record_to_summary_checked_under, stale_assets_for_many_under,
};
use super::{record_to_json, record_to_summary};
use crate::session_manager::{
    InjectionStatus, ManagedError, ManagedSessionId, ManagedSessionState, ManagedTmuxDriver,
    SessionRecord,
};

/// A tmux driver whose `list_sessions` FAILS — used to prove the list handler's
/// reconciliation fails CLOSED (keeps persisted state) rather than treating a
/// probe error as "zero live sessions".
struct EnumErrTmux;
impl ManagedTmuxDriver for EnumErrTmux {
    fn create_session(&self, _n: &str, _w: &str) -> Result<(), ManagedError> {
        Ok(())
    }
    fn kill_session(&self, _n: &str) -> Result<(), ManagedError> {
        Ok(())
    }
    fn send_line(&self, _n: &str, _t: &str) -> Result<(), ManagedError> {
        Ok(())
    }
    fn capture(&self, _n: &str, _l: usize) -> Result<String, ManagedError> {
        Ok(String::new())
    }
    fn list_sessions(&self) -> Result<Vec<String>, ManagedError> {
        Err(ManagedError::TmuxUnavailable("enumeration failed".into()))
    }
}

/// A tmux driver whose pane-liveness answer is controllable per test — used
/// to prove `reconcile_live_state`'s pane-scoped liveness check (#3714): a
/// name being live is not enough, the record's OWN recorded pane must also
/// be confirmed present.
///
/// `pane_alive: Option<bool>` mirrors the tri-state `pane_exists_checked`
/// contract directly (#3714 review finding 2) — `Some(true)`/`Some(false)`
/// simulate a confirmed answer, `None` simulates a `list-panes` QUERY
/// FAILURE (distinct from a confirmed-absent pane), so tests can assert the
/// two are handled differently by the display-only consumer.
struct PaneAwareTmux {
    pane_alive: Option<bool>,
}
impl ManagedTmuxDriver for PaneAwareTmux {
    fn create_session(&self, _n: &str, _w: &str) -> Result<(), ManagedError> {
        Ok(())
    }
    fn kill_session(&self, _n: &str) -> Result<(), ManagedError> {
        Ok(())
    }
    fn send_line(&self, _n: &str, _t: &str) -> Result<(), ManagedError> {
        Ok(())
    }
    fn capture(&self, _n: &str, _l: usize) -> Result<String, ManagedError> {
        Ok(String::new())
    }
    fn list_sessions(&self) -> Result<Vec<String>, ManagedError> {
        Ok(Vec::new())
    }
    fn pane_exists(&self, _name: &str, _pane_id: &str) -> bool {
        self.pane_alive.unwrap_or(true)
    }
    fn pane_exists_checked(&self, _name: &str, _pane_id: &str) -> Option<bool> {
        self.pane_alive
    }
}

/// Build a minimal [`SessionRecord`] suitable for serialization tests.
///
/// `pub(super)` so the sibling `staleness_bench_tests` benchmark module can
/// build its synthetic fleet from the identical record shape the unit tests
/// use, rather than maintaining a second, drift-prone copy.
pub(super) fn make_record(source_id: Option<&str>) -> SessionRecord {
    SessionRecord {
        id: ManagedSessionId::new(),
        tmux_name: "tmpm-test-session".into(),
        cwd: PathBuf::from("/tmp/test"),
        task: "test task".into(),
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
        source_id: source_id.map(String::from),
        claude_session_id: None,
        scrollback_path: None,
        last_cwd: None,
        deliverable_id: None,
        pane_id: None,
        injection_status: Default::default(),
        worktree_owner: None,
        terminal_at: None,
        stop_cause: None,
    }
}

/// A `Stopped` record whose tmux session is LIVE must reconcile to `active`
/// (and a gone one stays `stopped`) so `tm ls` never mislabels a running
/// session — the core of the "(stopped)" reconciliation bug fix.
#[test]
fn reconcile_live_state_flips_stopped_to_active_when_alive() {
    use std::collections::HashSet;
    let mut rec = make_record(None);
    rec.state = ManagedSessionState::Stopped;
    rec.tmux_name = "tm-worker-01".into();
    let records = vec![rec];
    let mut summaries: Vec<_> = records.iter().map(record_to_summary).collect();
    assert_eq!(summaries[0].state, "stopped", "persisted state is stopped");

    // Live + attached → reconciles to active, attached flag set.
    let live: HashSet<String> = ["tm-worker-01".to_string()].into_iter().collect();
    let attached: HashSet<String> = ["tm-worker-01".to_string()].into_iter().collect();
    // `pane_id: None` (legacy record) — falls back to the name-only check, so
    // the driver's own `pane_exists` answer is irrelevant here.
    reconcile_live_state(&EnumErrTmux, &mut summaries, &records, &live, &attached);
    assert_eq!(summaries[0].state, "active", "live tmux → active");
    assert!(summaries[0].attached, "attached client → attached flag");

    // No live tmux → stays stopped, not attached.
    let mut summaries2: Vec<_> = records.iter().map(record_to_summary).collect();
    reconcile_live_state(
        &EnumErrTmux,
        &mut summaries2,
        &records,
        &HashSet::new(),
        &HashSet::new(),
    );
    assert_eq!(summaries2[0].state, "stopped", "no tmux → stopped");
    assert!(!summaries2[0].attached);
}

/// #3531 core regression guard: `reconcile_live_state` must NEVER touch
/// `persisted_state`, even while it flips the DISPLAYED `state` — otherwise a
/// zombie (`Active` record, tmux gone) would again read identically to a
/// genuinely `Stopped` one, exactly the misclassification that made the CLI's
/// resume path 409 instead of auto-reconciling.
#[test]
fn reconcile_live_state_leaves_persisted_state_untouched() {
    use std::collections::HashSet;
    let mut rec = make_record(None);
    rec.state = ManagedSessionState::Active;
    rec.tmux_name = "tm-zombie-01".into();
    let records = vec![rec];
    let mut summaries: Vec<_> = records.iter().map(record_to_summary).collect();
    assert_eq!(summaries[0].state, "active");
    assert_eq!(summaries[0].persisted_state, "active");

    // Tmux is gone — DISPLAYED state flips to "stopped", but persisted_state
    // must keep reporting the TRUE record state ("active") so the CLI's
    // resume-decision logic can still tell this apart from a genuinely
    // stopped session.
    reconcile_live_state(
        &EnumErrTmux,
        &mut summaries,
        &records,
        &HashSet::new(),
        &HashSet::new(),
    );
    assert_eq!(
        summaries[0].state, "stopped",
        "display state still reconciles to stopped when tmux is gone"
    );
    assert_eq!(
        summaries[0].persisted_state, "active",
        "persisted_state must survive display reconciliation unchanged (#3531)"
    );
}

/// `reconcile_against_tmux` FAILS CLOSED on a tmux enumeration error: an Active
/// record must keep its `active` state, NOT be reconciled to `stopped` (which
/// would offer a fleet-wide destructive restart on a transient tmux hiccup).
#[test]
fn reconcile_against_tmux_fails_closed_on_enumeration_error() {
    let mut rec = make_record(None);
    rec.state = ManagedSessionState::Active;
    rec.tmux_name = "tm-live-1".into();
    let records = vec![rec];
    let mut summaries: Vec<_> = records.iter().map(record_to_summary).collect();
    assert_eq!(summaries[0].state, "active");

    // The driver's enumeration errors — reconciliation must be SKIPPED entirely.
    reconcile_against_tmux(&EnumErrTmux, &mut summaries, &records);
    assert_eq!(
        summaries[0].state, "active",
        "a tmux enumeration error must NOT flip a live record to stopped"
    );
    assert!(
        !summaries[0].attached,
        "attached stays at its default on a probe error"
    );
}

/// Terminal/non-transient states are NEVER flipped by the liveness probe — a
/// live tmux name must not resurrect a deleted/decommissioned label, and
/// errored/provisioning carry information a bare probe would erase.
#[test]
fn reconcile_live_state_leaves_terminal_states() {
    use std::collections::HashSet;
    let live: HashSet<String> = ["tm-x".to_string()].into_iter().collect();
    let empty = HashSet::new();
    for state in [
        ManagedSessionState::Deleted,
        ManagedSessionState::Decommissioned,
        ManagedSessionState::Errored,
        ManagedSessionState::Provisioning,
    ] {
        let mut rec = make_record(None);
        rec.state = state.clone();
        rec.tmux_name = "tm-x".into();
        let records = vec![rec];
        let mut summaries: Vec<_> = records.iter().map(record_to_summary).collect();
        let before = summaries[0].state.clone();
        reconcile_live_state(&EnumErrTmux, &mut summaries, &records, &live, &empty);
        assert_eq!(
            summaries[0].state, before,
            "{state:?} must not be reconciled by liveness"
        );
    }
}

/// #3714 core fix: a NAME being live is not enough — when the record carries
/// a `pane_id`, its OWN pane must be confirmed present in that named session
/// before the record reads `"active"`. Reproduces the reported
/// `state: "active"` / `persisted_state: "stopped"` contradiction: the record
/// is persisted `Stopped`, its NAME is in the live set (an unrelated
/// duplicate-name session), but its own recorded pane is confirmed gone.
#[test]
fn reconcile_live_state_prefers_pane_scoped_liveness_over_name_membership() {
    use std::collections::HashSet;
    let mut rec = make_record(None);
    rec.state = ManagedSessionState::Stopped;
    rec.tmux_name = "tm-tagents".into();
    rec.pane_id = Some("%3".to_string());
    let records = vec![rec];
    let live: HashSet<String> = ["tm-tagents".to_string()].into_iter().collect();
    let empty = HashSet::new();

    // The NAME is live (an unrelated session happens to share it), but this
    // record's OWN pane (%3) is confirmed gone — must stay "stopped", not
    // resurrect to "active".
    let mut summaries: Vec<_> = records.iter().map(record_to_summary).collect();
    reconcile_live_state(
        &PaneAwareTmux {
            pane_alive: Some(false),
        },
        &mut summaries,
        &records,
        &live,
        &empty,
    );
    assert_eq!(
        summaries[0].state, "stopped",
        "a live NAME must not resurrect the record when its OWN pane is confirmed gone"
    );
    assert_eq!(
        summaries[0].persisted_state, "stopped",
        "persisted_state must never disagree with the (correctly reconciled) display state here"
    );

    // The same record, but its OWN pane IS confirmed present this time —
    // must reconcile to "active" exactly as before #3714.
    let mut summaries2: Vec<_> = records.iter().map(record_to_summary).collect();
    reconcile_live_state(
        &PaneAwareTmux {
            pane_alive: Some(true),
        },
        &mut summaries2,
        &records,
        &live,
        &empty,
    );
    assert_eq!(
        summaries2[0].state, "active",
        "a live NAME whose own pane is confirmed present must still reconcile to active"
    );
}

/// A legacy record with no captured `pane_id` (pre-#2453) has no stronger
/// signal available — it must keep the pre-#3714 name-only reconciliation
/// rather than being newly refused liveness it cannot prove.
#[test]
fn reconcile_live_state_legacy_record_without_pane_id_uses_name_only() {
    use std::collections::HashSet;
    let mut rec = make_record(None);
    rec.state = ManagedSessionState::Stopped;
    rec.tmux_name = "tm-legacy-01".into();
    rec.pane_id = None;
    let records = vec![rec];
    let live: HashSet<String> = ["tm-legacy-01".to_string()].into_iter().collect();
    let empty = HashSet::new();

    let mut summaries: Vec<_> = records.iter().map(record_to_summary).collect();
    // Even a driver that would report the pane as gone must not matter here —
    // a legacy record has no `pane_id` to probe in the first place.
    reconcile_live_state(
        &PaneAwareTmux {
            pane_alive: Some(false),
        },
        &mut summaries,
        &records,
        &live,
        &empty,
    );
    assert_eq!(
        summaries[0].state, "active",
        "a legacy record with no pane_id falls back to the name-only check"
    );
}

/// #3714 review finding 2 (HIGH): a `list-panes` QUERY FAILURE ("could not
/// determine") must NOT be treated as "confirmed absent" for DISPLAY
/// reconciliation — that would flip a genuinely live, `Active`-persisted
/// record's shown `state` to `"stopped"` on a transient tmux hiccup,
/// reproducing the exact `state`/`persisted_state` disagreement #3714
/// exists to fix, just from a different trigger. Distinguishing the two
/// (via `pane_exists_checked`'s tri-state) is what `pane_exists`'s bare
/// `bool` — correct for the MUTATION guard in `SessionManager::rename`,
/// unaffected by this test — cannot do.
#[test]
fn reconcile_live_state_pane_query_error_does_not_flip_live_record_to_stopped() {
    use std::collections::HashSet;
    let mut rec = make_record(None);
    rec.state = ManagedSessionState::Active;
    rec.tmux_name = "tm-flaky-01".into();
    rec.pane_id = Some("%7".to_string());
    let records = vec![rec];
    let live: HashSet<String> = ["tm-flaky-01".to_string()].into_iter().collect();
    let empty = HashSet::new();

    let mut summaries: Vec<_> = records.iter().map(record_to_summary).collect();
    assert_eq!(summaries[0].state, "active");
    assert_eq!(summaries[0].persisted_state, "active");

    // `pane_alive: None` simulates a `list-panes` query FAILURE, not a
    // confirmed-absent pane.
    reconcile_live_state(
        &PaneAwareTmux { pane_alive: None },
        &mut summaries,
        &records,
        &live,
        &empty,
    );
    assert_eq!(
        summaries[0].state, "active",
        "a transient pane-query failure must fall back to the name-only signal, \
         never assert the pane is confirmed gone"
    );
    assert_eq!(
        summaries[0].persisted_state, "active",
        "state and persisted_state must still agree — no contradiction introduced"
    );
}

/// Why: both the MCP path (`record_to_json`) and the HTTP path
/// (`record_to_summary`) must expose `source_id` so callers on either
/// transport can filter or reconnect by project identity (#1733).
/// What: asserts that a record with `source_id = Some("owner/repo")` is
/// reflected by both serializers, and that a record with `source_id = None`
/// serializes as JSON `null` from `record_to_json` and as `None` from
/// `record_to_summary`.
/// Test: this test.
#[test]
fn serializers_include_source_id() {
    // ── Case 1: source_id is set ──────────────────────────────────────────
    let r = make_record(Some("owner/repo"));

    // record_to_json (MCP path) must include source_id as a string.
    let json = record_to_json(&r);
    assert_eq!(
        json["source_id"].as_str(),
        Some("owner/repo"),
        "record_to_json must serialize source_id when present"
    );

    // record_to_summary (HTTP path) must include source_id.
    let summary = record_to_summary(&r);
    assert_eq!(
        summary.source_id.as_deref(),
        Some("owner/repo"),
        "record_to_summary must include source_id when present"
    );

    // ── Case 2: source_id is None ─────────────────────────────────────────
    let r_none = make_record(None);

    // record_to_json must serialize source_id as JSON null, not omit it.
    let json_none = record_to_json(&r_none);
    assert!(
        json_none["source_id"].is_null(),
        "record_to_json must serialize source_id as null when absent, got {:?}",
        json_none["source_id"]
    );

    // record_to_summary must carry None.
    let summary_none = record_to_summary(&r_none);
    assert_eq!(
        summary_none.source_id, None,
        "record_to_summary must carry None source_id when absent"
    );
}

/// Why: `record_to_json` (MCP path) and `record_to_summary` (HTTP path) must
/// both surface `deliverable_id` (DOC-35 §10.6, #2379) so `tm sessions ls`/
/// `status` and the MCP tools can show which Deliverable a session is bound
/// to, exactly as they already do for `source_id`.
/// What: asserts a bound session serializes the stringified id on both paths,
/// and an unbound session serializes JSON `null` / `None` respectively.
/// Test: this test.
#[test]
fn serializers_include_deliverable_id() {
    use crate::deliverable::DeliverableId;

    // ── Case 1: deliverable_id is set ─────────────────────────────────────
    let mut r = make_record(None);
    let did = DeliverableId::new();
    r.deliverable_id = Some(did);

    let json = record_to_json(&r);
    assert_eq!(
        json["deliverable_id"].as_str(),
        Some(did.to_string().as_str()),
        "record_to_json must serialize deliverable_id when present"
    );
    let summary = record_to_summary(&r);
    assert_eq!(
        summary.deliverable_id.as_deref(),
        Some(did.to_string().as_str()),
        "record_to_summary must include deliverable_id when present"
    );

    // ── Case 2: deliverable_id is None (the common, unbound case) ─────────
    let r_none = make_record(None);
    let json_none = record_to_json(&r_none);
    assert!(
        json_none["deliverable_id"].is_null(),
        "record_to_json must serialize deliverable_id as null when absent"
    );
    let summary_none = record_to_summary(&r_none);
    assert_eq!(
        summary_none.deliverable_id, None,
        "record_to_summary must carry None deliverable_id when absent"
    );
}

/// Verify that `DecommissionResponse.workspace_removed` correctly reflects
/// workspace ownership rather than being inferred from the post-call filesystem.
///
/// Why: the TOCTOU issue (#1788 review) means a post-hoc `!p.exists()` check
/// gives `workspace_removed=true` even for workspaces that were ALREADY absent
/// before decommission ran. The response struct must be populated from the value
/// returned by `decommission_with_root` (which tracks whether `remove_dir_all`
/// actually ran), not from a filesystem re-check.
/// What: validates the `DecommissionResponse` serde contract — `workspace_removed`
/// is a bool present in the JSON, and `workspace_path_was` is an optional string
/// present only for owned sessions.
/// Test: this test; the runtime path is covered by
/// `manager_decommission_removes_workspace` (owned=true → workspace_removed=true)
/// and `manager_decommission_unowned_skips_deletion` (owned=false → false).
#[test]
fn decommission_workspace_removed_reflects_ownership() {
    use super::{DecommissionResponse, SessionSummary};

    // ── owned session: workspace_removed = true, workspace_path_was = Some ──
    let owned_summary = SessionSummary {
        id: "abc-123".into(),
        name: "tmpm-test".into(),
        state: "decommissioned".into(),
        persisted_state: "decommissioned".into(),
        workspace_path: None, // cleared in tombstone
        repo_url: None,
        branch: None,
        created_at: "2025-06-28T00:00:00Z".into(),
        last_activity_at: None,
        pending_decision: None,
        proposed_default: None,
        source_id: None,
        task: None,
        cwd: None,
        claude_session_id: None,
        deliverable_id: None,
        pane_id: None,
        injection_status: None,
        unresumable: false,
        stale_assets: false,
        stale_assets_unchecked: false,
        attached: false,
        slot: 0,
        deleted: false,
    };
    let resp_owned = DecommissionResponse {
        summary: owned_summary,
        workspace_removed: true,
        workspace_path_was: Some("/workspaces/trusty-mpm/session-abc".into()),
    };
    let json = serde_json::to_value(&resp_owned).unwrap();
    assert_eq!(json["workspace_removed"], true, "owned: must be true");
    assert_eq!(
        json["workspace_path_was"].as_str(),
        Some("/workspaces/trusty-mpm/session-abc"),
        "owned: workspace_path_was must be present"
    );

    // ── unowned session: workspace_removed = false, workspace_path_was absent ──
    let unowned_summary = SessionSummary {
        id: "xyz-456".into(),
        name: "tmpm-adopted".into(),
        state: "decommissioned".into(),
        persisted_state: "decommissioned".into(),
        workspace_path: None,
        repo_url: None,
        branch: None,
        created_at: "2025-06-28T00:00:00Z".into(),
        last_activity_at: None,
        pending_decision: None,
        proposed_default: None,
        source_id: None,
        task: None,
        cwd: None,
        claude_session_id: None,
        deliverable_id: None,
        pane_id: None,
        injection_status: None,
        unresumable: false,
        stale_assets: false,
        stale_assets_unchecked: false,
        attached: false,
        slot: 0,
        deleted: false,
    };
    let resp_unowned = DecommissionResponse {
        summary: unowned_summary,
        workspace_removed: false,
        workspace_path_was: None,
    };
    let json2 = serde_json::to_value(&resp_unowned).unwrap();
    assert_eq!(json2["workspace_removed"], false, "unowned: must be false");
    assert!(
        json2.get("workspace_path_was").is_none(),
        "unowned: workspace_path_was must be absent (skip_serializing_if = None)"
    );
}

/// Why (#2364): a session injection was never attempted for must omit the
/// `injection_status` key entirely on both wire paths, rather than emitting
/// the literal string `"not_applicable"` — the field should stay silent for
/// the (common) case where turnkey injection never applies to a session.
/// What: asserts `record_to_json` serializes `injection_status` as JSON
/// `null` and `record_to_summary` carries `None` when the record's
/// `injection_status` is the `NotApplicable` default.
/// Test: this test.
#[test]
fn injection_status_wire_omits_not_applicable() {
    let r = make_record(None);
    assert_eq!(r.injection_status, InjectionStatus::NotApplicable);

    let json = record_to_json(&r);
    assert!(
        json["injection_status"].is_null(),
        "record_to_json must serialize NotApplicable as null, got {:?}",
        json["injection_status"]
    );

    let summary = record_to_summary(&r);
    assert_eq!(
        summary.injection_status, None,
        "record_to_summary must carry None for NotApplicable"
    );
}

/// Why (#2364): callers polling delivery status need every non-trivial
/// [`InjectionStatus`] variant to round-trip through BOTH wire paths as its
/// snake_case string form, so `tm session info`/`tm sessions ls` can surface
/// exactly `pending`/`success`/`failed_timeout`/`failed_session_died`.
/// What: for each non-`NotApplicable` variant, asserts `record_to_json`
/// serializes the matching string and `record_to_summary` carries
/// `Some(<string>)`.
/// Test: this test.
#[test]
fn injection_status_wire_stringifies_other_variants() {
    let cases = [
        (InjectionStatus::Pending, "pending"),
        (InjectionStatus::Success, "success"),
        (InjectionStatus::FailedTimeout, "failed_timeout"),
        (InjectionStatus::FailedSessionDied, "failed_session_died"),
    ];
    for (status, expected) in cases {
        let mut r = make_record(None);
        r.injection_status = status;

        let json = record_to_json(&r);
        assert_eq!(
            json["injection_status"].as_str(),
            Some(expected),
            "record_to_json must serialize {status:?} as {expected:?}"
        );

        let summary = record_to_summary(&r);
        assert_eq!(
            summary.injection_status.as_deref(),
            Some(expected),
            "record_to_summary must carry {expected:?} for {status:?}"
        );
    }
}

// `unresumable_response`'s header-tagging behavior is unit-tested beside its
// definition in `resume_error.rs` (#2577 review) — see
// `resume_error::tests::unresumable_response_tags_reason_header_per_failure_class`.

/// A throwaway framework base for every test that reaches the staleness path
/// (#5040).
///
/// Why: this module used to point the PROCESS-GLOBAL `$HOME` at a temp dir and
/// restore it from a `Drop` guard. The guard IS the race — between the write and
/// the restore, any concurrently-running test in the same binary that reads live
/// `$HOME` sees the temp dir. `#[serial_test::serial]` does not close that:
/// it orders `#[serial]` tests against each other, not against the rest of the
/// binary, and under `cargo nextest`'s per-test processes it orders nothing at
/// all. Three readers caught the tempdir value on roughly one local
/// `cargo test -p trusty-mpm` run in three —
/// `core::session_assets::tests::session_plan_under_matches_session_plan_at_home`,
/// `managed_prune_worktrees_parity_defaults_to_a_preview`, and
/// `managed_reconcile_worktrees_parity`. Every staleness entry point now takes
/// the base as an argument, so a test using this fixture reads no environment
/// variable, writes none, and needs no serial guard.
/// What: a bare `TempDir` used as BOTH the framework base (`<dir>/.trusty-mpm`)
/// and the parent of each fixture workspace — the same two roles the redirected
/// `$HOME` played. `pub(super)` so the sibling `staleness_bench_tests` module
/// isolates its fixture the same way.
/// Test: used by every `checked_summaries_*`, `stale_assets_for_many_*`,
/// `staleness_inputs_*`, and `record_to_summary_checked_*` case below.
pub(super) fn fake_base() -> tempfile::TempDir {
    tempfile::TempDir::new().unwrap()
}

/// #2595 review (PR #2652, MEDIUM finding 4): [`checked_summaries`] fans its
/// per-record `unresumable` probes out concurrently via `JoinSet`, which
/// yields completed tasks in COMPLETION order, not submission order — this
/// pins that the returned `Vec<SessionSummary>` is nonetheless rebuilt in the
/// SAME order as the input `records` slice, and that only the genuinely-dead
/// record among a live/dead/healthy-stopped trio gets flagged.
///
/// #5040: base injected via [`fake_base`], so no `$HOME` write and no
/// `#[serial]`.
#[tokio::test]
async fn checked_summaries_preserves_input_order_and_flags_only_dead_sessions() {
    let base = fake_base();
    // r0: Active — the state gate alone skips the filesystem probe.
    let mut r0 = make_record(None);
    r0.state = ManagedSessionState::Active;

    // r1: Stopped with NO existing workdir candidate — must end up dead.
    let mut r1 = make_record(None);
    r1.state = ManagedSessionState::Stopped;
    r1.cwd = PathBuf::from("/nonexistent/checked-summaries-order-r1");
    r1.workspace_path = None;
    r1.last_cwd = None;

    // r2: Errored but its `cwd` genuinely exists — must stay resumable.
    let real_dir = tempfile::TempDir::new().expect("tempdir for r2");
    let mut r2 = make_record(None);
    r2.state = ManagedSessionState::Errored;
    r2.cwd = real_dir.path().to_path_buf();
    r2.workspace_path = None;
    r2.last_cwd = None;

    let records = vec![r0.clone(), r1.clone(), r2.clone()];
    let summaries = checked_summaries_with_under(&records, true, base.path().to_path_buf()).await;

    assert_eq!(summaries.len(), 3, "one summary per input record");
    // Order must match the input slice exactly, regardless of which probe
    // task happened to finish first.
    assert_eq!(
        summaries[0].id,
        r0.id.to_string(),
        "r0 must stay at index 0"
    );
    assert_eq!(
        summaries[1].id,
        r1.id.to_string(),
        "r1 must stay at index 1"
    );
    assert_eq!(
        summaries[2].id,
        r2.id.to_string(),
        "r2 must stay at index 2"
    );

    assert!(
        !summaries[0].unresumable,
        "an Active session must never be flagged, regardless of probe fan-out"
    );
    assert!(
        summaries[1].unresumable,
        "a stopped session with no existing workdir candidate must be flagged dead"
    );
    assert!(
        !summaries[2].unresumable,
        "an errored session with a REAL existing cwd must stay resumable"
    );
}

/// Issue #2444: [`checked_summaries`]'s asset-staleness probe must fire only
/// for the states where it is meaningful (`Active`/`Stopped`/`Errored`) and
/// must correctly flag a session whose deployed agent has drifted from the
/// (temp-base) bundled source, while leaving a `Provisioning` session (no
/// deploy has happened yet — every artifact would spuriously read "new") at
/// its `false` default regardless of workspace content.
///
/// #5040: base injected via [`fake_base`], so no `$HOME` write and no
/// `#[serial]`.
#[tokio::test]
async fn checked_summaries_flags_stale_assets_only_for_relevant_states() {
    let base = fake_base();
    let fw = crate::core::paths::FrameworkPaths::under(base.path());
    let bundled = fw.agent_source_dir();
    std::fs::create_dir_all(&bundled).unwrap();
    std::fs::write(bundled.join("rust-engineer.md"), "v1").unwrap();

    // Deploy into an Active session's workspace, then drift the catalog.
    let workspace = base.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let session_fw =
        crate::core::paths::FrameworkPaths::for_managed_workspace_under(base.path(), &workspace);
    crate::core::agent_deployer::deploy_agents_filtered(
        &bundled,
        &session_fw.agent_deploy_dir(),
        |_| true,
    )
    .unwrap();
    std::fs::write(bundled.join("rust-engineer.md"), "v2 — catalog moved").unwrap();

    let mut active = make_record(None);
    active.state = ManagedSessionState::Active;
    active.workspace_path = Some(workspace);

    // Provisioning: no deploy has happened in this workspace at all.
    let provisioning_ws = base.path().join("never-deployed");
    std::fs::create_dir_all(&provisioning_ws).unwrap();
    let mut provisioning = make_record(None);
    provisioning.state = ManagedSessionState::Provisioning;
    provisioning.workspace_path = Some(provisioning_ws);

    let records = vec![active, provisioning];
    let summaries = checked_summaries_with_under(&records, true, base.path().to_path_buf()).await;

    assert!(
        summaries[0].stale_assets,
        "an Active session whose deployed agent drifted from the bundled \
         source must be flagged stale_assets"
    );
    assert!(
        !summaries[1].stale_assets,
        "a Provisioning session must never be probed — it has not deployed yet"
    );
    assert!(
        !summaries[0].stale_assets_unchecked && !summaries[1].stale_assets_unchecked,
        "neither an Active (probed) nor a Provisioning (nothing to check) row \
         may claim an UNDETERMINED asset verdict"
    );
}

/// Deploy `rust-engineer.md` into `workspace`'s `.claude/agents` from the
/// bundled source under `base`, then drift the catalog underneath it — leaving
/// that workspace GENUINELY stale relative to the catalog.
///
/// Why: three staleness tests below need the identical "deployed at v1,
/// catalog moved to v2" setup; sharing it keeps them asserting about the
/// PROBE rather than re-deriving the fixture, and guarantees the stopped-vs-
/// on-demand pair below compare the exact same on-disk condition.
///
/// #5040: `base` is the caller's [`fake_base`] temp dir, so the fixture writes
/// into the same tree the probe is told to read — no `$HOME` redirect.
fn deploy_then_drift_catalog(base: &std::path::Path, workspace: &std::path::Path) {
    let fw = crate::core::paths::FrameworkPaths::under(base);
    let bundled = fw.agent_source_dir();
    std::fs::create_dir_all(&bundled).unwrap();
    std::fs::write(bundled.join("rust-engineer.md"), "v1").unwrap();
    std::fs::create_dir_all(workspace).unwrap();
    let session_fw =
        crate::core::paths::FrameworkPaths::for_managed_workspace_under(base, workspace);
    crate::core::agent_deployer::deploy_agents_filtered(
        &bundled,
        &session_fw.agent_deploy_dir(),
        |_| true,
    )
    .unwrap();
    std::fs::write(bundled.join("rust-engineer.md"), "v2 — catalog moved").unwrap();
}

/// Issue #4322: the FLEET LIST path must not probe `Stopped` sessions — that
/// probe is ~95 filesystem reads per session and dominated cold `tm ls`
/// latency, while telling the operator nothing actionable until resume.
///
/// This pins BOTH halves of the contract, because either alone would be a
/// silent regression:
///   1. a genuinely stale STOPPED session is NOT flagged `stale_assets` by
///      `checked_summaries` (proving the probe really was skipped — this
///      assertion FAILS if `probe_staleness_in_list` is reverted to include
///      `Stopped`), and
///   2. that row carries `stale_assets_unchecked`, so its `stale_assets:
///      false` can never be read as a "checked, and fresh" verdict.
/// The companion `record_to_summary_checked_still_flags_stale_stopped_session`
/// proves the SIGNAL itself was not deleted — the same record, fetched
/// individually (the resume path), still reports stale.
///
/// #5040: base injected via [`fake_base`], so no `$HOME` write and no
/// `#[serial]`.
#[tokio::test]
async fn checked_summaries_does_not_probe_stopped_sessions() {
    let base = fake_base();
    let workspace = base.path().join("stopped-drifted");
    deploy_then_drift_catalog(base.path(), &workspace);

    let mut stopped = make_record(None);
    stopped.state = ManagedSessionState::Stopped;
    stopped.workspace_path = Some(workspace);

    let summaries = checked_summaries_with_under(
        std::slice::from_ref(&stopped),
        true,
        base.path().to_path_buf(),
    )
    .await;

    assert!(
        !summaries[0].stale_assets,
        "the fleet list must NOT probe a Stopped session — a `true` here means \
         the per-session filesystem probe ran anyway (#4322 regression)"
    );
    assert!(
        summaries[0].stale_assets_unchecked,
        "an unprobed Stopped row must advertise that its asset verdict is \
         UNDETERMINED — absence of `[stale-assets]` must never read as 'fresh'"
    );
}

/// Issue #4322 anti-regression pin: parallelizing the staleness fan-out must
/// NOT reinstate the per-session catalog recompose #2444's review removed.
///
/// The guard is structural rather than a call counter: `staleness_inputs`
/// resolves the SHARED catalog half sequentially and hands each session an
/// `Arc` to it, and the spawned tasks only ever borrow that `Arc`. So proving
/// that every session resolving the same `(agent_source, skill_source)` pair
/// receives a POINTER-EQUAL handle proves there is exactly one compute per
/// group — and that no second one exists for a spawned task to perform. Move
/// `CatalogHashes::compute` inside the fan-out and this fails immediately.
///
/// #5040: takes its framework base from [`fake_base`] rather than redirecting
/// `$HOME`, so it reads and writes no process-global state and carries no
/// `#[serial]`.
#[tokio::test]
async fn staleness_inputs_computes_one_catalog_per_source_pair_shared_by_arc() {
    let base = fake_base();
    let records: Vec<SessionRecord> = (0..3)
        .map(|i| {
            let ws = base.path().join(format!("shared-catalog-{i}"));
            std::fs::create_dir_all(&ws).unwrap();
            let mut r = make_record(None);
            r.state = ManagedSessionState::Active;
            r.workspace_path = Some(ws);
            r
        })
        .collect();

    let inputs = super::summary::staleness_inputs_under(records, base.path());

    assert_eq!(inputs.len(), 3, "one input per record, in input order");
    assert!(
        std::sync::Arc::ptr_eq(&inputs[0].3, &inputs[1].3)
            && std::sync::Arc::ptr_eq(&inputs[0].3, &inputs[2].3),
        "every session resolving the SAME catalog source pair must share ONE \
         computed CatalogHashes — a distinct Arc per session means the catalog \
         was recomposed per session (#2444 regression)"
    );
}

/// Issue #4326 review, HIGH (empirically proven): the pin above only ever
/// called [`super::summary::staleness_inputs_under`] directly — it never exercised
/// [`stale_assets_for_many_under`], the function `checked_summaries` (and
/// therefore `tm ls`) actually calls. The critic proved this gap by moving
/// `CatalogHashes::compute` INSIDE `stale_assets_for_many_under`'s per-session
/// `JoinSet::spawn_blocking` fan-out — reinstating the exact #2444 per-session
/// recompose — and every existing test, including the pin above, stayed
/// green.
///
/// Why this test closes the gap: it calls `stale_assets_for_many` itself (the
/// real hot path) with three records that all resolve to the SAME
/// `(agent_source, skill_source)` pair (the shared default, anchored at this
/// test's own [`fake_base`] temp dir), and asserts via
/// [`crate::core::update_check::compute_calls_for`] — a call LOG keyed by the
/// exact catalog paths, not a bare counter, so unrelated tests reaching
/// `CatalogHashes::compute` through their own unrelated temp paths can never
/// pollute this assertion — that `compute` ran EXACTLY ONCE for that pair.
/// Reinstating the per-session compose inside the fan-out (the critic's
/// mutation) makes this assert `3`, not `1`, and fail.
/// What: resets the log, resolves this test's own catalog source pair via
/// `FrameworkPaths::under(base)` (identical resolution `staleness_inputs_under`
/// uses internally), runs the real fan-out, and asserts one compute for that
/// pair.
/// Test: this function; mutation-verified per the PR report (temporarily
/// reinstating the per-task compute reproduces the critic's failure, then
/// reverted).
///
/// #5040: base injected via [`fake_base`], so no `$HOME` redirect and no
/// `#[serial]`.
#[tokio::test]
async fn stale_assets_for_many_computes_catalog_exactly_once_per_source_pair() {
    let base = fake_base();
    // #5040: no `reset_compute_call_log()` — the log is process-global, so a
    // clear from one test erases another's entries. `compute_calls_for` filters
    // to the source pair built under THIS test's temp base, which is zero
    // before the fan-out below by construction.

    let fw = crate::core::paths::FrameworkPaths::under(base.path());
    let agent_source = fw.agent_source_dir();
    let skill_source = fw.skill_source_dir();

    let records: Vec<SessionRecord> = (0..3)
        .map(|i| {
            let ws = base.path().join(format!("hot-path-shared-catalog-{i}"));
            std::fs::create_dir_all(&ws).unwrap();
            let mut r = make_record(None);
            r.state = ManagedSessionState::Active;
            r.workspace_path = Some(ws);
            r
        })
        .collect();

    let result = stale_assets_for_many_under(records, base.path().to_path_buf()).await;

    assert_eq!(result.len(), 3, "every record gets a result");
    assert_eq!(
        crate::core::update_check::compute_calls_for(&agent_source, &skill_source),
        1,
        "three sessions sharing one (agent_source, skill_source) pair must \
         share exactly ONE CatalogHashes::compute — more than one means the \
         per-session recompose #2444's review removed was reinstated inside \
         the fan-out (#4326 review HIGH)"
    );
}

/// Issue #4322 correctness gate: skipping the probe on the LIST path must not
/// delete the SIGNAL. The single-session fetch (`GET …/managed/{id}`, which
/// `tm session resume` reads — the exact moment a stopped session's drift
/// becomes actionable) must still flag the very same genuinely-stale STOPPED
/// record its list row leaves undetermined.
///
/// #5040: base injected via [`fake_base`], so no `$HOME` write and no
/// `#[serial]`.
#[tokio::test]
async fn record_to_summary_checked_still_flags_stale_stopped_session() {
    let base = fake_base();
    let workspace = base.path().join("stopped-drifted-on-demand");
    deploy_then_drift_catalog(base.path(), &workspace);

    let mut stopped = make_record(None);
    stopped.state = ManagedSessionState::Stopped;
    stopped.workspace_path = Some(workspace);

    let summary = record_to_summary_checked_under(&stopped, base.path().to_path_buf()).await;

    assert!(
        summary.stale_assets,
        "the on-demand single-session path must still detect a Stopped \
         session's deployed-asset drift — a fast `tm ls` that never detects \
         staleness anywhere is a regression, not a fix"
    );
    assert!(
        !summary.stale_assets_unchecked,
        "the on-demand path DID check, so its verdict is authoritative"
    );
}

/// Issue #4335: `?slim=true` must actually SKIP the asset-staleness probe.
///
/// Why: asserting only that a slim listing reports `stale_assets: false` is
/// vacuous — `false` is also the value a session with current assets gets.
/// This builds the exact fixture
/// `checked_summaries_flags_stale_assets_only_for_relevant_states` uses to
/// produce a genuine `true`, then shows the slim path reports `false` for it.
/// The difference can only come from the probe not running, which is the whole
/// point of the flag: the guard's cold-daemon timeout was that probe's cost.
///
/// #5040: base injected via [`fake_base`], so no `$HOME` write and no
/// `#[serial]`.
#[tokio::test]
async fn checked_summaries_slim_skips_stale_assets_probe() {
    let base = fake_base();
    let fw = crate::core::paths::FrameworkPaths::under(base.path());
    let bundled = fw.agent_source_dir();
    std::fs::create_dir_all(&bundled).unwrap();
    std::fs::write(bundled.join("rust-engineer.md"), "v1").unwrap();

    let workspace = base.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let session_fw =
        crate::core::paths::FrameworkPaths::for_managed_workspace_under(base.path(), &workspace);
    crate::core::agent_deployer::deploy_agents_filtered(
        &bundled,
        &session_fw.agent_deploy_dir(),
        |_| true,
    )
    .unwrap();
    std::fs::write(bundled.join("rust-engineer.md"), "v2 — catalog moved").unwrap();

    let mut active = make_record(None);
    active.state = ManagedSessionState::Active;
    active.workspace_path = Some(workspace);
    let records = vec![active];

    // Control: the full probe SEES the drift.
    let full = checked_summaries_with_under(&records, true, base.path().to_path_buf()).await;
    assert!(
        full[0].stale_assets,
        "fixture must genuinely be stale, else the slim assertion below is vacuous"
    );

    // Slim: same records, probe skipped, flag left at its default.
    let slim = checked_summaries_with_under(&records, false, base.path().to_path_buf()).await;
    assert!(
        !slim[0].stale_assets,
        "slim mode must skip the staleness probe entirely (#4335)"
    );
    assert_eq!(
        slim[0].id, full[0].id,
        "slim mode must still return every session — only the probe is skipped"
    );
}

/// Issue #2444 review (MEDIUM finding): `checked_summaries`'s staleness probe
/// now shares ONE `CatalogHashes::compute` across every session resolving the
/// same default catalog source (`stale_assets_for_many` in `summary.rs`).
/// This pins that the SHARING never cross-contaminates results — two Active
/// sessions using the identical (default bundled) catalog source, one fresh
/// and one genuinely stale, must each get their OWN correct, independent
/// `stale_assets` verdict.
///
/// Issue #4409 rewired the drift vehicle from agents to SKILLS: bundled agents
/// now deploy into ONE machine-global tier shared by every session, so an
/// agent can no longer be stale for session A and fresh for session B. Skills
/// are still deployed per-workspace, so they carry the per-session property
/// this test exists to pin.
///
/// #5040: base injected via [`fake_base`], so no `$HOME` write and no
/// `#[serial]`.
#[tokio::test]
async fn checked_summaries_stale_assets_independent_per_session_sharing_one_catalog() {
    let base = fake_base();
    let fw = crate::core::paths::FrameworkPaths::under(base.path());
    let bundled = fw.skill_source_dir();
    std::fs::create_dir_all(&bundled).unwrap();
    std::fs::write(bundled.join("tm-doctor.md"), "v1").unwrap();

    let deploy_skills = |dest: &std::path::Path| {
        crate::core::skill_tiers::deploy_all_skill_tiers(
            &bundled,
            &fw.user_skill_source_dir(),
            dest,
            |_| true,
        )
        .unwrap();
    };

    // Session A: deploys while the catalog is at v1, then the catalog moves
    // to v2 — A must end up stale.
    let ws_a = base.path().join("workspace-a");
    std::fs::create_dir_all(&ws_a).unwrap();
    let fw_a = crate::core::paths::FrameworkPaths::for_managed_workspace_under(base.path(), &ws_a);
    deploy_skills(&fw_a.claude_skills_dir());
    std::fs::write(bundled.join("tm-doctor.md"), "v2 — catalog moved").unwrap();

    // Session B: deploys AFTER the catalog already moved to v2 — B must stay
    // fresh, sharing the SAME (now-v2) catalog hash cache entry as A.
    let ws_b = base.path().join("workspace-b");
    std::fs::create_dir_all(&ws_b).unwrap();
    let fw_b = crate::core::paths::FrameworkPaths::for_managed_workspace_under(base.path(), &ws_b);
    deploy_skills(&fw_b.claude_skills_dir());

    let mut a = make_record(None);
    a.state = ManagedSessionState::Active;
    a.workspace_path = Some(ws_a);
    let mut b = make_record(None);
    b.state = ManagedSessionState::Active;
    b.workspace_path = Some(ws_b);

    let records = vec![a, b];
    let summaries = checked_summaries_with_under(&records, true, base.path().to_path_buf()).await;

    assert!(
        summaries[0].stale_assets,
        "session A deployed against v1 and the catalog moved to v2 — must be stale"
    );
    assert!(
        !summaries[1].stale_assets,
        "session B deployed against the current v2 catalog — must stay fresh, \
         even though it shares the SAME cached catalog hashes as session A"
    );
}

// ── #4322: sharing the DEPLOYED-agent read across the fleet ──────────────────

/// Deploy `count` workspaces' SKILL trees from a shared bundled skill source,
/// returning one `Active` record per workspace.
///
/// Why: the #4322 fleet tests all need the same shape — one machine-global
/// agent deploy dir (#4409) plus N per-workspace skill trees — and building it
/// inline three times would let the three tests drift apart.
fn fleet_with_deployed_skills(base: &std::path::Path, count: usize) -> Vec<SessionRecord> {
    let fw = crate::core::paths::FrameworkPaths::under(base);
    let bundled_agents = fw.agent_source_dir();
    let bundled_skills = fw.skill_source_dir();
    std::fs::create_dir_all(&bundled_agents).unwrap();
    std::fs::create_dir_all(&bundled_skills).unwrap();
    std::fs::write(bundled_agents.join("rust-engineer.md"), "agent v1").unwrap();
    std::fs::write(bundled_skills.join("tm-doctor.md"), "skill v1").unwrap();
    crate::core::agent_deployer::deploy_agents_filtered(
        &bundled_agents,
        &fw.agent_deploy_dir(),
        |_| true,
    )
    .unwrap();

    (0..count)
        .map(|i| {
            let ws = base.join(format!("fleet-ws-{i}"));
            std::fs::create_dir_all(&ws).unwrap();
            let ws_fw = crate::core::paths::FrameworkPaths::for_managed_workspace_under(base, &ws);
            crate::core::skill_tiers::deploy_all_skill_tiers(
                &bundled_skills,
                &fw.user_skill_source_dir(),
                &ws_fw.claude_skills_dir(),
                |_| true,
            )
            .unwrap();
            let mut r = make_record(None);
            r.state = ManagedSessionState::Active;
            r.workspace_path = Some(ws);
            r
        })
        .collect()
}

/// Issue #4322: the machine-global deployed-AGENT directory must be read ONCE
/// per listing, not once per session.
///
/// Why: this is the performance claim made mechanically checkable. #4409 made
/// `FrameworkPaths::agent_deploy_dir()` a single path every managed session
/// resolves identically, so `tm ls` was opening the same agent files once per
/// session — on the reported 32-session fleet, 42 files re-read 32 times for
/// 32 byte-identical answers. A read COUNT (rather than a wall-clock
/// assertion) is the right pin: it does not move with machine load or
/// page-cache state, so it cannot flake, and it is exactly the quantity cold
/// `tm ls` latency scales with.
/// What: builds a 4-session fleet, runs the real `stale_assets_for_many` hot
/// path, and asserts the AGENT deploy directory was read exactly once while
/// each of the 4 per-workspace skill trees was read exactly once. Reverting the
/// hoist (moving `DeployedAgentHashes::read` back inside the per-session
/// fan-out) makes the agent count 4 instead of 1 and fail.
///
/// Isolation (#4619 review, HIGH): the read log is PROCESS-GLOBAL and none of
/// `core::update_check::tests`' 21 tests are `#[serial]`, so an exact count
/// taken globally attributes concurrent unrelated reads to this test — a
/// filtered `cargo test -- detect` run reproducibly observed 25 where this
/// expected 5. Counts are therefore taken with
/// `deployed_reads_under(<prefix>)`, scoped to directories that exist only
/// inside THIS test's own temp base, so no other test's reads can land under
/// them.
///
/// #5040: the base is now injected via [`fake_base`] and
/// `stale_assets_for_many_under`, so the test mutates no `$HOME` and carries no
/// `#[serial]` — it was the OTHER tests' `$HOME` writes that made this one
/// report 25 reads where it expected 1.
#[tokio::test]
async fn stale_assets_for_many_reads_shared_agent_dir_once_for_the_whole_fleet() {
    let base = fake_base();
    let records = fleet_with_deployed_skills(base.path(), 4);
    let fw = crate::core::paths::FrameworkPaths::under(base.path());
    let agents_dir = fw.agent_deploy_dir();

    // #5040: no `reset_deployed_read_log()` — a process-global clear from one
    // test erases another's entries. `deployed_reads_under` filters to this
    // test's own temp base, which is zero before the call below.
    let result = stale_assets_for_many_under(records, base.path().to_path_buf()).await;
    let agent_reads = crate::core::update_check::deployed_reads_under(&agents_dir);

    assert_eq!(result.len(), 4, "every session must still get a verdict");
    assert_eq!(
        agent_reads, 1,
        "the ONE catalog agent in the machine-global deploy dir must be read \
         ONCE for the whole 4-session fleet. Observing 4 means the shared \
         directory is being re-read per session — the #4322 fan-out this PR \
         removed"
    );
    for i in 0..4 {
        let ws = base.path().join(format!("fleet-ws-{i}"));
        let skills_dir =
            crate::core::paths::FrameworkPaths::for_managed_workspace_under(base.path(), &ws)
                .claude_skills_dir();
        assert_eq!(
            crate::core::update_check::deployed_reads_under(&skills_dir),
            1,
            "each session's PER-WORKSPACE skill tree must still be read for \
             that session — sharing must not have collapsed the genuinely \
             per-session half too"
        );
    }
}

/// Issue #4322 correctness gate: sharing the deployed-agent read WITHIN one
/// listing must not outlive that listing.
///
/// Why: the failure mode a reviewer must rule out is a cache that keeps
/// answering from stale bytes after the assets genuinely change. This PR
/// shares the read across sessions but retains NOTHING between calls, and this
/// test is what proves it: it runs the fan-out, mutates the shared agent tree
/// on disk, and runs it again in the SAME process with no reset of any kind.
/// The second call must see the change. A cross-request memo (the mtime-keyed
/// design #4322 sketched as step 3) is exactly what would fail here.
/// What: fresh fleet reports not-stale; the catalog then moves under it; the
/// very next call reports stale.
#[tokio::test]
async fn stale_assets_for_many_sees_an_agent_change_on_the_very_next_call() {
    let base = fake_base();
    let records = fleet_with_deployed_skills(base.path(), 3);

    let before = stale_assets_for_many_under(records.clone(), base.path().to_path_buf()).await;
    assert!(
        records.iter().all(|r| !before[&r.id]),
        "a freshly deployed fleet must start out fresh, else the assertion \
         below is vacuous"
    );

    // The catalog moves under the live process — no restart, no cache reset.
    let fw = crate::core::paths::FrameworkPaths::under(base.path());
    std::fs::write(
        fw.agent_source_dir().join("rust-engineer.md"),
        "agent v2 — catalog moved",
    )
    .unwrap();

    let after = stale_assets_for_many_under(records.clone(), base.path().to_path_buf()).await;
    assert!(
        records.iter().all(|r| after[&r.id]),
        "the NEXT call must observe the change immediately — sharing the \
         deployed-agent read is scoped to one listing, never retained across \
         listings, so there is no invalidation window to miss (#4322)"
    );
}

/// Issue #4322: the shared read must not let one session's verdict leak into
/// another's, and the concurrent fan-out must not interleave them.
///
/// Why: sharing an input across N concurrently-spawned blocking tasks is the
/// classic place a per-session answer gets cross-contaminated. Agents are
/// machine-global (#4409) so they cannot differ per session, but SKILLS still
/// deploy per workspace — so a mixed fleet where some workspaces are drifted
/// and others are not is the discriminating case: every session shares one
/// `Arc<DeployedAgentHashes>` and one `Arc<CatalogHashes>` while reaching
/// different, correct conclusions from its own skill tree.
/// What: 6 sessions, alternating drifted/fresh skill deployments, run through
/// the real concurrent fan-out; each id must map to its own correct verdict.
#[tokio::test]
async fn stale_assets_for_many_keeps_per_session_verdicts_correct_under_concurrency() {
    let base = fake_base();
    let fw = crate::core::paths::FrameworkPaths::under(base.path());
    let bundled_skills = fw.skill_source_dir();
    std::fs::create_dir_all(&bundled_skills).unwrap();

    // Session ids in fixture order; the expected verdict for each is derived
    // AFTER the loop, from what is actually on disk once the last catalog write
    // has landed (deriving it inside the loop would record a guess that the
    // subsequent iterations can invalidate).
    let mut ids: Vec<ManagedSessionId> = Vec::new();
    let mut records = Vec::new();
    for i in 0..6 {
        let drifted = i % 2 == 0;
        // Deploy every workspace from the CURRENT catalog, then move the
        // catalog on only for the ones that must end up stale.
        std::fs::write(bundled_skills.join("tm-doctor.md"), format!("skill v{i}")).unwrap();
        let ws = base.path().join(format!("mixed-ws-{i}"));
        std::fs::create_dir_all(&ws).unwrap();
        let ws_fw =
            crate::core::paths::FrameworkPaths::for_managed_workspace_under(base.path(), &ws);
        crate::core::skill_tiers::deploy_all_skill_tiers(
            &bundled_skills,
            &fw.user_skill_source_dir(),
            &ws_fw.claude_skills_dir(),
            |_| true,
        )
        .unwrap();
        if drifted {
            std::fs::write(
                bundled_skills.join("tm-doctor.md"),
                format!("skill v{i} — moved after deploy"),
            )
            .unwrap();
        }
        let mut r = make_record(None);
        r.state = ManagedSessionState::Active;
        r.workspace_path = Some(ws);
        ids.push(r.id);
        records.push(r);
    }

    // Only the LAST catalog write is live when the probe runs, so derive what
    // each session should conclude against that final catalog state: a session
    // is stale iff its deployed body differs from the final catalog.
    let final_body = std::fs::read_to_string(bundled_skills.join("tm-doctor.md")).unwrap();
    let expected: Vec<(ManagedSessionId, bool)> = ids
        .iter()
        .enumerate()
        .map(|(i, id)| {
            let ws = base.path().join(format!("mixed-ws-{i}"));
            let ws_fw =
                crate::core::paths::FrameworkPaths::for_managed_workspace_under(base.path(), &ws);
            let deployed =
                std::fs::read_to_string(ws_fw.claude_skills_dir().join("tm-doctor/SKILL.md"))
                    .unwrap_or_default();
            (*id, deployed != final_body)
        })
        .collect();

    let got = stale_assets_for_many_under(records, base.path().to_path_buf()).await;

    assert!(
        expected.iter().any(|(_, stale)| *stale) && expected.iter().any(|(_, stale)| !*stale),
        "the fixture must contain BOTH stale and fresh sessions, else a \
         cross-contamination bug would pass unnoticed"
    );
    for (id, want) in expected {
        assert_eq!(
            got.get(&id).copied(),
            Some(want),
            "session {id} must get its OWN verdict despite sharing one \
             DeployedAgentHashes and one CatalogHashes across the concurrent \
             fan-out (#4322)"
        );
    }
}

/// The `worktree` spawn request survives the wire in all three shapes (#5274).
///
/// Why: `SpawnRequest::worktree` is the ONLY way a person's "give this session
/// its own worktree" reaches the daemon — `tm launch --worktree` posts it and
/// nothing else does. A dropped `#[serde(default)]`, a rename, or an
/// `Option<bool>` would each fail silently in a different direction: the first
/// makes every existing client's body a 400, the second makes the flag a no-op,
/// the third makes `"worktree": null` mean main-checkout instead of rejecting a
/// malformed body. None of those is visible from a behaviour test, so the wire
/// shape is pinned here directly.
/// What: deserializes three bodies — key absent, `true`, and `null` — and
/// asserts `false` / `true` / a parse error respectively. The `null` case is the
/// deliberate consequence of a plain `bool` over `Option<bool>`, the same choice
/// `force_new` and `background` make and for the same reason.
/// Test: itself.
#[test]
fn spawn_request_worktree_wire_shape() {
    let base = r#"{"repo_url":"https://github.invalid/o/r","ref":"main","task":"t""#;

    let absent: super::SpawnRequest = serde_json::from_str(&format!("{base}}}")).expect("absent");
    assert!(
        !absent.worktree,
        "#5274: a body with no `worktree` key must mean the main checkout, so \
         every pre-existing client keeps working unchanged"
    );

    let asked: super::SpawnRequest =
        serde_json::from_str(&format!(r#"{base},"worktree":true}}"#)).expect("true");
    assert!(
        asked.worktree,
        "#5274: `\"worktree\": true` must arrive as an explicit worktree request"
    );

    assert!(
        serde_json::from_str::<super::SpawnRequest>(&format!(r#"{base},"worktree":null}}"#))
            .is_err(),
        "an explicit null is a malformed body, not a tolerated `false` — same \
         rule as `force_new` and `background`"
    );
}

/// Why (#5007): the `store_health` disclosure must be ABSENT from a healthy
/// list response, not present-and-null. Every existing consumer of this
/// endpoint parses the body as-is, and a new always-present field is a wire
/// change; an omitted one is not.
/// What: serializes a healthy response and asserts the key does not appear,
/// then serializes a degraded one and asserts it does.
/// Test: this test.
#[test]
fn list_response_omits_store_health_when_healthy() {
    let healthy = super::ListSessionsResponse {
        sessions: vec![],
        store_health: None,
    };
    let json = serde_json::to_value(&healthy).expect("serialize");
    assert!(
        json.get("store_health").is_none(),
        "a healthy response must not grow a field: {json}"
    );

    let degraded = super::ListSessionsResponse {
        sessions: vec![],
        store_health: Some(super::StoreHealthPayload {
            message: "/x/sessions.json is corrupt".into(),
            corrupt: true,
            observed_at: "2026-08-06T09:01:17Z".into(),
        }),
    };
    let json = serde_json::to_value(&degraded).expect("serialize");
    assert_eq!(json["store_health"]["corrupt"], serde_json::json!(true));
}
