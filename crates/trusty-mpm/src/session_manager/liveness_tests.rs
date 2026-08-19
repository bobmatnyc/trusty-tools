//! #2022 coverage: the delete/prune/decommission running-guard is a REAL tmux
//! liveness probe, not a persisted-state check.
//!
//! Why: `session_manager/tests.rs` and `delete_tests.rs` are both close to (or
//! at) the 1500-SLOC test cap; this #2022-specific coverage lives in its own
//! file so none of the three grow past their limit, mirroring the pattern
//! established by `decommission_worktree_tests.rs` / `backfill_tests.rs`.
//! Reuses the sibling `tests` module's `make_manager` / `seed_record` helpers
//! rather than duplicating the scaffolding.
//! What: two tests proving a STALE `Active` record (state says running, but the
//! tmux session backing it has actually died) is removable WITHOUT `--force` —
//! one via `delete_record`, one via `prune_managed` (which also backs `tm
//! session prune`/`tm session decommission`) — while a genuinely LIVE session
//! stays guarded in both paths.
//!
//! #5859 extends the file to the OTHER arm of the same probe: a probe that
//! could not reach tmux at all. Three tests prove `delete_record`,
//! `prune_managed`, and `resume` refuse rather than reading that as death, and
//! a fourth pins the display-only `session_exists` wrapper still answering
//! `false` for it.
//! Test: this file IS the test module; run with `cargo test -p trusty-mpm`.

use tempfile::TempDir;

use super::manager::{ManagedError, ManagedTmuxDriver};
use super::record::{ManagedSessionId, ManagedSessionState};
use super::tests::{make_manager, seed_record};

/// A STALE `Active` record whose tmux session has actually died is deletable
/// WITHOUT `--force` (#2022).
///
/// Why: this IS the bug the ticket fixes. Before the fix, the guard trusted
/// the persisted `state` field: a record that still says `Active` after its
/// tmux session died (crash, `tmux kill-server`, host restart) could never be
/// deleted without `--force`, even though there was nothing left to protect.
/// The guard must track REALITY — a live tmux probe — not a snapshot that can
/// drift from it.
/// What: seeds an `Active` record (which, per `seed_record`, registers a live
/// tmux session on the fake driver), then kills that session directly to
/// simulate the daemon never having observed the crash, and asserts
/// `delete_record(id, force=false)` succeeds and the record is marked `Deleted`.
/// Test: this function IS the test.
#[tokio::test]
async fn delete_record_stale_active_deletable_when_tmux_dead() {
    let dir = TempDir::new().unwrap();
    let (mgr, fake) = make_manager(&dir).await;
    let id = ManagedSessionId::new();
    seed_record(&mgr, &dir, id, ManagedSessionState::Active, false).await;

    // The record still says Active, but the tmux session backing it is gone —
    // e.g. it crashed or was reaped without the daemon observing it.
    fake.kill_session(&format!("tmpm-seed-{id}"))
        .expect("simulate tmux death");

    mgr.delete_record(&id, false).await.expect(
        "a stale Active record whose tmux is actually dead must be deletable without --force",
    );
    // Soft-delete: the record is marked `--deleted--`, kept in the store (#2012).
    assert_eq!(
        mgr.get(&id).await.expect("record still tracked").state,
        ManagedSessionState::Deleted
    );
}

