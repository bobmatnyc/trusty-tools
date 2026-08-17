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

use super::dedup::{is_resolved_existing, plan_dedup, plan_workspace_duplicates};
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
        deliverable_id: None,
        pane_id: None,
        injection_status: Default::default(),
        worktree_owner: None,
        terminal_at: None,
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

// ---------------------------------------------------------------------
// `plan_workspace_duplicates` — exact-workspace_path collapsing (#3396).
// ---------------------------------------------------------------------

/// Mark a record built via [`rec`] as SM-owned (the workspace was
/// provisioned by the SM itself, e.g. via clone).
fn owned(mut r: SessionRecord) -> SessionRecord {
    r.workspace_owned = true;
    r
}

/// THE #3396 regression case: two non-`Decommissioned` records resolve to
/// the LITERAL SAME existing workspace directory. One's `tmux_name` is live,
/// the other's is not (its tmux session was renamed/replaced out from under
/// it). `plan_dedup` alone (proven by `plan_dedup_live_group_untouched`
/// above, whose live-group guard skips the WHOLE group when ANY member is
/// live) would never collapse this — that is exactly why the duplicate
/// persisted in #3396. `plan_workspace_duplicates` must recognise "same
/// literal path" as unambiguous and decommission the dead sibling while
/// keeping the live one.
/// Test: this function IS the test.
#[test]
fn plan_workspace_duplicates_collapses_dead_sibling_of_live_record() {
    let dir = TempDir::new().unwrap();
    let live = rec(
        "tm-tcode-01",
        Some("proj"),
        Some(&dir.path().to_path_buf()),
        ManagedSessionState::Active,
        1,
    );
    let stale = rec(
        "tm-tm-tcode-01",
        Some("proj"),
        Some(&dir.path().to_path_buf()),
        ManagedSessionState::Stopped,
        50, // more recently "active" per the record, but must not matter —
            // the live tmux_name always wins over recency.
    );
    let live_id = live.id;
    let stale_id = stale.id;
    let mut live_names = HashSet::new();
    live_names.insert("tm-tcode-01".to_string());

    let losers = plan_workspace_duplicates(&[live, stale], &live_names);
    assert_eq!(
        losers,
        vec![stale_id],
        "the dead-tmux_name sibling at the same literal path must be the loser"
    );
    assert!(!losers.contains(&live_id), "the live record must survive");
}

/// Two records at the same literal path, NEITHER currently live: the most
/// recently active/created one survives (mirrors `plan_dedup`'s /unknown
/// tie-break), the other is the loser.
/// Test: this function IS the test.
#[test]
fn plan_workspace_duplicates_picks_most_recent_when_none_live() {
    let dir = TempDir::new().unwrap();
    let older = rec(
        "tm-proj-01",
        Some("proj"),
        Some(&dir.path().to_path_buf()),
        ManagedSessionState::Stopped,
        1,
    );
    let newer = rec(
        "tm-proj-02",
        Some("proj"),
        Some(&dir.path().to_path_buf()),
        ManagedSessionState::Stopped,
        99,
    );
    let older_id = older.id;
    let newer_id = newer.id;

    let losers = plan_workspace_duplicates(&[older, newer], &HashSet::new());
    assert_eq!(losers, vec![older_id], "the older record must be the loser");
    assert!(!losers.contains(&newer_id));
}

/// Two records at the same literal path, BOTH currently live: genuinely
/// ambiguous (two live panes in one directory) — must NOT auto-collapse,
/// since guessing a survivor risks tearing down a real, active session.
/// Test: this function IS the test.
#[test]
fn plan_workspace_duplicates_leaves_two_live_records_untouched() {
    let dir = TempDir::new().unwrap();
    let a = rec(
        "tm-proj-01",
        Some("proj"),
        Some(&dir.path().to_path_buf()),
        ManagedSessionState::Active,
        1,
    );
    let b = rec(
        "tm-proj-02",
        Some("proj"),
        Some(&dir.path().to_path_buf()),
        ManagedSessionState::Active,
        99,
    );
    let mut live_names = HashSet::new();
    live_names.insert("tm-proj-01".to_string());
    live_names.insert("tm-proj-02".to_string());

    let losers = plan_workspace_duplicates(&[a, b], &live_names);
    assert!(
        losers.is_empty(),
        "two simultaneously-live records at one path must be left for the operator: {losers:?}"
    );
}

