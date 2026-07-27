//! Tests for the by-state managed-session prune surface (#1508).
//!
//! Why: `session_manager/tests.rs` is at the 1500-SLOC test cap; these tests
//! (extracted verbatim, issue #3981 Part 2 review follow-up — the
//! `disable_hooks`/`pm_unrestricted` field additions to `SessionRecord` pushed
//! `tests.rs` 10 SLOC over budget) live here, mirroring
//! `reload_error_tests.rs`/`reactivate_tests.rs`/`restart_tests.rs`'s
//! established extraction pattern.
//! What: `prune_by_state_never_touches_active`, `prune_decommissioned_compacts`,
//! `prune_deleted_compacts`, `prune_all_targets_non_running`,
//! `prune_dry_run_reports_without_mutating`, `prune_filter_parse_round_trip`,
//! `prune_outcome_serializes`.
//! Test: this file IS the test module; run with `cargo test -p trusty-mpm`.

use super::manager::ManagedError;
use super::record::{ManagedSessionId, ManagedSessionState};
use super::tests::{make_manager, seed_record};

/// The by-state Stopped prune NEVER touches a running (Active) session (#1508).
///
/// Why: clearing legacy stopped/decommissioned records must not risk reaping a
/// live session. `include_active=false` is the fail-closed default.
/// What: seeds an Active and a Stopped session, prunes `Stopped`, asserts only the
/// Stopped one is decommissioned and the Active one is left running.
/// Test: this function IS the test.
#[tokio::test]
async fn prune_by_state_never_touches_active() {
    let dir = crate::test_support::hermetic_temp_dir();
    let (mgr, _fake) = make_manager(&dir).await;

    let active = ManagedSessionId::new();
    let stopped = ManagedSessionId::new();
    seed_record(&mgr, &dir, active, ManagedSessionState::Active, false).await;
    seed_record(&mgr, &dir, stopped, ManagedSessionState::Stopped, false).await;

    let outcome = mgr
        .prune_managed(
            crate::session_manager::PruneFilter::Stopped,
            false,
            false,
            None,
        )
        .await
        .expect("prune stopped");
    assert_eq!(outcome.count(), 1, "only the Stopped session is pruned");
    assert_eq!(
        mgr.get(&active).await.unwrap().state,
        ManagedSessionState::Active,
        "the Active session must be untouched"
    );
    assert_eq!(
        mgr.get(&stopped).await.unwrap().state,
        ManagedSessionState::Decommissioned
    );
}

/// The Decommissioned prune COMPACTS the store (removes tombstones) (#1508).
///
/// Why: tombstones accumulated unbounded; the compaction pass must actually delete
/// them from sessions.json so the file stops growing.
/// What: seeds two Decommissioned tombstones + one Stopped session, prunes
/// `Decommissioned`, and asserts both tombstones are GONE from the store while the
/// Stopped session remains.
/// Test: this function IS the test.
#[tokio::test]
async fn prune_decommissioned_compacts() {
    let dir = crate::test_support::hermetic_temp_dir();
    let (mgr, _fake) = make_manager(&dir).await;

    let t1 = ManagedSessionId::new();
    let t2 = ManagedSessionId::new();
    let stopped = ManagedSessionId::new();
    seed_record(&mgr, &dir, t1, ManagedSessionState::Decommissioned, false).await;
    seed_record(&mgr, &dir, t2, ManagedSessionState::Decommissioned, false).await;
    seed_record(&mgr, &dir, stopped, ManagedSessionState::Stopped, false).await;

    let outcome = mgr
        .prune_managed(
            crate::session_manager::PruneFilter::Decommissioned,
            false,
            false,
            None,
        )
        .await
        .expect("compact");
    assert_eq!(outcome.count(), 2, "both tombstones compacted");
    assert!(
        outcome
            .sessions
            .iter()
            .all(|s| s.action == crate::session_manager::PruneAction::Removed),
        "decommissioned prune reports Removed"
    );

    // Both tombstones are GONE from the store; the Stopped record survives.
    assert!(matches!(
        mgr.get(&t1).await,
        Err(ManagedError::SessionNotFound(_))
    ));
    assert!(matches!(
        mgr.get(&t2).await,
        Err(ManagedError::SessionNotFound(_))
    ));
    assert_eq!(
        mgr.list().await.len(),
        1,
        "only the Stopped session remains"
    );
}

