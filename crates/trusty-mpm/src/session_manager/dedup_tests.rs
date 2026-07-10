//! Dedup coverage for stale duplicate session records (#2306).
//!
//! Why: `session_manager/tests.rs` is at the 1500-SLOC test cap; this
//! #2306-specific coverage lives here so neither file grows past its limit,
//! mirroring the pattern established by `backfill_tests.rs` /
//! `decommission_worktree_tests.rs`. It reuses the sibling `tests` module's
//! `FakeTmuxDriver` scaffolding for the reconcile integration tests, and drives
//! the pure planner [`super::dedup::plan_dedup`] / predicate
//! [`super::dedup::is_resolved_existing`] directly for the algorithm cases (no
//! real tmux — the #1790 guard). Existence checks use real [`TempDir`]s so
//! `is_resolved_existing`'s `Path::exists()` call observes genuine filesystem
//! state rather than being mocked.
//! What: unit tests for the grouping/eligibility/live-group rules of
//! `plan_dedup` post the #2306 review fix — a resolved, still-existing
//! `workspace_path` can NEVER be a dedup loser, regardless of group size or
//! recency, because `source_id` groups by repository and multiple concurrent
//! worktree sessions of one repo legitimately share a group — plus end-to-end
//! `reconcile_on_boot` tests proving the same invariant holds through the full
//! boot path.
//! Test: this file IS the test module; run with `cargo test -p trusty-mpm`.

use std::collections::HashSet;
use std::path::PathBuf;

use chrono::{Duration, Utc};
use tempfile::TempDir;

use super::dedup::{is_resolved_existing, plan_dedup};
use super::record::{ManagedSessionId, ManagedSessionState, SessionRecord};
use super::tests::FakeTmuxDriver;
use crate::session_manager::SessionManager;

