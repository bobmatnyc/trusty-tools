//! Unit tests for managed-routes serializers.
//!
//! Why: isolating these tests into a `tests.rs` sibling keeps the production
//! file under the 500-SLOC cap while giving the test module the generous
//! 1500-SLOC budget.
//! What: focused assertions on `record_to_json` and `record_to_summary`.
//! Test: this file.

use std::path::PathBuf;

use chrono::Utc;

use super::summary::{reconcile_against_tmux, reconcile_live_state};
use super::{checked_summaries, record_to_json, record_to_summary};
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

/// Build a minimal [`SessionRecord`] suitable for serialization tests.
fn make_record(source_id: Option<&str>) -> SessionRecord {
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
    reconcile_live_state(&mut summaries, &records, &live, &attached);
    assert_eq!(summaries[0].state, "active", "live tmux → active");
    assert!(summaries[0].attached, "attached client → attached flag");

    // No live tmux → stays stopped, not attached.
    let mut summaries2: Vec<_> = records.iter().map(record_to_summary).collect();
    reconcile_live_state(&mut summaries2, &records, &HashSet::new(), &HashSet::new());
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
    reconcile_live_state(&mut summaries, &records, &HashSet::new(), &HashSet::new());
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
        reconcile_live_state(&mut summaries, &records, &live, &empty);
        assert_eq!(
            summaries[0].state, before,
            "{state:?} must not be reconciled by liveness"
        );
    }
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

/// RAII guard restoring `$HOME` on drop (including panic) — mirrors the
/// identical pattern in `core::session_assets::tests::HomeGuard` /
/// `core::standalone::load::tests::HomeGuard`.
///
/// Why: `checked_summaries` now also runs the #2444 asset-staleness probe
/// (`crate::core::session_assets::session_assets_stale`), which resolves its
/// bundled-source half via `FrameworkPaths::for_managed_workspace` — always
/// anchored at `FrameworkPaths::default().root` (the real `$HOME/.trusty-mpm`
/// in production). Any test exercising `checked_summaries` must therefore
/// point `$HOME` at a throwaway tempdir so the probe reads a fake framework
/// tree, never the developer's real one.
struct HomeGuard(Option<String>);
impl Drop for HomeGuard {
    fn drop(&mut self) {
        // SAFETY: paired with `#[serial_test::serial]` — no other thread
        // reads/writes the environment concurrently.
        match self.0 {
            Some(ref p) => unsafe { std::env::set_var("HOME", p) },
            None => unsafe { std::env::remove_var("HOME") },
        }
    }
}

/// Point `$HOME` at a fresh tempdir for the duration of the guard.
fn fake_home() -> (tempfile::TempDir, HomeGuard) {
    let home = tempfile::TempDir::new().unwrap();
    let prior = std::env::var("HOME").ok();
    // SAFETY: serialized via `#[serial_test::serial]` on every caller.
    unsafe { std::env::set_var("HOME", home.path()) };
    (home, HomeGuard(prior))
}