/// A STALE `Active` record whose tmux session has actually died IS pruned
/// (decommissioned) by `prune_managed` WITHOUT `--force`/`include_active` —
/// i.e. `prune`/`decommission` honor the same corrected liveness probe as
/// `delete_record` (#2022).
///
/// Why: `prune_managed`'s fail-closed gate must reflect the SAME reality check
/// as `delete_record` — otherwise `tm session prune`/`tm session decommission`
/// would still refuse to reap a genuinely-dead-but-stale-Active record even
/// after `tm session delete` was fixed to allow it, leaving the three verbs
/// inconsistent.
/// What: seeds one `Active` record whose tmux session is killed immediately
/// after seeding (stale-but-dead) and one `Active` record whose tmux session is
/// left alive (genuinely running), then prunes with `PruneFilter::All` and
/// `include_active=false` (the fail-closed default). Asserts the stale record
/// WAS decommissioned (tmux dead → not running by the new probe) while the
/// genuinely live record is untouched (tmux alive → still guarded).
/// Test: this function IS the test.
#[tokio::test]
async fn prune_stale_active_removable_without_force_when_tmux_dead() {
    let dir = TempDir::new().unwrap();
    let (mgr, fake) = make_manager(&dir).await;

    let stale = ManagedSessionId::new();
    let live = ManagedSessionId::new();
    seed_record(&mgr, &dir, stale, ManagedSessionState::Active, false).await;
    seed_record(&mgr, &dir, live, ManagedSessionState::Active, false).await;

    // The `stale` record's tmux session dies without the daemon observing it;
    // the `live` record's tmux session is left alive (seed_record registered it).
    fake.kill_session(&format!("tmpm-seed-{stale}"))
        .expect("simulate tmux death");

    let outcome = mgr
        .prune_managed(crate::session_manager::PruneFilter::All, false, false, None)
        .await
        .expect("prune all");
    assert_eq!(
        outcome.count(),
        1,
        "only the stale (tmux-dead) Active record is reaped"
    );
    assert_eq!(
        mgr.get(&stale).await.unwrap().state,
        ManagedSessionState::Decommissioned,
        "stale Active with dead tmux must be reaped without --force (#2022)"
    );
    assert_eq!(
        mgr.get(&live).await.unwrap().state,
        ManagedSessionState::Active,
        "a genuinely live (tmux-alive) session must still be guarded"
    );
}

// ---------------------------------------------------------------------------
// #5859: an UNDETERMINABLE liveness probe is not a confirmed-absent session.
// ---------------------------------------------------------------------------

/// `delete_record`'s running-guard REFUSES when the tmux probe itself fails
/// (#5859).
///
/// Why: the guard used to call `session_exists`, which folded a
/// `list-sessions` error into `false`. A transient tmux failure — or the
/// `NoopTmuxDriver` installed whenever `RealTmuxDriver::discover()` fails —
/// therefore read a live, attached session as not-running, and an unforced
/// `tm session delete` dropped it. "Could not tell" must not be spelled the
/// same way as "confirmed dead".
/// What: seeds an `Active` record (whose tmux session the fake driver reports
/// as live), makes `list_sessions` fail, and asserts `delete_record(id,
/// force=false)` returns `TmuxUnavailable` with the record untouched. Pre-fix
/// this returned `Ok` and marked the record `Deleted`.
/// Test: this function IS the test.
#[tokio::test]
async fn delete_record_refuses_when_the_tmux_probe_fails() {
    let dir = TempDir::new().unwrap();
    let (mgr, fake) = make_manager(&dir).await;
    let id = ManagedSessionId::new();
    seed_record(&mgr, &dir, id, ManagedSessionState::Active, false).await;

    // tmux is live and so is the session — but the probe cannot reach it.
    *fake.list_sessions_should_fail.lock().unwrap() = true;

    let err = mgr
        .delete_record(&id, false)
        .await
        .expect_err("an unobservable liveness probe must refuse the delete (#5859)");
    assert!(
        matches!(err, ManagedError::TmuxUnavailable(_)),
        "expected TmuxUnavailable, got {err:?}"
    );
    assert_eq!(
        mgr.get(&id).await.expect("record still tracked").state,
        ManagedSessionState::Active,
        "no record may be touched when liveness could not be established"
    );
}