/// Build a `SessionRecord` with only the fields the dedup logic reads varying.
///
/// Why: `plan_dedup` keys off `source_id`, `workspace_path`, `state`,
/// `tmux_name`, `last_activity_at`, and `created_at`; a compact builder keeps
/// the 20-field struct literal out of every test. `ws_path = None` models the
/// `/unknown` stub (no `workspace_path`); pass a real, existing directory to
/// model a resolved-existing record, or a path that was never created (or was
/// removed) to model a resolved-but-dead one.
/// Test: used by every test in this module.
#[allow(clippy::too_many_arguments)]
fn rec(
    tmux_name: &str,
    source_id: Option<&str>,
    ws_path: Option<&PathBuf>,
    state: ManagedSessionState,
    activity_offset_secs: i64,
) -> SessionRecord {
    let created = Utc::now();
    SessionRecord {
        id: ManagedSessionId::new(),
        tmux_name: tmux_name.into(),
        cwd: ws_path
            .cloned()
            .unwrap_or_else(|| PathBuf::from("/unknown")),
        task: "t".into(),
        state,
        created_at: created,
        last_activity_at: Some(created + Duration::seconds(activity_offset_secs)),
        workspace_path: ws_path.cloned(),
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

// ---------------------------------------------------------------------
// `is_resolved_existing` predicate — the #2306 review-fixed safety boundary.
// ---------------------------------------------------------------------

/// A record whose `workspace_path` points at a real, currently-existing
/// directory is resolved-existing.
///
/// Why: this is the exact predicate both `plan_dedup`'s partition and the
/// belt-and-suspenders recheck at the decommission call site rely on; it must
/// observe genuine disk state, hence a real `TempDir`.
/// Test: this function IS the test.
#[test]
fn is_resolved_existing_true_for_real_dir() {
    let dir = TempDir::new().unwrap();
    let r = rec(
        "tm-proj-01",
        Some("proj"),
        Some(&dir.path().to_path_buf()),
        ManagedSessionState::Stopped,
        0,
    );
    assert!(is_resolved_existing(&r));
}

/// A record whose `workspace_path` is set but the directory does not (or no
/// longer) exists on disk is NOT resolved-existing.
///
/// Why: this is the "resolved-but-dead" case — e.g. a worktree removed by
/// other means — which must be dedup-loser-eligible since there is nothing
/// left on disk to lose.
/// Test: this function IS the test.
#[test]
fn is_resolved_existing_false_for_missing_dir() {
    let dir = TempDir::new().unwrap();
    let ghost = dir.path().join("never-existed");
    let r = rec(
        "tm-proj-01",
        Some("proj"),
        Some(&ghost),
        ManagedSessionState::Stopped,
        0,
    );
    assert!(!is_resolved_existing(&r));
}

/// Neither a bare `None` `workspace_path` nor the literal `/unknown` sentinel
/// path counts as resolved-existing.
///
/// Why: both represent "never resolved" records; guards against a stray
/// literal `/unknown` `PathBuf` ever slipping past the check.
/// Test: this function IS the test.
#[test]
fn is_resolved_existing_false_for_none_and_unknown_sentinel() {
    let none_ws = rec(
        "tm-proj-01",
        Some("proj"),
        None,
        ManagedSessionState::Stopped,
        0,
    );
    assert!(!is_resolved_existing(&none_ws));

    let sentinel = PathBuf::from("/unknown");
    let literal_unknown = rec(
        "tm-proj-02",
        Some("proj"),
        Some(&sentinel),
        ManagedSessionState::Stopped,
        0,
    );
    assert!(!is_resolved_existing(&literal_unknown));
}

// ---------------------------------------------------------------------
// `plan_dedup` — grouping / eligibility / live-group rules.
// ---------------------------------------------------------------------

/// (i, REWRITE of the pre-review `plan_dedup_three_way_group_one_survivor`):
/// two DISTINCT resolved-and-existing workspace paths sharing one
/// `source_id`, both `Stopped`, no live tmux → NEITHER is a loser.
///
/// Why: this is the exact scenario the review flagged as a data-loss bug —
/// `source_id` is per-repository, so two concurrent worktree sessions of one
/// repo share a group. The old policy collapsed this to "keep the newest,"
/// which would `git worktree remove --force` real work. The fixed rule must
/// leave both untouched regardless of recency.
/// Test: this function IS the test.
#[test]
fn plan_dedup_two_resolved_existing_paths_untouched() {
    let dir_a = TempDir::new().unwrap();
    let dir_b = TempDir::new().unwrap();
    let older = rec(
        "tm-proj-01",
        Some("proj"),
        Some(&dir_a.path().to_path_buf()),
        ManagedSessionState::Stopped,
        1,
    );
    let newer = rec(
        "tm-proj-02",
        Some("proj"),
        Some(&dir_b.path().to_path_buf()),
        ManagedSessionState::Stopped,
        99, // more recent — must NOT matter; both are resolved-existing
    );

    let losers = plan_dedup(&[older, newer], &HashSet::new());
    assert!(
        losers.is_empty(),
        "two resolved-existing distinct-path records must both survive: {losers:?}"
    );
}

/// A canonical resolved-existing record + a stale `/unknown` adopted record
/// sharing one `source_id`, both `Stopped`, no live tmux → the `/unknown`
/// record is the loser; the canonical one survives.
///
/// Why: this IS the #2306 target scenario — the core property the whole
/// feature exists to fix.
/// Test: this function IS the test.
#[test]
fn plan_dedup_prefers_resolved_existing_over_unknown() {
    let dir = TempDir::new().unwrap();
    let resolved = rec(
        "tm-proj-01",
        Some("proj"),
        Some(&dir.path().to_path_buf()),
        ManagedSessionState::Stopped,
        10,
    );
    let unknown = rec(
        "tm-proj-02",
        Some("proj"),
        None,
        ManagedSessionState::Stopped,
        99, // more recent, but must NOT win — resolved-existing beats /unknown
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
        "the resolved-existing record must survive"
    );
}

/// A resolved-but-path-deleted record + a resolved-existing record sharing one
/// `source_id`, both `Stopped`, no live tmux → the dead-path record is the
/// loser, regardless of recency.
///
/// Why: "resolved" alone (a non-`None` `workspace_path`) is not enough to
/// protect a record — only a path that still `exists()` does. This proves the
/// dead-path case is still caught even though it superficially "looks like" a
/// concurrent-session record (non-`None` `workspace_path`).
/// Test: this function IS the test.
#[test]
fn plan_dedup_dead_path_loses_to_resolved_existing() {
    let dir = TempDir::new().unwrap();
    let ghost_dir = TempDir::new().unwrap();
    let ghost_path = ghost_dir.path().join("removed-worktree");
    // ghost_path is never created — models a resolved path whose directory
    // was already removed by other means.

    let existing = rec(
        "tm-proj-01",
        Some("proj"),
        Some(&dir.path().to_path_buf()),
        ManagedSessionState::Stopped,
        1,
    );
    let dead = rec(
        "tm-proj-02",
        Some("proj"),
        Some(&ghost_path),
        ManagedSessionState::Stopped,
        99, // more recent, but must NOT matter — the path is dead
    );
    let existing_id = existing.id;
    let dead_id = dead.id;

    let losers = plan_dedup(&[existing, dead], &HashSet::new());
    assert_eq!(
        losers,
        vec![dead_id],
        "the dead-path record must be the loser"
    );
    assert!(!losers.contains(&existing_id));
}

/// A three-way mixed group (resolved-existing + resolved-dead + /unknown)
/// sharing one `source_id`: the resolved-existing member survives, BOTH
/// others are losers.
///
/// Why: generalises the categorical-exclusion rule beyond two-member groups —
/// every non-resolved-existing member is superseded once any
/// resolved-existing member is present, independent of group size.
/// Test: this function IS the test.
#[test]
fn plan_dedup_mixed_three_way_group_existing_survives_others_lose() {
    let dir = TempDir::new().unwrap();
    let ghost_dir = TempDir::new().unwrap();
    let ghost_path = ghost_dir.path().join("removed-worktree");

    let existing = rec(
        "tm-proj-01",
        Some("proj"),
        Some(&dir.path().to_path_buf()),
        ManagedSessionState::Stopped,
        1,
    );
    let dead = rec(
        "tm-proj-02",
        Some("proj"),
        Some(&ghost_path),
        ManagedSessionState::Stopped,
        50,
    );
    let unknown = rec(
        "tm-proj-03",
        Some("proj"),
        None,
        ManagedSessionState::Stopped,
        99,
    );
    let existing_id = existing.id;
    let dead_id = dead.id;
    let unknown_id = unknown.id;

    let losers = plan_dedup(&[existing, dead, unknown], &HashSet::new());
    let got: HashSet<ManagedSessionId> = losers.iter().copied().collect();
    let expected: HashSet<ManagedSessionId> = [dead_id, unknown_id].into_iter().collect();
    assert_eq!(got, expected);
    assert!(!losers.contains(&existing_id));
}

/// A group with ANY live-tmux member is left entirely untouched.
///
/// Why: the live pane is authoritative; dedup must only quiesce fully-stopped
/// groups (issue nuance) — verified independent of the eligibility rule above.
/// Test: this function IS the test.
#[test]
fn plan_dedup_live_group_untouched() {
    let dir = TempDir::new().unwrap();
    let live = rec(
        "tm-proj-01",
        Some("proj"),
        Some(&dir.path().to_path_buf()),
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

/// Records with distinct `source_id`s are distinct projects — never merged.
///
/// Why: dedup must not collapse two legitimately-separate projects.
/// Test: this function IS the test.
#[test]
fn plan_dedup_distinct_source_ids_untouched() {
    let dir = TempDir::new().unwrap();
    let a = rec(
        "tm-a-01",
        Some("proj-a"),
        Some(&dir.path().to_path_buf()),
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

/// A `/unknown`-only group (zero resolved-existing members): the most-recent
/// record survives, the rest are losers.
///
/// Why: when nothing in the group is resolved-existing, there is nothing on
/// disk to lose — the tie-break falls through to recency.
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
        "with no resolved-existing record, the older /unknown loses"
    );
    assert!(!losers.contains(&newer_id));
}

/// A single record per project is never a duplicate.
///
/// Why: guard against decommissioning a lone record.
/// Test: this function IS the test.
#[test]
fn plan_dedup_singleton_group_untouched() {
    let dir = TempDir::new().unwrap();
    let only = rec(
        "tm-proj-01",
        Some("proj"),
        Some(&dir.path().to_path_buf()),
        ManagedSessionState::Stopped,
        10,
    );
    assert!(plan_dedup(&[only], &HashSet::new()).is_empty());
}

// ---------------------------------------------------------------------
// `reconcile_on_boot` — end-to-end wiring.
// ---------------------------------------------------------------------

/// End-to-end: `reconcile_on_boot` collapses a quiesced duplicate pair into the
/// resolved-existing survivor and decommissions the `/unknown` record.
///
/// Why: proves the pure planner is wired into the boot reconcile and that
/// decommission runs on the loser via the safe (no-disk) path.
/// What: seeds two `Stopped` records sharing one `source_id` (one
/// resolved-existing via a real `TempDir`, one `/unknown`), with NO live
/// tmux, runs reconcile, then asserts the `/unknown` record is
/// `Decommissioned` and the resolved one is not.
/// Test: this function IS the test.
#[tokio::test]
async fn reconcile_dedup_collapses_stopped_duplicates() {
    let dir = TempDir::new().unwrap();
    let ws_dir = TempDir::new().unwrap();
    let fake = FakeTmuxDriver::new();
    let mgr = SessionManager::new(dir.path(), fake.clone()).await.unwrap();

    let resolved = rec(
        "tm-proj-01",
        Some("proj"),
        Some(&ws_dir.path().to_path_buf()),
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
        "the resolved-existing survivor must NOT be decommissioned"
    );
    assert!(
        ws_dir.path().exists(),
        "the resolved survivor's workspace directory must remain on disk"
    );
}

/// End-to-end: two resolved-existing concurrent-worktree-style records
/// sharing one `source_id` are BOTH left untouched by reconcile dedup.
///
/// Why: this is the exact review-flagged data-loss scenario driven through the
/// full boot path, not just the pure planner — proves reconcile_on_boot never
/// decommissions (and therefore never `git worktree remove --force`s) either
/// side of a legitimate concurrent-session pair.
/// Test: this function IS the test.
#[tokio::test]
async fn reconcile_dedup_leaves_two_resolved_existing_worktrees_untouched() {
    let dir = TempDir::new().unwrap();
    let ws_a = TempDir::new().unwrap();
    let ws_b = TempDir::new().unwrap();
    let fake = FakeTmuxDriver::new();
    let mgr = SessionManager::new(dir.path(), fake.clone()).await.unwrap();

    let a = rec(
        "tm-proj-01",
        Some("proj"),
        Some(&ws_a.path().to_path_buf()),
        ManagedSessionState::Stopped,
        1,
    );
    let b = rec(
        "tm-proj-02",
        Some("proj"),
        Some(&ws_b.path().to_path_buf()),
        ManagedSessionState::Stopped,
        99,
    );
    let a_id = a.id;
    let b_id = b.id;
    {
        let mut store = mgr.store.write().await;
        store.upsert(a).await.unwrap();
        store.upsert(b).await.unwrap();
    }

    mgr.reconcile_on_boot(false).await.expect("reconcile");

    assert_ne!(
        mgr.get(&a_id).await.unwrap().state,
        ManagedSessionState::Decommissioned,
        "first concurrent worktree session must survive"
    );
    assert_ne!(
        mgr.get(&b_id).await.unwrap().state,
        ManagedSessionState::Decommissioned,
        "second concurrent worktree session must survive"
    );
    assert!(ws_a.path().exists(), "workspace A must remain on disk");
    assert!(ws_b.path().exists(), "workspace B must remain on disk");
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
    let ws_dir = TempDir::new().unwrap();
    let fake = FakeTmuxDriver::new();
    fake.seeded_names.lock().unwrap().push("tm-proj-01".into());
    let mgr = SessionManager::new(dir.path(), fake.clone()).await.unwrap();

    let live = rec(
        "tm-proj-01",
        Some("proj"),
        Some(&ws_dir.path().to_path_buf()),
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
