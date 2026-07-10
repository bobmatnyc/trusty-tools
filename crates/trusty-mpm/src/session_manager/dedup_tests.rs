//! Dedup coverage for stale duplicate session records (#2306).
//!
//! Why: `session_manager/tests.rs` is at the 1500-SLOC test cap; this
//! #2306-specific coverage lives here so neither file grows past its limit,
//! mirroring the pattern established by `backfill_tests.rs` /
//! `decommission_worktree_tests.rs`. It reuses the sibling `tests` module's
//! `FakeTmuxDriver` scaffolding for the reconcile integration tests, and drives
//! the pure planner [`super::dedup::plan_dedup`] directly for the algorithm
//! cases (no real tmux — the #1790 guard).
//! What: unit tests for the grouping/survivor/live-group rules of `plan_dedup`,
//! plus end-to-end `reconcile_on_boot` tests proving quiesced duplicates
//! collapse to the resolved survivor while live/distinct groups are untouched.
//! Test: this file IS the test module; run with `cargo test -p trusty-mpm`.

use std::collections::HashSet;
use std::path::PathBuf;

use chrono::{Duration, Utc};
use tempfile::TempDir;

use super::dedup::plan_dedup;
use super::record::{ManagedSessionId, ManagedSessionState, SessionRecord};
use super::tests::FakeTmuxDriver;
use crate::session_manager::SessionManager;

/// Build a `SessionRecord` with only the fields the dedup logic reads varying.
///
/// Why: `plan_dedup` keys off `source_id`, `workspace_path`, `state`,
/// `tmux_name`, `last_activity_at`, and `created_at`; a compact builder keeps
/// the 20-field struct literal out of every test.
/// What: constructs a record; `ws_path = None` models the `/unknown` stub.
/// Test: used by every test in this module.
#[allow(clippy::too_many_arguments)]
fn rec(
    tmux_name: &str,
    source_id: Option<&str>,
    ws_path: Option<&str>,
    state: ManagedSessionState,
    activity_offset_secs: i64,
) -> SessionRecord {
    let created = Utc::now();
    SessionRecord {
        id: ManagedSessionId::new(),
        tmux_name: tmux_name.into(),
        cwd: ws_path
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/unknown")),
        task: "t".into(),
        state,
        created_at: created,
        last_activity_at: Some(created + Duration::seconds(activity_offset_secs)),
        workspace_path: ws_path.map(PathBuf::from),
        repo_url: None,
        branch: None,
        pending_decision: None,
        proposed_default: None,
        correlation: Default::default(),
        runtime: Default::default(),
        ephemeral: false,
        workspace_owned: false,
        source_id: source_id.map(Into::into),
        claude_session_id: None,
        scrollback_path: None,
        last_cwd: None,
    }
}

/// A quiesced group (same `source_id`, no live tmux) collapses to the resolved
/// record; the `/unknown` stub is the loser.
///
/// Why: this is the core #2306 property — re-attach must stop targeting a stale
/// `/unknown` record when a resolved sibling exists.
/// Test: this function IS the test.
#[test]
fn plan_dedup_prefers_resolved_over_unknown() {
    let resolved = rec(
        "tm-proj-01",
        Some("proj"),
        Some("/tmp/proj"),
        ManagedSessionState::Stopped,
        10,
    );
    let unknown = rec(
        "tm-proj-02",
        Some("proj"),
        None,
        ManagedSessionState::Stopped,
        99, // more recent, but must NOT win — resolved beats /unknown
    );
    let resolved_id = resolved.id;
    let unknown_id = unknown.id;

    let losers = plan_dedup(&[resolved, unknown], &HashSet::new());
    assert_eq!(
        losers,
        vec![unknown_id],
        "the /unknown stub must be the loser"
    );
    assert!(
        !losers.contains(&resolved_id),
        "the resolved record must survive"
    );
}

