//! Coverage for [`super`] — the terminal-record retention sweep.
//!
//! Why: this is the only path that hard-deletes a session record without an
//! operator naming it, so every guard that decides NOT to delete needs a test
//! as much as the deletion itself does. Two of them are load-bearing beyond
//! the display bug this feature exists for: the workspace-still-on-disk guard
//! keeps the record in `prune_orphaned_worktrees`'s protected set, and the
//! undated-record guard stops a legacy tombstone being evicted the first time
//! the new code sees it.
//! What: pure [`super::retention_verdict`] cases, then end-to-end sweeps
//! through a real `SessionManager` covering eviction, slot release and reuse,
//! the no-duplicate-`NUM` invariant across that cycle, and a filesystem
//! no-touch assertion.
//! Test: this file IS the test module; run with `cargo test -p trusty-mpm`.

use std::path::PathBuf;

use chrono::{Duration, Utc};
use tempfile::TempDir;

use super::super::record::{ManagedSessionState, SessionRecord};
use super::super::tests::make_manager;
use super::{
    RetentionDebounce, RetentionVerdict, TERMINAL_RECORD_RETENTION_DAYS, retention_verdict,
    workspace_present,
};

/// The window the daemon actually applies. Pinned so a silent change to the
/// constant fails a test rather than quietly shortening retention.
#[test]
fn retention_window_is_seven_days() {
    assert_eq!(TERMINAL_RECORD_RETENTION_DAYS, 7);
}

fn window() -> Duration {
    Duration::days(TERMINAL_RECORD_RETENTION_DAYS)
}

/// A terminal record with no workspace, dated `age_days` ago.
fn terminal_record(state: ManagedSessionState, age_days: i64) -> SessionRecord {
    let mut r = base_record();
    r.state = state;
    r.terminal_at = Some(Utc::now() - Duration::days(age_days));
    r
}

fn base_record() -> SessionRecord {
    SessionRecord {
        id: super::super::record::ManagedSessionId::new(),
        tmux_name: "tm-retention-test".into(),
        cwd: PathBuf::from("/tmp"),
        task: "t".into(),
        state: ManagedSessionState::Active,
        created_at: Utc::now() - Duration::days(365),
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
        worktree_owner: None,
        terminal_at: None,
    }
}

/// Why: a `stopped` session is resumable with its workspace intact, and an
/// `active` one is running right now. Age must never bring either into scope,
/// however ancient `created_at` is (the fixture's is a year old).
#[test]
fn retention_verdict_keeps_live_states() {
    for state in [
        ManagedSessionState::Provisioning,
        ManagedSessionState::Active,
        ManagedSessionState::Stopped,
        ManagedSessionState::Errored,
    ] {
        let mut r = base_record();
        r.state = state.clone();
        r.terminal_at = Some(Utc::now() - Duration::days(400));
        assert_eq!(
            retention_verdict(&r, false, Utc::now(), window()),
            RetentionVerdict::Keep,
            "{state} must never be evicted"
        );
    }
}

/// Why: THE guard that keeps a record-only eviction from becoming a filesystem
/// deletion by proxy. `prune_orphaned_worktrees` protects a worktree by finding
/// its path among the store's `workspace_path`s; dropping the record drops the
/// path from every read of that set at once.
#[test]
fn retention_verdict_keeps_record_whose_workspace_still_exists() {
    let mut r = terminal_record(ManagedSessionState::Decommissioned, 400);
    r.workspace_path = Some(PathBuf::from("/tmp/some-worktree"));
    assert_eq!(
        retention_verdict(&r, true, Utc::now(), window()),
        RetentionVerdict::Keep,
        "a record whose worktree is still on disk keeps protecting it"
    );
    // Same record, directory gone: now it is safe to evict.
    assert_eq!(
        retention_verdict(&r, false, Utc::now(), window()),
        RetentionVerdict::Evict
    );
}

