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
/// `delete_record(id, force=false)` succeeds and the record is gone.
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
    assert!(matches!(
        mgr.get(&id).await,
        Err(ManagedError::SessionNotFound(_))
    ));
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
        .prune_managed(crate::session_manager::PruneFilter::All, false, false)
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