/// `prune_managed`'s running-guard REFUSES the whole sweep when the tmux probe
/// fails, and `include_active` still short-circuits it (#5859).
///
/// Why: the same fail-open as `delete_record`, reached through `tm session
/// prune` / `tm session decommission`, which additionally tear the WORKSPACE
/// down. The short-circuit half matters too: `decommission_all_ephemeral`
/// passes `include_active=true` and ignores liveness by design, so it must keep
/// working on a host where tmux cannot be observed at all.
/// What: seeds an `Active` record, makes `list_sessions` fail, and asserts the
/// `include_active=false` prune returns `TmuxUnavailable` while the record stays
/// `Active`; then asserts the `include_active=true` prune still reaps it.
/// Pre-fix the first call returned `Ok` and decommissioned the live session.
/// Test: this function IS the test.
#[tokio::test]
async fn prune_refuses_when_the_tmux_probe_fails() {
    let dir = TempDir::new().unwrap();
    let (mgr, fake) = make_manager(&dir).await;
    let id = ManagedSessionId::new();
    seed_record(&mgr, &dir, id, ManagedSessionState::Active, false).await;

    *fake.list_sessions_should_fail.lock().unwrap() = true;

    let err = mgr
        .prune_managed(crate::session_manager::PruneFilter::All, false, false, None)
        .await
        .expect_err("an unobservable liveness probe must refuse the prune (#5859)");
    assert!(
        matches!(err, ManagedError::TmuxUnavailable(_)),
        "expected TmuxUnavailable, got {err:?}"
    );
    assert_eq!(
        mgr.get(&id).await.expect("record still tracked").state,
        ManagedSessionState::Active,
        "a live session must survive a prune whose liveness probe failed"
    );

    // `include_active` never consults the probe, so it is unaffected.
    let outcome = mgr
        .prune_managed(crate::session_manager::PruneFilter::All, false, true, None)
        .await
        .expect("include_active skips the liveness gate entirely");
    assert_eq!(outcome.count(), 1);
}

/// `resume` REFUSES rather than killing and rebuilding the pane when the tmux
/// probe fails (#5859).
///
/// Why: `resume`'s else-branch is destructive — it `kill_session`s and creates
/// a fresh pane. Reading an unobservable probe as "no live pane" destroyed the
/// pane the operator was looking at, along with any sibling window in that tmux
/// session.
/// What: seeds a `Stopped` record, registers its tmux name as live on the fake
/// driver, makes `list_sessions` fail, and asserts `resume` returns
/// `TmuxUnavailable` with no `kill_session` and no `create_session` issued, and
/// the record still `Stopped`. Pre-fix this killed and recreated the pane.
/// Test: this function IS the test.
#[tokio::test]
async fn resume_refuses_when_the_tmux_probe_fails() {
    let dir = TempDir::new().unwrap();
    let (mgr, fake) = make_manager(&dir).await;
    let id = ManagedSessionId::new();
    seed_record(&mgr, &dir, id, ManagedSessionState::Stopped, false).await;

    // The pane survived the runtime exit (#2148) — it is genuinely alive.
    fake.seeded_names
        .lock()
        .unwrap()
        .push(format!("tmpm-seed-{id}"));
    *fake.list_sessions_should_fail.lock().unwrap() = true;

    let err = mgr
        .resume(&id)
        .await
        .expect_err("an unobservable liveness probe must refuse the resume (#5859)");
    assert!(
        matches!(err, ManagedError::TmuxUnavailable(_)),
        "expected TmuxUnavailable, got {err:?}"
    );
    assert!(
        fake.kill_calls.lock().unwrap().is_empty(),
        "resume must not kill a pane it could not prove dead"
    );
    assert!(
        fake.create_cwd_calls.lock().unwrap().is_empty(),
        "resume must not rebuild a pane it could not prove dead"
    );
    assert_eq!(
        mgr.get(&id).await.expect("record still tracked").state,
        ManagedSessionState::Stopped
    );
}

/// The display-only `session_exists` wrapper still reads an unobservable probe
/// as absent (#5859).
///
/// Why: the fix must not turn every status render, name-collision check, and
/// worktree inventory row into a hard error — those callers act on nothing and
/// already tolerate an unknown. Splitting the probe is only safe while this half
/// keeps its old, lenient answer.
/// What: makes `list_sessions` fail and asserts `session_exists` is `false`
/// while `session_exists_checked` is `Err` for the same name.
/// Test: this function IS the test.
#[test]
fn session_exists_reads_an_unobservable_probe_as_absent() {
    let fake = super::tests::FakeTmuxDriver::new();
    *fake.list_sessions_should_fail.lock().unwrap() = true;

    assert!(!fake.session_exists("tmpm-anything"));
    assert!(matches!(
        fake.session_exists_checked("tmpm-anything"),
        Err(ManagedError::TmuxUnavailable(_))
    ));
}