/// An SM-owned record can NEVER be a loser, even when a dead unowned
/// duplicate shares its literal workspace path — the owned record is the
/// permanent survivor and the dead unowned sibling is decommissioned.
/// Test: this function IS the test.
#[test]
fn plan_workspace_duplicates_owned_record_is_permanent_survivor() {
    let dir = TempDir::new().unwrap();
    let canonical = owned(rec(
        "tm-proj-01",
        Some("proj"),
        Some(&dir.path().to_path_buf()),
        ManagedSessionState::Stopped,
        1,
    ));
    let phantom = rec(
        "tm-tm-proj-01",
        Some("proj"),
        Some(&dir.path().to_path_buf()),
        ManagedSessionState::Stopped,
        99, // more recent — must not matter, ownership always wins.
    );
    let canonical_id = canonical.id;
    let phantom_id = phantom.id;

    let losers = plan_workspace_duplicates(&[canonical, phantom], &HashSet::new());
    assert_eq!(
        losers,
        vec![phantom_id],
        "the unowned duplicate must be the loser"
    );
    assert!(
        !losers.contains(&canonical_id),
        "the SM-owned record must never be a loser"
    );
}

/// An SM-owned record's duplicate is left alone while its `tmux_name` is
/// live — never decommission (and thereby kill the tmux session of) a
/// currently-active duplicate, even one that will eventually need manual
/// cleanup.
/// Test: this function IS the test.
#[test]
fn plan_workspace_duplicates_owned_record_never_kills_live_duplicate() {
    let dir = TempDir::new().unwrap();
    let canonical = owned(rec(
        "tm-proj-01",
        Some("proj"),
        Some(&dir.path().to_path_buf()),
        ManagedSessionState::Stopped,
        1,
    ));
    let live_dupe = rec(
        "tm-tm-proj-01",
        Some("proj"),
        Some(&dir.path().to_path_buf()),
        ManagedSessionState::Active,
        99,
    );
    let mut live_names = HashSet::new();
    live_names.insert("tm-tm-proj-01".to_string());

    let losers = plan_workspace_duplicates(&[canonical, live_dupe], &live_names);
    assert!(
        losers.is_empty(),
        "a live duplicate must never be decommissioned, even alongside an owned record: {losers:?}"
    );
}

/// Two DIFFERENT SM-owned records sharing one literal path should never
/// structurally happen; rather than guess which is "real", the whole group
/// is left untouched.
/// Test: this function IS the test.
#[test]
fn plan_workspace_duplicates_two_owned_records_untouched() {
    let dir = TempDir::new().unwrap();
    let a = owned(rec(
        "tm-proj-01",
        Some("proj"),
        Some(&dir.path().to_path_buf()),
        ManagedSessionState::Stopped,
        1,
    ));
    let b = owned(rec(
        "tm-proj-02",
        Some("proj"),
        Some(&dir.path().to_path_buf()),
        ManagedSessionState::Stopped,
        99,
    ));
    let losers = plan_workspace_duplicates(&[a, b], &HashSet::new());
    assert!(
        losers.is_empty(),
        "two owned records at one path must never be auto-collapsed: {losers:?}"
    );
}

/// Records at DISTINCT workspace paths are never grouped, even sharing one
/// `source_id` — sanity check that grouping keys on the literal path, not
/// something coarser.
/// Test: this function IS the test.
#[test]
fn plan_workspace_duplicates_distinct_paths_untouched() {
    let dir_a = TempDir::new().unwrap();
    let dir_b = TempDir::new().unwrap();
    let a = rec(
        "tm-proj-01",
        Some("proj"),
        Some(&dir_a.path().to_path_buf()),
        ManagedSessionState::Stopped,
        1,
    );
    let b = rec(
        "tm-proj-02",
        Some("proj"),
        Some(&dir_b.path().to_path_buf()),
        ManagedSessionState::Stopped,
        99,
    );
    let losers = plan_workspace_duplicates(&[a, b], &HashSet::new());
    assert!(
        losers.is_empty(),
        "distinct paths must never be grouped: {losers:?}"
    );
}