/// A group with ANY live-tmux member is left entirely untouched.
///
/// Why: the live pane is authoritative; dedup must only quiesce fully-stopped
/// groups (issue nuance).
/// Test: this function IS the test.
#[test]
fn plan_dedup_live_group_untouched() {
    let live = rec(
        "tm-proj-01",
        Some("proj"),
        Some("/tmp/proj"),
        ManagedSessionState::Active,
        10,
    );
    let stale = rec(
        "tm-proj-02",
        Some("proj"),
        None,
        ManagedSessionState::Stopped,
        5,
    );
    let mut live_names = HashSet::new();
    live_names.insert("tm-proj-01".to_string());

    let losers = plan_dedup(&[live, stale], &live_names);
    assert!(
        losers.is_empty(),
        "a group with a live member must be untouched: {losers:?}"
    );
}

/// A three-way quiesced group keeps exactly one survivor (the resolved,
/// most-recent) and decommissions the other two.
///
/// Why: dedup must generalise beyond N=2.
/// Test: this function IS the test.
#[test]
fn plan_dedup_three_way_group_one_survivor() {
    let old_resolved = rec(
        "tm-proj-01",
        Some("proj"),
        Some("/tmp/proj-old"),
        ManagedSessionState::Stopped,
        1,
    );
    let new_resolved = rec(
        "tm-proj-02",
        Some("proj"),
        Some("/tmp/proj-new"),
        ManagedSessionState::Stopped,
        50, // most recent resolved → survivor
    );
    let unknown = rec(
        "tm-proj-03",
        Some("proj"),
        None,
        ManagedSessionState::Stopped,
        99,
    );
    let survivor_id = new_resolved.id;
    let old_id = old_resolved.id;
    let unknown_id = unknown.id;

    let losers = plan_dedup(&[old_resolved, new_resolved, unknown], &HashSet::new());
    let got: HashSet<ManagedSessionId> = losers.iter().copied().collect();
    let expected: HashSet<ManagedSessionId> = [old_id, unknown_id].into_iter().collect();
    assert_eq!(got, expected, "only the most-recent resolved must survive");
    assert!(!losers.contains(&survivor_id));
}

/// Records with distinct `source_id`s are distinct projects — never merged.
///
/// Why: dedup must not collapse two legitimately-separate projects.
/// Test: this function IS the test.
#[test]
fn plan_dedup_distinct_source_ids_untouched() {
    let a = rec(
        "tm-a-01",
        Some("proj-a"),
        Some("/tmp/a"),
        ManagedSessionState::Stopped,
        10,
    );
    let b = rec(
        "tm-b-01",
        Some("proj-b"),
        None,
        ManagedSessionState::Stopped,
        10,
    );
    let losers = plan_dedup(&[a, b], &HashSet::new());
    assert!(
        losers.is_empty(),
        "distinct projects must be untouched: {losers:?}"
    );
}

/// A `/unknown`-only group (grouped via `workspace_path`... but both lack one)
/// keyed by `source_id`: the most-recent record survives when none is resolved.
///
/// Why: when there is no resolved record to prefer, the tie-break must fall
/// through to most-recent `last_activity_at`.
/// Test: this function IS the test.
#[test]
fn plan_dedup_unknown_only_group_keeps_most_recent() {
    let older = rec(
        "tm-proj-01",
        Some("proj"),
        None,
        ManagedSessionState::Stopped,
        5,
    );
    let newer = rec(
        "tm-proj-02",
        Some("proj"),
        None,
        ManagedSessionState::Stopped,
        50,
    );
    let older_id = older.id;
    let newer_id = newer.id;

    let losers = plan_dedup(&[older, newer], &HashSet::new());
    assert_eq!(
        losers,
        vec![older_id],
        "with no resolved record, the older /unknown loses"
    );
    assert!(!losers.contains(&newer_id));
}