/// Why: every record written before this feature has `terminal_at == None`.
/// Inferring a death time from `created_at`/`last_activity_at` would evict the
/// whole legacy backlog on the first sweep with zero retention.
#[test]
fn retention_verdict_stamps_undated_terminal_record() {
    let now = Utc::now();
    for state in [
        ManagedSessionState::Decommissioned,
        ManagedSessionState::Deleted,
    ] {
        let mut r = base_record();
        r.state = state;
        r.terminal_at = None;
        r.last_activity_at = Some(now - Duration::days(90));
        assert_eq!(
            retention_verdict(&r, false, now, window()),
            RetentionVerdict::Stamp(now - Duration::days(90)),
            "the stamp carries the inferred date, not `now`"
        );
    }
}

/// Why: #3034's tombstone must survive the whole window — this is the case
/// where the retention change must be invisible.
#[test]
fn retention_verdict_keeps_record_inside_window() {
    for age in [0, 1, 6] {
        let r = terminal_record(ManagedSessionState::Decommissioned, age);
        assert_eq!(
            retention_verdict(&r, false, Utc::now(), window()),
            RetentionVerdict::Keep,
            "{age}-day-old tombstone is inside the window"
        );
    }
}

#[test]
fn retention_verdict_evicts_record_outside_window() {
    for state in [
        ManagedSessionState::Decommissioned,
        ManagedSessionState::Deleted,
    ] {
        let mut r = terminal_record(state.clone(), 8);
        assert_eq!(
            retention_verdict(&r, false, Utc::now(), window()),
            RetentionVerdict::Evict,
            "{state} 8 days past terminal is evictable"
        );
        // Exactly at the boundary evicts too — `now - at >= retention`.
        r.terminal_at = Some(Utc::now() - window());
        assert_eq!(
            retention_verdict(&r, false, Utc::now(), window()),
            RetentionVerdict::Evict
        );
    }
}

/// Seed a manager with one session, force it into `state`, and date its
/// `terminal_at` `age_days` ago. Returns the record's id.
async fn seed_terminal(
    mgr: &super::super::manager::SessionManager,
    task: &str,
    state: ManagedSessionState,
    age_days: i64,
) -> super::super::record::ManagedSessionId {
    let rec = mgr
        .create(task.into(), None, None, None, None, None)
        .await
        .expect("create");
    let mut updated = mgr.get(&rec.id).await.expect("get");
    updated.state = state;
    updated.terminal_at = Some(Utc::now() - Duration::days(age_days));
    // Clear any provisioned workspace so the on-disk guard is not what the
    // eviction assertions are actually measuring.
    updated.workspace_path = None;
    mgr.store
        .write()
        .await
        .upsert(updated)
        .await
        .expect("seed terminal record");
    rec.id
}

/// Run the sweep to the point where it can actually delete.
///
/// The debounce requires a candidate on TWO consecutive sweeps, so a test
/// asserting an eviction must tick twice with the same gate. Returns the second
/// sweep's outcome — the one that does the deleting.
async fn sweep_twice(
    mgr: &super::super::manager::SessionManager,
    now: chrono::DateTime<Utc>,
) -> super::RetentionOutcome {
    let mut gate = RetentionDebounce::new();
    mgr.sweep_terminal_records(now, window(), &mut gate)
        .await
        .expect("first sweep");
    mgr.sweep_terminal_records(now, window(), &mut gate)
        .await
        .expect("second sweep")
}

/// Why: the core promise — a record inside the window survives, one outside it
/// does not, and a live session is untouched by either.
#[tokio::test]
async fn sweep_evicts_only_records_past_the_window() {
    let dir = crate::test_support::hermetic_temp_dir();
    let (mgr, _fake) = make_manager(&dir).await;

    let fresh = seed_terminal(&mgr, "fresh", ManagedSessionState::Decommissioned, 1).await;
    let stale = seed_terminal(&mgr, "stale", ManagedSessionState::Decommissioned, 30).await;
    let deleted_stale = seed_terminal(&mgr, "gone", ManagedSessionState::Deleted, 9).await;
    let live = mgr
        .create("live".into(), None, None, None, None, None)
        .await
        .expect("create live");

    let outcome = sweep_twice(&mgr, Utc::now()).await;

    assert_eq!(outcome.evicted.len(), 2, "{outcome:?}");
    assert!(outcome.evicted.contains(&stale));
    assert!(outcome.evicted.contains(&deleted_stale));
    assert!(
        mgr.get(&fresh).await.is_ok(),
        "in-window tombstone survives"
    );
    assert!(mgr.get(&live.id).await.is_ok(), "live session untouched");
    assert!(mgr.get(&stale).await.is_err(), "stale record is gone");
}