/// A single record at a path is never a duplicate.
/// Test: this function IS the test.
#[test]
fn plan_workspace_duplicates_singleton_untouched() {
    let dir = TempDir::new().unwrap();
    let only = rec(
        "tm-proj-01",
        Some("proj"),
        Some(&dir.path().to_path_buf()),
        ManagedSessionState::Stopped,
        1,
    );
    assert!(plan_workspace_duplicates(&[only], &HashSet::new()).is_empty());
}

// ---------------------------------------------------------------------
// `reconcile_on_boot` — end-to-end wiring for exact-workspace duplicates.
// ---------------------------------------------------------------------

/// End-to-end #3396 regression: reconcile collapses a dead duplicate at the
/// SAME literal workspace path as a currently-live record — the exact shape
/// `plan_dedup`'s live-group guard leaves untouched forever (proven by
/// `reconcile_dedup_skips_live_group` above), which is why the #3396
/// duplicate persisted across every prior reconcile pass.
/// Test: this function IS the test.
#[tokio::test]
async fn reconcile_dedup_collapses_exact_workspace_duplicate_of_live_record() {
    let dir = TempDir::new().unwrap();
    let ws_dir = TempDir::new().unwrap();
    let fake = FakeTmuxDriver::new();
    fake.seeded_names.lock().unwrap().push("tm-tcode-01".into());
    let mgr = SessionManager::new(dir.path(), fake.clone()).await.unwrap();

    let live = rec(
        "tm-tcode-01",
        Some("proj"),
        Some(&ws_dir.path().to_path_buf()),
        ManagedSessionState::Active,
        1,
    );
    let stale = rec(
        "tm-tm-tcode-01",
        Some("proj"),
        Some(&ws_dir.path().to_path_buf()),
        ManagedSessionState::Stopped,
        50,
    );
    let live_id = live.id;
    let stale_id = stale.id;
    {
        let mut store = mgr.store.write().await;
        store.upsert(live).await.unwrap();
        store.upsert(stale).await.unwrap();
    }

    mgr.reconcile_on_boot(false).await.expect("reconcile");

    assert_eq!(
        mgr.get(&stale_id).await.unwrap().state,
        ManagedSessionState::Decommissioned,
        "the stale duplicate at the same literal path must be decommissioned"
    );
    assert_ne!(
        mgr.get(&live_id).await.unwrap().state,
        ManagedSessionState::Decommissioned,
        "the live record must survive"
    );
    assert!(
        ws_dir.path().exists(),
        "the workspace directory must remain on disk (loser was never SM-owned)"
    );
}

/// End-to-end: an SM-owned record's dead duplicate at the same literal path
/// is decommissioned by reconcile, but the owned record and its on-disk
/// workspace are never touched.
/// Test: this function IS the test.
#[tokio::test]
async fn reconcile_dedup_collapses_dead_duplicate_of_owned_record() {
    let dir = TempDir::new().unwrap();
    let ws_dir = TempDir::new().unwrap();
    let fake = FakeTmuxDriver::new();
    let mgr = SessionManager::new(dir.path(), fake.clone()).await.unwrap();

    let canonical = owned(rec(
        "tm-proj-01",
        Some("proj"),
        Some(&ws_dir.path().to_path_buf()),
        ManagedSessionState::Stopped,
        1,
    ));
    let phantom = rec(
        "tm-tm-proj-01",
        Some("proj"),
        Some(&ws_dir.path().to_path_buf()),
        ManagedSessionState::Stopped,
        99,
    );
    let canonical_id = canonical.id;
    let phantom_id = phantom.id;
    {
        let mut store = mgr.store.write().await;
        store.upsert(canonical).await.unwrap();
        store.upsert(phantom).await.unwrap();
    }

    mgr.reconcile_on_boot(false).await.expect("reconcile");

    assert_eq!(
        mgr.get(&phantom_id).await.unwrap().state,
        ManagedSessionState::Decommissioned,
        "the unowned phantom duplicate must be decommissioned"
    );
    assert_ne!(
        mgr.get(&canonical_id).await.unwrap().state,
        ManagedSessionState::Decommissioned,
        "the SM-owned record must never be decommissioned by dedup"
    );
    assert!(
        ws_dir.path().exists(),
        "the owned workspace directory must remain on disk"
    );
}