/// The Deleted prune COMPACTS the store (removes `--deleted--` tombstones) (#2012).
///
/// Why: `tm sessions delete` now SOFT-deletes (marks `--deleted--`, keeps the
/// record); operators need a permanent-removal path, and `prune --state deleted`
/// is it. A `Deleted` record is a terminal tombstone, so prune must COMPACT it
/// (remove), not re-run a decommission teardown.
/// What: seeds two `Deleted` tombstones + one `Stopped` session, prunes
/// `Deleted`, and asserts both tombstones are GONE while the Stopped survives,
/// each reported as `Removed`.
/// Test: this function IS the test.
#[tokio::test]
async fn prune_deleted_compacts() {
    let dir = crate::test_support::hermetic_temp_dir();
    let (mgr, _fake) = make_manager(&dir).await;

    let d1 = ManagedSessionId::new();
    let d2 = ManagedSessionId::new();
    let stopped = ManagedSessionId::new();
    seed_record(&mgr, &dir, d1, ManagedSessionState::Deleted, false).await;
    seed_record(&mgr, &dir, d2, ManagedSessionState::Deleted, false).await;
    seed_record(&mgr, &dir, stopped, ManagedSessionState::Stopped, false).await;

    let outcome = mgr
        .prune_managed(
            crate::session_manager::PruneFilter::Deleted,
            false,
            false,
            None,
        )
        .await
        .expect("compact deleted");
    assert_eq!(outcome.count(), 2, "both deleted tombstones compacted");
    assert!(
        outcome
            .sessions
            .iter()
            .all(|s| s.action == crate::session_manager::PruneAction::Removed),
        "deleted prune reports Removed"
    );
    assert!(matches!(
        mgr.get(&d1).await,
        Err(ManagedError::SessionNotFound(_))
    ));
    assert_eq!(
        mgr.list().await.len(),
        1,
        "only the Stopped session remains"
    );
}

/// `All` targets every NON-running record (#1508).
///
/// Why: the legacy purge needs ONE sweep that tears down stopped/errored/ephemeral
/// AND compacts decommissioned, while leaving running sessions alone.
/// What: seeds Active + Stopped + Errored + Decommissioned, prunes `All`, and
/// asserts the Active is untouched, Stopped/Errored became Decommissioned, and the
/// pre-existing tombstone was removed.
/// Test: this function IS the test.
#[tokio::test]
async fn prune_all_targets_non_running() {
    let dir = crate::test_support::hermetic_temp_dir();
    let (mgr, _fake) = make_manager(&dir).await;

    let active = ManagedSessionId::new();
    let stopped = ManagedSessionId::new();
    let errored = ManagedSessionId::new();
    let tomb = ManagedSessionId::new();
    seed_record(&mgr, &dir, active, ManagedSessionState::Active, false).await;
    seed_record(&mgr, &dir, stopped, ManagedSessionState::Stopped, false).await;
    seed_record(&mgr, &dir, errored, ManagedSessionState::Errored, false).await;
    seed_record(&mgr, &dir, tomb, ManagedSessionState::Decommissioned, false).await;

    let outcome = mgr
        .prune_managed(crate::session_manager::PruneFilter::All, false, false, None)
        .await
        .expect("prune all");
    assert_eq!(
        outcome.count(),
        3,
        "stopped + errored + tombstone (not active)"
    );

    assert_eq!(
        mgr.get(&active).await.unwrap().state,
        ManagedSessionState::Active,
        "running session is never touched by All"
    );
    assert_eq!(
        mgr.get(&stopped).await.unwrap().state,
        ManagedSessionState::Decommissioned
    );
    assert_eq!(
        mgr.get(&errored).await.unwrap().state,
        ManagedSessionState::Decommissioned
    );
    assert!(matches!(
        mgr.get(&tomb).await,
        Err(ManagedError::SessionNotFound(_))
    ));
}