/// Why: an in-window terminal record must still render as a `-- deleted --`
/// tombstone at its original slot — #3034's guarantee, unchanged inside the
/// window. The tombstone appears once the record leaves the store, so this
/// drives the real `compact_record` path for the in-window record and asserts
/// the sweep did not disturb the slot.
#[tokio::test]
async fn sweep_preserves_the_in_window_tombstone_slot() {
    let dir = crate::test_support::hermetic_temp_dir();
    let (mgr, _fake) = make_manager(&dir).await;

    let fresh = seed_terminal(&mgr, "fresh", ManagedSessionState::Decommissioned, 1).await;
    let snap = mgr.numbered_snapshot(&mgr.list().await).await;
    let slot = snap
        .iter()
        .find(|s| s.record.as_ref().map(|r| r.id) == Some(fresh))
        .expect("observed")
        .slot;

    // An explicit compaction (the `tm sessions prune` path) removes the record
    // while the slot stays reserved — the tombstone #3034 specifies.
    mgr.compact_record(&fresh).await.expect("compact");
    sweep_twice(&mgr, Utc::now()).await;

    let after = mgr.numbered_snapshot(&mgr.list().await).await;
    let row = after
        .iter()
        .find(|s| s.slot == slot)
        .expect("slot still present");
    assert!(
        row.record.is_none(),
        "renders as a tombstone, not a session"
    );
}

/// Why: releasing the slot is what makes `NUM` fall. This pins the release, the
/// reuse, and the no-duplicate invariant across the whole cycle.
#[tokio::test]
async fn sweep_releases_the_evicted_slot() {
    let dir = crate::test_support::hermetic_temp_dir();
    let (mgr, _fake) = make_manager(&dir).await;

    let stale = seed_terminal(&mgr, "stale", ManagedSessionState::Decommissioned, 30).await;
    let keeper = mgr
        .create("keeper".into(), None, None, None, None, None)
        .await
        .expect("create keeper");

    let before = mgr.numbered_snapshot(&mgr.list().await).await;
    let stale_slot = before
        .iter()
        .find(|s| s.record.as_ref().map(|r| r.id) == Some(stale))
        .expect("stale observed")
        .slot;
    let keeper_slot = before
        .iter()
        .find(|s| s.record.as_ref().map(|r| r.id) == Some(keeper.id))
        .expect("keeper observed")
        .slot;

    sweep_twice(&mgr, Utc::now()).await;

    let after = mgr.numbered_snapshot(&mgr.list().await).await;
    assert!(
        after.iter().all(|s| s.slot != stale_slot),
        "evicted slot leaves the listing entirely — no phantom tombstone row"
    );
    assert_eq!(
        after
            .iter()
            .find(|s| s.record.as_ref().map(|r| r.id) == Some(keeper.id))
            .expect("keeper still listed")
            .slot,
        keeper_slot,
        "a surviving session's NUM never shifts"
    );

    // The freed number is available again, and nothing ends up sharing a NUM.
    let fresh = mgr
        .create("fresh".into(), None, None, None, None, None)
        .await
        .expect("create fresh");
    let reused = mgr.numbered_snapshot(&mgr.list().await).await;
    assert_eq!(
        reused
            .iter()
            .find(|s| s.record.as_ref().map(|r| r.id) == Some(fresh.id))
            .expect("fresh listed")
            .slot,
        stale_slot,
        "the released slot is reusable"
    );
    let mut slots: Vec<u32> = reused.iter().map(|s| s.slot).collect();
    let total = slots.len();
    slots.sort_unstable();
    slots.dedup();
    assert_eq!(slots.len(), total, "no duplicate NUM after reallocation");
}