// ---------------------------------------------------------------------
// Fail-open: an unobservable tmux must never be read as an empty tmux.
//
// The incident these guard: two non-`Decommissioned`, unowned records shared
// the LITERAL same `workspace_path` (the main checkout, not two worktrees) and
// one of them was live and attached. That is
// `plan_workspace_duplicates`'s zero-owned / zero-live-candidates arm — it
// picks a survivor by recency and decommissions the rest — and it is reached
// only because the live one was missing from the live set. The live set was
// built by `list_sessions().unwrap_or_default()`, so any failure to observe
// tmux produced exactly that. `decommission` is irreversible: it tombstones
// the record, clears `workspace_path`, and `filter_live_sessions` hides
// `decommissioned` from every picker view, so the operator's own session
// vanished from the picker while they sat in it.
//
// Each test below makes the failure occur AT THE LIVENESS QUERY — the process
// reaches `dedup_stale_duplicates` normally and only tmux observation fails,
// so a pass cannot come from the code never reaching the step under test.
// ---------------------------------------------------------------------

/// Seed the incident shape: two unowned records at one literal existing path,
/// the first of them live in tmux.
///
/// Why: every refusal test below needs the same fixture, and the fixture is
/// the point — a different shape would exercise a different `plan_*` arm.
/// What: returns `(manager, live_id, other_id, _workspace_tempdir)`. The
/// caller configures `fake`'s failure mode BEFORE calling
/// `dedup_stale_duplicates`. `ws_dir` is returned so the caller keeps it
/// alive: dropping it deletes the directory and flips `is_resolved_existing`.
/// Test: used by `dedup_refuses_*` and `dedup_collapses_*_when_tmux_answers`.
async fn seed_same_path_pair(
    dir: &TempDir,
    tmux: std::sync::Arc<dyn super::driver::ManagedTmuxDriver>,
) -> (SessionManager, ManagedSessionId, ManagedSessionId, TempDir) {
    let ws_dir = TempDir::new().unwrap();
    let mgr = SessionManager::new(dir.path(), tmux).await.unwrap();
    let live = rec(
        "tm-dogfood",
        Some("bobmatnyc/trusty-tools"),
        Some(&ws_dir.path().to_path_buf()),
        ManagedSessionState::Active,
        1,
    );
    let other = rec(
        "tm-dogfood-02",
        Some("bobmatnyc/trusty-tools"),
        Some(&ws_dir.path().to_path_buf()),
        ManagedSessionState::Stopped,
        900,
    );
    let live_id = live.id;
    let other_id = other.id;
    {
        let mut store = mgr.store.write().await;
        store.upsert(live).await.unwrap();
        store.upsert(other).await.unwrap();
    }
    (mgr, live_id, other_id, ws_dir)
}

/// A `list_sessions()` ERROR must skip the dedup pass, not decommission.
///
/// Why: this is the arm `unwrap_or_default()` swallowed. The error is raised
/// by the liveness query itself, so the pass provably reaches it.
/// Test: this function IS the test.
#[tokio::test]
async fn dedup_refuses_when_list_sessions_fails() {
    let dir = TempDir::new().unwrap();
    let fake = FakeTmuxDriver::new();
    fake.seeded_names.lock().unwrap().push("tm-dogfood".into());
    let (mgr, live_id, other_id, _ws) = seed_same_path_pair(&dir, fake.clone()).await;

    *fake.list_sessions_should_fail.lock().unwrap() = true;
    let mut to_resume = Vec::new();
    let decommissioned = mgr
        .dedup_stale_duplicates(&mut to_resume)
        .await
        .expect("an unobservable tmux is a skip, not an error to the caller");

    assert!(
        decommissioned.is_empty(),
        "dedup decommissioned {decommissioned:?} while tmux could not be queried"
    );
    for (label, id) in [("live", live_id), ("sibling", other_id)] {
        assert_ne!(
            mgr.get(&id).await.unwrap().state,
            ManagedSessionState::Decommissioned,
            "the {label} record must survive a liveness query that failed"
        );
    }
}