/// A single record per project is never a duplicate.
///
/// Why: guard against decommissioning a lone record.
/// Test: this function IS the test.
#[test]
fn plan_dedup_singleton_group_untouched() {
    let only = rec(
        "tm-proj-01",
        Some("proj"),
        Some("/tmp/proj"),
        ManagedSessionState::Stopped,
        10,
    );
    assert!(plan_dedup(&[only], &HashSet::new()).is_empty());
}

/// End-to-end: `reconcile_on_boot` collapses a quiesced duplicate pair into the
/// resolved survivor and decommissions the `/unknown` record.
///
/// Why: proves the pure planner is wired into the boot reconcile and that
/// decommission runs on the loser via the safe (no-disk) path.
/// What: seeds two `Stopped` records sharing one `source_id` (one resolved, one
/// `/unknown`), with NO live tmux, runs reconcile, then asserts the `/unknown`
/// record is `Decommissioned` and the resolved one is not.
/// Test: this function IS the test.
#[tokio::test]
async fn reconcile_dedup_collapses_stopped_duplicates() {
    let dir = TempDir::new().unwrap();
    let fake = FakeTmuxDriver::new();
    let mgr = SessionManager::new(dir.path(), fake.clone()).await.unwrap();

    let resolved = rec(
        "tm-proj-01",
        Some("proj"),
        Some("/tmp/proj"),
        ManagedSessionState::Stopped,
        10,
    );
    let unknown = rec(
        "tm-proj-02",
        Some("proj"),
        None,
        ManagedSessionState::Stopped,
        20,
    );
    let resolved_id = resolved.id;
    let unknown_id = unknown.id;
    {
        let mut store = mgr.store.write().await;
        store.upsert(resolved).await.unwrap();
        store.upsert(unknown).await.unwrap();
    }

    mgr.reconcile_on_boot(false).await.expect("reconcile");

    let unknown_after = mgr.get(&unknown_id).await.unwrap();
    assert_eq!(
        unknown_after.state,
        ManagedSessionState::Decommissioned,
        "the /unknown duplicate must be decommissioned by dedup"
    );
    let resolved_after = mgr.get(&resolved_id).await.unwrap();
    assert_ne!(
        resolved_after.state,
        ManagedSessionState::Decommissioned,
        "the resolved survivor must NOT be decommissioned"
    );
}

/// End-to-end: a group with a LIVE tmux member is untouched by reconcile dedup.
///
/// Why: the live/authoritative-record nuance must hold through the full boot
/// path, not just the pure planner.
/// What: seeds a live record (its `tmux_name` in `seeded_names`) and a stale
/// `/unknown` sibling sharing one `source_id`; after reconcile neither is
/// decommissioned.
/// Test: this function IS the test.
#[tokio::test]
async fn reconcile_dedup_skips_live_group() {
    let dir = TempDir::new().unwrap();
    let fake = FakeTmuxDriver::new();
    fake.seeded_names.lock().unwrap().push("tm-proj-01".into());
    let mgr = SessionManager::new(dir.path(), fake.clone()).await.unwrap();

    let live = rec(
        "tm-proj-01",
        Some("proj"),
        Some("/tmp/proj"),
        ManagedSessionState::Active,
        10,
    );
    let stale = rec(
        "tm-proj-02",
        Some("proj"),
        None,
        ManagedSessionState::Stopped,
        20,
    );
    let live_id = live.id;
    let stale_id = stale.id;
    {
        let mut store = mgr.store.write().await;
        store.upsert(live).await.unwrap();
        store.upsert(stale).await.unwrap();
    }

    mgr.reconcile_on_boot(false).await.expect("reconcile");

    assert_ne!(
        mgr.get(&live_id).await.unwrap().state,
        ManagedSessionState::Decommissioned,
        "the live record must survive"
    );
    assert_ne!(
        mgr.get(&stale_id).await.unwrap().state,
        ManagedSessionState::Decommissioned,
        "a group with a live member must NOT be deduped"
    );
}