/// Why: the hard requirement. Eviction is a record-store operation; a retention
/// sweep that reached the filesystem would be a data-loss defect. This seeds a
/// stale terminal record whose workspace directory still exists and asserts BOTH
/// that the sweep leaves the directory alone AND that it declines to evict the
/// record while the directory is there — the guard that keeps
/// `prune_orphaned_worktrees`'s protected set intact.
#[tokio::test]
async fn sweep_never_touches_the_filesystem() {
    let dir = crate::test_support::hermetic_temp_dir();
    let (mgr, _fake) = make_manager(&dir).await;

    let workspace = TempDir::new().expect("workspace");
    let canary = workspace.path().join("unsaved-work.txt");
    std::fs::write(&canary, b"do not delete me").expect("write canary");

    let id = seed_terminal(&mgr, "stale", ManagedSessionState::Decommissioned, 400).await;
    let mut rec = mgr.get(&id).await.expect("get");
    rec.workspace_path = Some(workspace.path().to_path_buf());
    mgr.store.write().await.upsert(rec).await.expect("re-seed");

    let outcome = sweep_twice(&mgr, Utc::now()).await;

    assert!(
        outcome.evicted.is_empty(),
        "a record whose workspace is still on disk is never evicted: {outcome:?}"
    );
    assert!(workspace.path().exists(), "workspace directory survives");
    assert!(canary.exists(), "file inside the workspace survives");
    assert_eq!(
        std::fs::read(&canary).expect("read canary"),
        b"do not delete me",
        "file contents untouched"
    );
    assert!(mgr.get(&id).await.is_ok(), "record itself survives too");
}

/// Why: the backlog this feature exists to clear is entirely legacy records —
/// 76 of 116 on the reporting machine. Stamping them `now` would grandfather
/// every one for a further seven days, so the owner would install the fix and
/// still see the same `NUM`. An old record must be dated from its own history
/// and become eligible on the very next sweep.
/// What: seeds a legacy record whose last activity was 400 days ago, asserts
/// sweep 1 backfills that date rather than deleting outright (so the inference
/// is on disk before anything acts on it), then that the record goes through
/// the ordinary arm-and-evict path with no wait.
/// Test: this test.
#[tokio::test]
async fn sweep_backfills_an_old_legacy_record_and_evicts_it_without_waiting() {
    let dir = crate::test_support::hermetic_temp_dir();
    let (mgr, _fake) = make_manager(&dir).await;

    let rec = mgr
        .create("legacy".into(), None, None, None, None, None)
        .await
        .expect("create");
    let mut legacy = mgr.get(&rec.id).await.expect("get");
    legacy.state = ManagedSessionState::Decommissioned;
    legacy.created_at = Utc::now() - Duration::days(400);
    legacy.last_activity_at = Some(Utc::now() - Duration::days(400));
    legacy.terminal_at = None;
    legacy.workspace_path = None;
    mgr.store.write().await.upsert(legacy).await.expect("seed");

    let mut gate = RetentionDebounce::new();
    let first = mgr
        .sweep_terminal_records(Utc::now(), window(), &mut gate)
        .await
        .expect("first sweep");
    assert_eq!(first.stamped, 1, "{first:?}");
    assert!(
        first.evicted.is_empty(),
        "the inferred date is written before anything acts on it: {first:?}"
    );
    let stamped = mgr.get(&rec.id).await.expect("still present").terminal_at;
    let age = Utc::now() - stamped.expect("stamped");
    assert!(
        age > Duration::days(399),
        "backfilled from the record's own history, not from `now`: {stamped:?}"
    );

    // Already past the window, so it arms on the next sweep and goes on the one
    // after — no seven-day wait.
    let second = mgr
        .sweep_terminal_records(Utc::now(), window(), &mut gate)
        .await
        .expect("second sweep");
    assert!(second.evicted.is_empty(), "armed only: {second:?}");
    let third = mgr
        .sweep_terminal_records(Utc::now(), window(), &mut gate)
        .await
        .expect("third sweep");
    assert_eq!(
        third.evicted,
        vec![rec.id],
        "evicted without waiting a window"
    );
}