/// A tmux server that cannot be STARTED must skip the pass too.
///
/// Why: `ensure_server_up` is the first half of the liveness query (#3823 /
/// #3886). Its failure means tmux was never observed, which carries exactly
/// the same weight as a failed `list-sessions`.
/// Test: this function IS the test.
#[tokio::test]
async fn dedup_refuses_when_the_tmux_server_cannot_be_started() {
    let dir = TempDir::new().unwrap();
    let fake = FakeTmuxDriver::new();
    fake.seeded_names.lock().unwrap().push("tm-dogfood".into());
    let (mgr, live_id, other_id, _ws) = seed_same_path_pair(&dir, fake.clone()).await;

    *fake.ensure_server_up_should_fail.lock().unwrap() = true;
    let mut to_resume = Vec::new();
    let decommissioned = mgr.dedup_stale_duplicates(&mut to_resume).await.unwrap();

    assert!(
        decommissioned.is_empty(),
        "dedup decommissioned {decommissioned:?} while the tmux server was unreachable"
    );
    assert_ne!(
        mgr.get(&live_id).await.unwrap().state,
        ManagedSessionState::Decommissioned
    );
    assert_ne!(
        mgr.get(&other_id).await.unwrap().state,
        ManagedSessionState::Decommissioned
    );
}

/// The tmux-absent fallback driver must not be read as "zero live sessions".
///
/// Why: this is the arm no call-site error handling could have caught, because
/// `NoopTmuxDriver::list_sessions` used to return `Ok(vec![])` — a successful
/// answer from a driver that never reached tmux. The daemon installs it
/// whenever `RealTmuxDriver::discover()` fails, which includes the #5784
/// host-state gate refusing access on a reassigned `$HOME`, not only a missing
/// binary.
/// Test: this function IS the test.
#[tokio::test]
async fn dedup_refuses_on_the_noop_driver_rather_than_reading_zero_as_dead() {
    let dir = TempDir::new().unwrap();
    let noop: std::sync::Arc<dyn super::driver::ManagedTmuxDriver> =
        std::sync::Arc::new(super::real_tmux::NoopTmuxDriver);
    let (mgr, live_id, other_id, _ws) = seed_same_path_pair(&dir, noop).await;

    let mut to_resume = Vec::new();
    let decommissioned = mgr.dedup_stale_duplicates(&mut to_resume).await.unwrap();

    assert!(
        decommissioned.is_empty(),
        "dedup decommissioned {decommissioned:?} on a driver that never reached tmux"
    );
    assert_ne!(
        mgr.get(&live_id).await.unwrap().state,
        ManagedSessionState::Decommissioned,
        "an attached session must not be tombstoned because tmux was unreachable"
    );
    assert_ne!(
        mgr.get(&other_id).await.unwrap().state,
        ManagedSessionState::Decommissioned
    );
}

/// Refusing on an unobservable tmux must not disable dedup on an observable
/// one: the same fixture, answered honestly, still collapses the dead sibling.
///
/// Why: a fail-closed guard that also blocks the legitimate path is not a fix.
/// This pins the boundary — the ONLY difference from
/// `dedup_refuses_when_list_sessions_fails` is that tmux answered.
/// Test: this function IS the test.
#[tokio::test]
async fn dedup_collapses_the_dead_sibling_when_tmux_answers() {
    let dir = TempDir::new().unwrap();
    let fake = FakeTmuxDriver::new();
    fake.seeded_names.lock().unwrap().push("tm-dogfood".into());
    let (mgr, live_id, other_id, _ws) = seed_same_path_pair(&dir, fake.clone()).await;

    let mut to_resume = Vec::new();
    let decommissioned = mgr.dedup_stale_duplicates(&mut to_resume).await.unwrap();

    assert_eq!(
        decommissioned,
        vec![other_id],
        "the dead sibling at the same literal path is still the loser"
    );
    assert_ne!(
        mgr.get(&live_id).await.unwrap().state,
        ManagedSessionState::Decommissioned,
        "the live record survives"
    );
    assert!(
        *fake.ensure_server_up_calls.lock().unwrap() >= 1,
        "the liveness query must run the server-up guard before listing"
    );
}