/// A dry-run reports candidates WITHOUT mutating anything (#1508).
///
/// Why: the operator must be able to preview a legacy purge before destroying
/// records. `--dry-run` must be side-effect free.
/// What: seeds a Stopped session, prunes `Stopped` with `dry_run=true`, asserts the
/// outcome lists it but the record is STILL Stopped afterward.
/// Test: this function IS the test.
#[tokio::test]
async fn prune_dry_run_reports_without_mutating() {
    let dir = crate::test_support::hermetic_temp_dir();
    let (mgr, _fake) = make_manager(&dir).await;

    let stopped = ManagedSessionId::new();
    seed_record(&mgr, &dir, stopped, ManagedSessionState::Stopped, false).await;

    let outcome = mgr
        .prune_managed(
            crate::session_manager::PruneFilter::Stopped,
            true,
            false,
            None,
        )
        .await
        .expect("dry run");
    assert!(outcome.dry_run, "outcome flagged dry_run");
    assert_eq!(outcome.count(), 1, "candidate reported");
    // The record must be UNCHANGED after a dry run.
    assert_eq!(
        mgr.get(&stopped).await.unwrap().state,
        ManagedSessionState::Stopped,
        "dry run must not mutate the record"
    );
}

/// `PruneFilter::parse` round-trips and rejects garbage (#1508).
///
/// Why: the CLI/HTTP/MCP surfaces all parse the same spellings; a typo must be a
/// clear error, not a silent default.
/// What: parses every valid spelling (asserting `as_str` round-trips) and asserts
/// an unknown value errors.
/// Test: this function IS the test.
#[test]
fn prune_filter_parse_round_trip() {
    use crate::session_manager::PruneFilter;
    for f in [
        PruneFilter::Ephemeral,
        PruneFilter::Stopped,
        PruneFilter::Decommissioned,
        PruneFilter::Deleted,
        PruneFilter::All,
    ] {
        assert_eq!(PruneFilter::parse(f.as_str()).unwrap(), f);
    }
    assert_eq!(
        PruneFilter::parse("EPHEMERAL ").unwrap(),
        PruneFilter::Ephemeral
    );
    assert!(PruneFilter::parse("bogus").is_err());
}

/// `PruneOutcome`/`PruneAction` serialize to the wire shape the HTTP+MCP surfaces
/// expect (#1508).
///
/// Why: the dry-run/report JSON must carry `dry_run`, `filter`, and per-session
/// `action` so callers can render a precise preview; a serde regression would
/// silently change the wire contract.
/// What: builds an outcome, serializes it, and asserts the key fields/strings.
/// Test: this function IS the test.
#[tokio::test]
async fn prune_outcome_serializes() {
    let dir = crate::test_support::hermetic_temp_dir();
    let (mgr, _fake) = make_manager(&dir).await;
    let id = ManagedSessionId::new();
    seed_record(&mgr, &dir, id, ManagedSessionState::Stopped, true).await;

    let outcome = mgr
        .prune_managed(
            crate::session_manager::PruneFilter::Ephemeral,
            true,
            false,
            None,
        )
        .await
        .expect("dry run");
    let v = serde_json::to_value(&outcome).expect("serialize outcome");
    assert_eq!(v["dry_run"], serde_json::json!(true));
    assert_eq!(v["filter"], serde_json::json!("ephemeral"));
    assert_eq!(
        v["sessions"][0]["action"],
        serde_json::json!("decommissioned")
    );
}