/// Why: the inference must not sweep away a session decommissioned yesterday
/// just because it also predates the field. A recent legacy record keeps its
/// full window, which is the #3034 tombstone guarantee.
/// What: seeds a legacy record active two days ago, asserts it is backfilled to
/// that date and survives repeated sweeps, then that it goes once the window
/// has genuinely elapsed.
/// Test: this test.
#[tokio::test]
async fn sweep_gives_a_recent_legacy_record_its_full_window() {
    let dir = crate::test_support::hermetic_temp_dir();
    let (mgr, _fake) = make_manager(&dir).await;

    let rec = mgr
        .create("recent".into(), None, None, None, None, None)
        .await
        .expect("create");
    let mut legacy = mgr.get(&rec.id).await.expect("get");
    legacy.state = ManagedSessionState::Decommissioned;
    legacy.created_at = Utc::now() - Duration::days(2);
    legacy.last_activity_at = Some(Utc::now() - Duration::days(2));
    legacy.terminal_at = None;
    legacy.workspace_path = None;
    mgr.store.write().await.upsert(legacy).await.expect("seed");

    let mut gate = RetentionDebounce::new();
    for pass in 0..3 {
        let outcome = mgr
            .sweep_terminal_records(Utc::now(), window(), &mut gate)
            .await
            .expect("sweep");
        assert!(
            outcome.evicted.is_empty(),
            "a 2-day-old tombstone must survive (pass {pass}): {outcome:?}"
        );
    }
    assert!(mgr.get(&rec.id).await.is_ok(), "still in the store");

    // …and it does go once its own window has actually elapsed.
    let later = sweep_twice(&mgr, Utc::now() + Duration::days(6)).await;
    assert_eq!(later.evicted, vec![rec.id]);
}

/// Why: a timestamp in the future is not evidence of anything — clock skew, or
/// a corrupt record. Dating from it would compute a negative age; worse, any
/// scheme that treated "unusable" as "old" would delete on the strength of a
/// value that cannot be true. Such a record gets `now` and a full window.
/// What: seeds a legacy record whose `created_at` and `last_activity_at` are
/// both a year ahead, asserts the stamp is `now` (not the future value) and
/// that it is not evicted.
/// Test: this test.
#[tokio::test]
async fn sweep_stamps_now_when_a_legacy_record_has_no_usable_signal() {
    let dir = crate::test_support::hermetic_temp_dir();
    let (mgr, _fake) = make_manager(&dir).await;

    let rec = mgr
        .create("skewed".into(), None, None, None, None, None)
        .await
        .expect("create");
    let mut legacy = mgr.get(&rec.id).await.expect("get");
    legacy.state = ManagedSessionState::Decommissioned;
    legacy.created_at = Utc::now() + Duration::days(365);
    legacy.last_activity_at = Some(Utc::now() + Duration::days(365));
    legacy.terminal_at = None;
    legacy.workspace_path = None;
    mgr.store.write().await.upsert(legacy).await.expect("seed");

    let now = Utc::now();
    let mut gate = RetentionDebounce::new();
    let first = mgr
        .sweep_terminal_records(now, window(), &mut gate)
        .await
        .expect("first sweep");
    assert_eq!(first.stamped, 1);
    assert_eq!(
        mgr.get(&rec.id).await.expect("present").terminal_at,
        Some(now),
        "an unusable signal falls back to `now`, not to the future value"
    );

    let second = mgr
        .sweep_terminal_records(now, window(), &mut gate)
        .await
        .expect("second sweep");
    assert!(second.evicted.is_empty(), "and it gets a full window");
}

/// Why: the backfill must not open a path around the workspace guard. An old
/// legacy record whose directory still exists is exactly the shape that guard
/// exists for — its `workspace_path` is what keeps `prune_orphaned_worktrees`
/// from deleting the worktree — and the inference gives it a date old enough to
/// evict if the guard were checked in the wrong order.
/// What: seeds a 400-day legacy record with a real directory and a canary file,
/// sweeps three times, and asserts nothing is stamped or evicted and the
/// directory survives intact.
/// Test: this test.
#[tokio::test]
async fn sweep_backfill_still_spares_a_legacy_record_whose_workspace_exists() {
    let dir = crate::test_support::hermetic_temp_dir();
    let (mgr, _fake) = make_manager(&dir).await;
    let workspace = TempDir::new().expect("workspace");
    let canary = workspace.path().join("unsaved-work.txt");
    std::fs::write(&canary, b"do not delete me").expect("write canary");

    let rec = mgr
        .create("legacy-with-workspace".into(), None, None, None, None, None)
        .await
        .expect("create");
    let mut legacy = mgr.get(&rec.id).await.expect("get");
    legacy.state = ManagedSessionState::Decommissioned;
    legacy.created_at = Utc::now() - Duration::days(400);
    legacy.last_activity_at = Some(Utc::now() - Duration::days(400));
    legacy.terminal_at = None;
    legacy.workspace_path = Some(workspace.path().to_path_buf());
    mgr.store.write().await.upsert(legacy).await.expect("seed");

    let mut gate = RetentionDebounce::new();
    for pass in 0..3 {
        let outcome = mgr
            .sweep_terminal_records(Utc::now(), window(), &mut gate)
            .await
            .expect("sweep");
        assert!(
            outcome.is_empty(),
            "the workspace guard runs BEFORE the backfill (pass {pass}): {outcome:?}"
        );
    }
    assert!(mgr.get(&rec.id).await.is_ok(), "record survives");
    assert!(canary.exists(), "and so does the work inside its workspace");
}