/// A terminal record whose tmux session is LIVE is left terminal.
///
/// Why: the contradiction is real and reported, but its cause is ambiguous —
/// it is either a wrongly-tombstoned session or the #2777 case of a correctly
/// decommissioned session whose pane lingers as a bare shell the operator may
/// be attached to. Reviving on liveness would resurrect every one of the
/// latter, so `reconcile_on_boot` logs and moves on. This pins that choice so
/// a future change to auto-revive is a deliberate one.
/// Test: this function IS the test.
#[tokio::test]
async fn reconcile_never_revives_a_terminal_record_with_a_live_session() {
    let dir = TempDir::new().unwrap();
    let ws_dir = TempDir::new().unwrap();
    let fake = FakeTmuxDriver::new();
    fake.seeded_names
        .lock()
        .unwrap()
        .push("tm-lingering".into());
    let mgr = SessionManager::new(dir.path(), fake.clone()).await.unwrap();

    let tombstone = rec(
        "tm-lingering",
        Some("proj"),
        Some(&ws_dir.path().to_path_buf()),
        ManagedSessionState::Decommissioned,
        1,
    );
    let tombstone_id = tombstone.id;
    mgr.store.write().await.upsert(tombstone).await.unwrap();

    mgr.reconcile_on_boot(false).await.expect("reconcile");

    assert_eq!(
        mgr.get(&tombstone_id).await.unwrap().state,
        ManagedSessionState::Decommissioned,
        "a live tmux session must not silently revive a terminal record"
    );
}

/// The contradiction is REPORTED — the warning is the whole operator-facing
/// value of leaving the record terminal.
///
/// Why (PR #5856 review, finding 2): the test above executes the `warn!` block
/// but asserts nothing about it, so inverting the `live_names.contains` check
/// or deleting the block outright left it green. A record that is silently
/// left broken is not the designed behavior; a record that is left broken AND
/// named, with the call that fixes it, is.
/// What: captures the reconcile pass through the crate's existing
/// [`trusty_common::log_buffer::LogBufferLayer`] entry point — the pattern
/// `ensure_managed_config_dir_emits_the_frozen_skill_warning` and
/// `log_reload_fallback_separates_corruption_from_a_transient_error` already
/// use — and asserts the line's level, the record it names, and the remedy it
/// carries. `with_subscriber` rather than `with_default` because the call
/// under test is a future: it attaches the dispatcher to each poll instead of
/// to one synchronous closure. Sound here because the `warn!` is emitted
/// inline in `reconcile_on_boot`'s own record loop, on the task that awaits
/// it — a `tokio::spawn`ed emitter would escape this and every other
/// thread-local capture (#5846).
/// Test: this function IS the test.
#[tokio::test]
#[serial_test::serial]
async fn reconcile_warns_that_a_terminal_record_has_a_live_tmux_session() {
    use tracing::instrument::WithSubscriber;
    use tracing_subscriber::layer::SubscriberExt;

    // #4181: `tracing` short-circuits every macro on a process-global
    // MAX_LEVEL that starts at OFF and is raised only when some test installs
    // a GLOBAL default. A thread-local dispatcher never raises it, so without
    // this the capture below would record `[]` whenever no other test in the
    // binary happened to run first. See
    // `ensure_managed_config_dir_emits_the_frozen_skill_warning`.
    static RAISE_MAX_LEVEL: std::sync::Once = std::sync::Once::new();
    RAISE_MAX_LEVEL.call_once(|| {
        let _ = tracing::subscriber::set_global_default(tracing_subscriber::registry());
    });

    let dir = TempDir::new().unwrap();
    let ws_dir = TempDir::new().unwrap();
    let fake = FakeTmuxDriver::new();
    fake.seeded_names
        .lock()
        .unwrap()
        .push("tm-lingering".into());
    let mgr = SessionManager::new(dir.path(), fake.clone()).await.unwrap();

    let tombstone = rec(
        "tm-lingering",
        Some("proj"),
        Some(&ws_dir.path().to_path_buf()),
        ManagedSessionState::Decommissioned,
        1,
    );
    let tombstone_id = tombstone.id;
    mgr.store.write().await.upsert(tombstone).await.unwrap();

    let buffer = trusty_common::log_buffer::LogBuffer::new(64);
    let subscriber = tracing_subscriber::registry().with(
        trusty_common::log_buffer::LogBufferLayer::new(buffer.clone()),
    );
    mgr.reconcile_on_boot(false)
        .with_subscriber(subscriber)
        .await
        .expect("reconcile");

    let lines = buffer.tail(64);
    let line = lines
        .iter()
        .find(|l| l.contains("terminal record has a LIVE tmux session"))
        .unwrap_or_else(|| {
            panic!(
                "reconcile left `tm-lingering` terminal while its tmux session was live and \
                 said nothing — the operator has no way to learn the picker is hiding a \
                 session they are sitting in. Captured lines: {lines:#?}"
            )
        });
    assert!(
        line.contains("WARN"),
        "a hidden-but-live session must not be logged below WARN: {line}"
    );
    assert!(
        line.contains(&tombstone_id.to_string()),
        "the warning must name the record so `reactivate` can be aimed at it: {line}"
    );
    assert!(
        line.contains("/reactivate"),
        "the warning must carry the call that resolves the contradiction: {line}"
    );
}