/// #2595 review (PR #2652, MEDIUM finding 4): [`checked_summaries`] fans its
/// per-record `unresumable` probes out concurrently via `JoinSet`, which
/// yields completed tasks in COMPLETION order, not submission order — this
/// pins that the returned `Vec<SessionSummary>` is nonetheless rebuilt in the
/// SAME order as the input `records` slice, and that only the genuinely-dead
/// record among a live/dead/healthy-stopped trio gets flagged.
#[tokio::test]
#[serial_test::serial]
async fn checked_summaries_preserves_input_order_and_flags_only_dead_sessions() {
    let _home = fake_home();
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
    let summaries = checked_summaries(&records).await;

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
/// (fake-home) bundled source, while leaving a `Provisioning` session (no
/// deploy has happened yet — every artifact would spuriously read "new") at
/// its `false` default regardless of workspace content.
#[tokio::test]
#[serial_test::serial]
async fn checked_summaries_flags_stale_assets_only_for_relevant_states() {
    let (home, _guard) = fake_home();
    let fw = crate::core::paths::FrameworkPaths::default();
    let bundled = fw.agent_source_dir();
    std::fs::create_dir_all(&bundled).unwrap();
    std::fs::write(bundled.join("rust-engineer.md"), "v1").unwrap();

    // Deploy into an Active session's workspace, then drift the catalog.
    let workspace = home.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let session_fw = crate::core::paths::FrameworkPaths::for_managed_workspace(&workspace);
    crate::core::agent_deployer::deploy_agents_filtered(
        &bundled,
        &session_fw.claude_agents_dir(),
        |_| true,
    )
    .unwrap();
    std::fs::write(bundled.join("rust-engineer.md"), "v2 — catalog moved").unwrap();

    let mut active = make_record(None);
    active.state = ManagedSessionState::Active;
    active.workspace_path = Some(workspace);

    // Provisioning: no deploy has happened in this workspace at all.
    let provisioning_ws = home.path().join("never-deployed");
    std::fs::create_dir_all(&provisioning_ws).unwrap();
    let mut provisioning = make_record(None);
    provisioning.state = ManagedSessionState::Provisioning;
    provisioning.workspace_path = Some(provisioning_ws);

    let records = vec![active, provisioning];
    let summaries = checked_summaries(&records).await;

    assert!(
        summaries[0].stale_assets,
        "an Active session whose deployed agent drifted from the bundled \
         source must be flagged stale_assets"
    );
    assert!(
        !summaries[1].stale_assets,
        "a Provisioning session must never be probed — it has not deployed yet"
    );
}

/// Issue #2444 review (MEDIUM finding): `checked_summaries`'s staleness probe
/// now shares ONE `CatalogHashes::compute` across every session resolving the
/// same default catalog source (`stale_assets_for_many` in `summary.rs`).
/// This pins that the SHARING never cross-contaminates results — two Active
/// sessions using the identical (default bundled) catalog source, one fresh
/// and one genuinely stale, must each get their OWN correct, independent
/// `stale_assets` verdict.
#[tokio::test]
#[serial_test::serial]
async fn checked_summaries_stale_assets_independent_per_session_sharing_one_catalog() {
    let (home, _guard) = fake_home();
    let fw = crate::core::paths::FrameworkPaths::default();
    let bundled = fw.agent_source_dir();
    std::fs::create_dir_all(&bundled).unwrap();
    std::fs::write(bundled.join("rust-engineer.md"), "v1").unwrap();

    // Session A: deploys while the catalog is at v1, then the catalog moves
    // to v2 — A must end up stale.
    let ws_a = home.path().join("workspace-a");
    std::fs::create_dir_all(&ws_a).unwrap();
    let fw_a = crate::core::paths::FrameworkPaths::for_managed_workspace(&ws_a);
    crate::core::agent_deployer::deploy_agents_filtered(
        &bundled,
        &fw_a.claude_agents_dir(),
        |_| true,
    )
    .unwrap();
    std::fs::write(bundled.join("rust-engineer.md"), "v2 — catalog moved").unwrap();

    // Session B: deploys AFTER the catalog already moved to v2 — B must stay
    // fresh, sharing the SAME (now-v2) catalog hash cache entry as A.
    let ws_b = home.path().join("workspace-b");
    std::fs::create_dir_all(&ws_b).unwrap();
    let fw_b = crate::core::paths::FrameworkPaths::for_managed_workspace(&ws_b);
    crate::core::agent_deployer::deploy_agents_filtered(
        &bundled,
        &fw_b.claude_agents_dir(),
        |_| true,
    )
    .unwrap();

    let mut a = make_record(None);
    a.state = ManagedSessionState::Active;
    a.workspace_path = Some(ws_a);
    let mut b = make_record(None);
    b.state = ManagedSessionState::Active;
    b.workspace_path = Some(ws_b);

    let records = vec![a, b];
    let summaries = checked_summaries(&records).await;

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