// ── phase 3: re-validation against the current store ────────────────────────
//
// These reach `revalidate_for_eviction` directly. They have to: inside the full
// sweep, phase 1 re-derives candidates every tick, so anything that changes
// between ticks is dropped there and the phase-3 loop body never executes —
// confirmed by instrumenting the loop. The candidate list is a plain `Vec<Id>`,
// exactly what phase 2 produces, and handing it one computed from an older view
// of the store IS the stale snapshot these arms defend against.

/// Why: the passing arm — re-validation must not become a blanket refusal to
/// delete. If this went green while the two below also went green, the guard
/// would be indistinguishable from "never evict anything".
/// What: a candidate that is still terminal, still stale, and still has no
/// workspace survives re-validation.
/// Test: this test.
#[tokio::test]
async fn revalidate_keeps_a_still_evictable_record() {
    let dir = crate::test_support::hermetic_temp_dir();
    let (mgr, _fake) = make_manager(&dir).await;
    let stale = seed_terminal(&mgr, "stale", ManagedSessionState::Decommissioned, 30).await;

    let survivors = mgr
        .revalidate_for_eviction(vec![stale], Utc::now(), window())
        .await;
    assert_eq!(
        survivors,
        vec![stale],
        "an unchanged candidate still evicts"
    );
}

/// Why: THE stale-snapshot guard. A record reactivated after the snapshot was
/// taken — by another process, or by this daemon between the phases — must not
/// be deleted on the strength of a reading that is no longer true. This drives
/// the `Ok(_)` arm that leaves the record in place.
/// What: builds a candidate list from a record that IS evictable, then
/// reactivates it through the real `mark_reactivated` path before calling
/// re-validation, standing in for a write that landed after the snapshot.
/// Test: this test.
#[tokio::test]
async fn revalidate_drops_a_record_reactivated_after_the_snapshot() {
    let dir = crate::test_support::hermetic_temp_dir();
    let (mgr, _fake) = make_manager(&dir).await;
    let workspace = TempDir::new().expect("workspace");

    let id = seed_terminal(&mgr, "stale", ManagedSessionState::Decommissioned, 30).await;
    // The snapshot said Evict…
    assert_eq!(
        mgr.revalidate_for_eviction(vec![id], Utc::now(), window())
            .await,
        vec![id]
    );

    // …then the reactivation lands. `mark_reactivated` needs a real directory.
    let mut rec = mgr.get(&id).await.expect("get");
    rec.workspace_path = Some(workspace.path().to_path_buf());
    mgr.store.write().await.upsert(rec).await.expect("re-seed");
    mgr.mark_reactivated(&id).await.expect("reactivate");

    let survivors = mgr
        .revalidate_for_eviction(vec![id], Utc::now(), window())
        .await;
    assert!(
        survivors.is_empty(),
        "a candidate that went live after the snapshot must not be deleted: {survivors:?}"
    );
    assert!(mgr.get(&id).await.is_ok(), "and it is still in the store");
}