/// A tmux that cannot be observed must not mark a live session `Stopped`
/// (PR #5856 review, finding 1).
///
/// Why: `dedup_stale_duplicates` was not the only fail-open in this file.
/// `reconcile_on_boot` built its live set with
/// `list_sessions().unwrap_or_else(warn, Vec::new)`, which reads an
/// unobservable tmux as an empty one — every running session falls through to
/// the "gone" arm and is marked `Stopped`, and under `auto_resume` each is
/// queued for a relaunch it does not need. `auto_resume = true` here so the
/// queueing half is exercised, not just the state write.
/// What: the failure is raised AT the liveness query, so the pass provably
/// reaches it. `report.stopped` and the driver's `create_cwd_calls` are the
/// load-bearing assertions — against the pre-fix code the record's own state
/// reads `Active` at the end anyway, because auto-resume respawned the session
/// it had just wrongly stopped. The last assertion pins the other half of the
/// fix: the skip is not an early return, so the dedup / deploy-validate /
/// auto-resume tail still runs (dedup's own `ensure_server_up` is the
/// observable proof it was reached).
/// Test: this function IS the test.
#[tokio::test]
async fn reconcile_refuses_to_stop_sessions_when_tmux_cannot_be_observed() {
    let dir = TempDir::new().unwrap();
    let ws_dir = TempDir::new().unwrap();
    let fake = FakeTmuxDriver::new();
    fake.seeded_names.lock().unwrap().push("tm-attached".into());
    let mgr = SessionManager::new(dir.path(), fake.clone()).await.unwrap();

    let live = rec(
        "tm-attached",
        Some("proj"),
        Some(&ws_dir.path().to_path_buf()),
        ManagedSessionState::Active,
        1,
    );
    let live_id = live.id;
    mgr.store.write().await.upsert(live).await.unwrap();

    *fake.list_sessions_should_fail.lock().unwrap() = true;
    let report = mgr
        .reconcile_on_boot(true)
        .await
        .expect("an unobservable tmux is a skip, not an error to the caller");

    assert!(
        report.stopped.is_empty(),
        "an attached session was reported gone because tmux could not be queried; that list \
         is also the auto-resume queue, so every entry is relaunched next: {:?}",
        report.stopped
    );
    let created = fake.create_cwd_calls.lock().unwrap().clone();
    assert!(
        created.is_empty(),
        "auto-resume respawned a session tmux was never asked about — the operator's own \
         pane gets a fresh launch on top of the one they are sitting in: {created:?}"
    );
    assert_eq!(
        mgr.get(&live_id).await.unwrap().state,
        ManagedSessionState::Active,
        "the record must be left exactly as it was found"
    );
    assert!(
        report.adopted.is_empty() && report.external_adopted.is_empty(),
        "a tmux that never answered cannot have produced a session to adopt"
    );
    assert!(
        *fake.ensure_server_up_calls.lock().unwrap() >= 2,
        "the skip must not be an early return: reconcile's own liveness query and the dedup \
         pass in its tail each run the server-up guard, so the tail was never reached"
    );
}