/// Why: the other non-evicting arm. A concurrent prune removing the record
/// first is a race we lose harmlessly, not an error — but it must drop out of
/// the delete list rather than being counted as evicted, or the sweep reports
/// and releases a slot for something it never removed.
/// What: passes an id that is not in the store at all.
/// Test: this test.
#[tokio::test]
async fn revalidate_drops_a_record_a_concurrent_prune_already_removed() {
    let dir = crate::test_support::hermetic_temp_dir();
    let (mgr, _fake) = make_manager(&dir).await;
    let id = seed_terminal(&mgr, "stale", ManagedSessionState::Decommissioned, 30).await;
    mgr.compact_record(&id).await.expect("concurrent prune");

    let survivors = mgr
        .revalidate_for_eviction(vec![id], Utc::now(), window())
        .await;
    assert!(
        survivors.is_empty(),
        "an already-removed id is not something this sweep evicted: {survivors:?}"
    );
}

/// Why: `Path::exists()` collapses "cannot determine" into "not there", and
/// "not there" is what evicts. A permission error on an ancestor, a stale NFS
/// handle, or `EIO` from a departed volume would therefore drop the record —
/// and with it the `workspace_path` that keeps `prune_orphaned_worktrees` from
/// deleting the worktree. The probe is injected rather than provoked with real
/// filesystem permissions so this is deterministic on every platform and under
/// any uid, including root.
/// What: asserts an `Err` probe reports PRESENT, so the verdict is `Keep`;
/// `Ok(true)` is present, `Ok(false)` absent, and a `None` path is absent.
/// Test: this test.
#[test]
fn workspace_present_treats_an_undetermined_path_as_present() {
    let path = PathBuf::from("/definitely/unreadable");
    assert!(
        workspace_present(Some(&path), |_| Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "EACCES"
        ))),
        "an undetermined path must count as PRESENT so the record is kept"
    );
    assert!(workspace_present(Some(&path), |_| Ok(true)));
    assert!(!workspace_present(Some(&path), |_| Ok(false)));
    assert!(
        !workspace_present(None, |_| Ok(true)),
        "no path, nothing to protect"
    );

    // …and the verdict that consumes it keeps the record.
    let mut r = terminal_record(ManagedSessionState::Decommissioned, 400);
    r.workspace_path = Some(path.clone());
    let undetermined = workspace_present(Some(&path), |_| Err(std::io::Error::other("EIO")));
    assert_eq!(
        retention_verdict(&r, undetermined, Utc::now(), window()),
        RetentionVerdict::Keep,
        "a stat error must never evict"
    );
}

/// Why: the two sibling destructive sweeps in the same orphan-GC tick each
/// require two consecutive observations, and retention deletes something no
/// less permanent. A single observation would act on one filesystem reading —
/// an unmounted volume answers `Ok(false)` truthfully and no error handling
/// catches it.
/// What: asserts the first sweep evicts nothing and the second one does.
/// Test: this test.
#[tokio::test]
async fn debounce_requires_two_consecutive_observations() {
    let dir = crate::test_support::hermetic_temp_dir();
    let (mgr, _fake) = make_manager(&dir).await;
    let stale = seed_terminal(&mgr, "stale", ManagedSessionState::Decommissioned, 30).await;

    let mut gate = RetentionDebounce::new();
    let first = mgr
        .sweep_terminal_records(Utc::now(), window(), &mut gate)
        .await
        .expect("first sweep");
    assert!(
        first.evicted.is_empty(),
        "one observation is never enough: {first:?}"
    );
    assert!(mgr.get(&stale).await.is_ok(), "record survives sweep one");

    let second = mgr
        .sweep_terminal_records(Utc::now(), window(), &mut gate)
        .await
        .expect("second sweep");
    assert_eq!(second.evicted, vec![stale]);
}

/// Why: a candidate that stops being one — its volume remounts, or an operator
/// reactivates it — must not stay armed and get deleted on a later tick.
/// What: drives the gate directly: arm, lapse, re-arm, and assert only a
/// genuinely consecutive pair confirms.
/// Test: this test.
#[test]
fn debounce_disarms_a_candidate_that_lapses() {
    let id = super::super::record::ManagedSessionId::new();
    let other = super::super::record::ManagedSessionId::new();
    let mut gate = RetentionDebounce::new();

    assert!(
        gate.confirm(&[id]).is_empty(),
        "first observation never confirms"
    );
    // The candidate lapses for one tick…
    assert!(gate.confirm(&[other]).is_empty());
    // …so its next appearance is a FIRST observation again, not a second.
    assert!(
        gate.confirm(&[id]).is_empty(),
        "a lapse must re-arm from zero"
    );
    assert_eq!(gate.confirm(&[id]), vec![id], "two in a row confirms");
}

/// Why: an armed candidate that gets reactivated before the deleting sweep must
/// survive. Be precise about WHICH guard saves it, because the name this test
/// first carried claimed the wrong one: phase 1 re-derives candidates from
/// scratch every tick, so a record reactivated BETWEEN sweeps is classified
/// `Keep` there and never reaches phase 3's re-validation at all — verified
/// with a marker in the phase-3 loop, which this test does not reach. The
/// debounce plus phase 1 is what protects it here. Phase 3's own arms are
/// covered by the `revalidate_*` tests below.
/// What: arms the gate on a stale terminal record, reactivates it through the
/// real `mark_reactivated` path between the two sweeps, then asserts the second
/// sweep leaves it alone with its stamp cleared.
/// Test: this test.
#[tokio::test]
async fn sweep_spares_a_record_reactivated_between_sweeps() {
    let dir = crate::test_support::hermetic_temp_dir();
    let (mgr, _fake) = make_manager(&dir).await;
    let workspace = TempDir::new().expect("workspace");

    let id = seed_terminal(&mgr, "stale", ManagedSessionState::Decommissioned, 30).await;
    let mut gate = RetentionDebounce::new();
    let first = mgr
        .sweep_terminal_records(Utc::now(), window(), &mut gate)
        .await
        .expect("first sweep");
    assert!(first.evicted.is_empty(), "armed, not yet evicted");

    // A reactivation lands between the two sweeps. `mark_reactivated` needs a
    // real directory to resume into.
    let mut rec = mgr.get(&id).await.expect("get");
    rec.workspace_path = Some(workspace.path().to_path_buf());
    mgr.store.write().await.upsert(rec).await.expect("re-seed");
    mgr.mark_reactivated(&id).await.expect("reactivate");

    let second = mgr
        .sweep_terminal_records(Utc::now(), window(), &mut gate)
        .await
        .expect("second sweep");
    assert!(
        second.evicted.is_empty(),
        "a record reactivated after the snapshot must survive: {second:?}"
    );
    let live = mgr.get(&id).await.expect("record still present");
    assert_eq!(live.state, ManagedSessionState::Active);
    assert_eq!(
        live.terminal_at, None,
        "reactivation clears the retention stamp"
    );
}

// ── the one-time backfill's inferred timestamp ──────────────────────────────

/// Why: the inference decides how long a legacy record's window is, so which
/// signal it picks — and in which direction it errs — is the whole behaviour.
/// Taking the LATEST evidence keeps it as conservative as the data allows.
/// What: `last_activity_at` wins when set; `created_at` is the fallback; and an
/// implausibly early `last_activity_at` never drags the date before
/// `created_at`.
/// Test: this test.
#[test]
fn inferred_terminal_at_uses_the_latest_evidence_of_life() {
    let now = Utc::now();
    let mut r = base_record();

    r.created_at = now - Duration::days(400);
    r.last_activity_at = Some(now - Duration::days(30));
    assert_eq!(
        super::inferred_terminal_at(&r, now),
        now - Duration::days(30),
        "activity is the better signal when present"
    );

    r.last_activity_at = None;
    assert_eq!(
        super::inferred_terminal_at(&r, now),
        now - Duration::days(400),
        "creation is the fallback"
    );

    // A record whose recorded activity predates its own creation is incoherent;
    // never take the earlier of the two, which would shorten the window.
    r.last_activity_at = Some(now - Duration::days(900));
    assert_eq!(
        super::inferred_terminal_at(&r, now),
        now - Duration::days(400),
        "never earlier than creation"
    );
}

/// Why: a future timestamp is clock skew or corruption, not evidence. Dating
/// from it must not happen in either direction — neither trusting it nor
/// treating "unusable" as "old".
/// What: both timestamps a year ahead resolve to `now`.
/// Test: this test.
#[test]
fn inferred_terminal_at_falls_back_to_now_for_a_future_timestamp() {
    let now = Utc::now();
    let mut r = base_record();
    r.created_at = now + Duration::days(365);
    r.last_activity_at = Some(now + Duration::days(365));
    assert_eq!(super::inferred_terminal_at(&r, now), now);
}
