//! Tests for the steady-state memory-pressure enforcement ticker (issue
//! #2846) and its PR-review follow-ups: hysteresis (`should_reclaim_now`)
//! and the opt-in self-restart last resort (`run_memory_pressure_tick`'s
//! restart branch).
//!
//! Why: `memguard::tests` already covers the pure threshold decision
//! (`over_high_water`) and the config readers with synthetic values. This
//! file covers two more things extracted from `run_memory_pressure_tick` for
//! exactly the same reason — a real process's RSS cannot be driven to
//! specific values on demand:
//!   1. `should_reclaim_now` — the pure hysteresis gate — with synthetic
//!      before/after RSS pairs.
//!   2. The orchestration layer's restart branch, which DOES need a real
//!      `SearchAppState` (to observe `shutdown_tx`) but sidesteps the
//!      "drive real RSS to a specific value" problem by setting the soft
//!      limit to 1 MB — any real test-process RSS (always several MB) is
//!      then unconditionally "over the hard limit" at pct=100, so the
//!      restart branch's precondition is deterministic without faking the
//!      sampler.
//!
//! Isolation: the restart-branch tests mutate the process-global `memguard`
//! memory-limit atomic and the `TRUSTY_MEMORY_RESTART_ON_LIMIT` env var, both
//! saved and restored at the end of each test (mirrors
//! `memguard::tests::test_runtime_set_limit`'s save/restore convention for
//! the atomic, and `tests_idle_evict.rs`'s `TRUSTY_HNSW_REVIEW_IDLE` pattern
//! for the env var — this crate has no other test touching
//! `TRUSTY_MEMORY_RESTART_ON_LIMIT`). `#[serial_test::serial]` reduces (does
//! not eliminate) cross-test interference risk on the shared atomic, matching
//! `residency_sweep_tests.rs`'s convention for shared process env/global
//! state.
//!
//! Test: `cargo test -p trusty-search -- memory_pressure`

use super::*;
use crate::core::memguard;
use crate::core::registry::IndexRegistry;

// ---------------------------------------------------------------------------
// `should_reclaim_now` — pure hysteresis gate
// ---------------------------------------------------------------------------

/// `0` is the "no reclaim yet this pressure episode" sentinel — the first
/// crossing of the high-water mark must always reclaim, regardless of RSS.
#[test]
fn hysteresis_first_crossing_always_reclaims() {
    assert!(should_reclaim_now(500, 0));
    assert!(
        should_reclaim_now(1, 0),
        "even a tiny RSS reclaims on first crossing"
    );
}

/// Flat or falling RSS relative to the last sweep's outcome must NOT
/// re-trigger a sweep — this is the thrash guard the #2846 PR review asked
/// for (reclaim → rehydrate → reclaim every tick with no memory benefit).
#[test]
fn hysteresis_skips_when_rss_has_not_risen() {
    assert!(
        !should_reclaim_now(500, 500),
        "RSS unchanged since the last sweep must not re-trigger"
    );
    assert!(
        !should_reclaim_now(499, 500),
        "RSS below the last sweep's outcome must not re-trigger"
    );
}

/// Once RSS climbs past where the last sweep left it, caches have measurably
/// repopulated — the next sweep must fire.
#[test]
fn hysteresis_reclaims_again_once_rss_has_risen() {
    assert!(should_reclaim_now(501, 500));
    assert!(should_reclaim_now(10_000, 500));
}

// ---------------------------------------------------------------------------
// `run_memory_pressure_tick` — restart-branch orchestration
// ---------------------------------------------------------------------------

/// RAII-style guard that saves the global `memory_limit_mb` atomic and the
/// `TRUSTY_MEMORY_RESTART_ON_LIMIT` env var on construction and restores both
/// on drop, so a test panic still leaves global state clean for the rest of
/// the binary.
struct MemGuardEnv {
    prior_limit: Option<u64>,
    prior_restart_env: Option<String>,
}

impl MemGuardEnv {
    fn capture() -> Self {
        Self {
            prior_limit: memguard::memory_limit_mb(),
            prior_restart_env: std::env::var("TRUSTY_MEMORY_RESTART_ON_LIMIT").ok(),
        }
    }
}

impl Drop for MemGuardEnv {
    fn drop(&mut self) {
        memguard::set_memory_limit_mb(self.prior_limit);
        // SAFETY: this test module is the sole reader/writer of
        // TRUSTY_MEMORY_RESTART_ON_LIMIT in this crate's test suite.
        unsafe {
            match &self.prior_restart_env {
                Some(v) => std::env::set_var("TRUSTY_MEMORY_RESTART_ON_LIMIT", v),
                None => std::env::remove_var("TRUSTY_MEMORY_RESTART_ON_LIMIT"),
            }
        }
    }
}

/// The opt-in self-restart branch must actually signal `shutdown_tx` — the
/// #2846 PR review's LOW test-seam ask (rather than only relying on #1746's
/// unrelated graceful-shutdown-DRAIN coverage, which tests what happens
/// AFTER a shutdown is signalled, not whether the memory-pressure branch
/// signals one at all).
#[tokio::test]
#[serial_test::serial]
async fn restart_branch_signals_shutdown_tx_when_still_over_hard_limit() {
    let _guard = MemGuardEnv::capture();
    memguard::set_memory_limit_mb(Some(1));
    // SAFETY: see `MemGuardEnv`'s doc comment.
    unsafe { std::env::set_var("TRUSTY_MEMORY_RESTART_ON_LIMIT", "1") };

    let state = Arc::new(SearchAppState::new(IndexRegistry::new()));
    let mut shutdown_rx = state.shutdown_tx.subscribe();

    run_memory_pressure_tick(&state).await;

    let changed =
        tokio::time::timeout(std::time::Duration::from_millis(500), shutdown_rx.changed()).await;
    assert!(
        changed.is_ok(),
        "shutdown channel must be signalled within 500 ms when the restart tier is enabled \
         and RSS is (deterministically, via a 1 MB soft limit) over the hard limit"
    );
    assert!(
        *shutdown_rx.borrow(),
        "shutdown_tx value must be true after the restart branch fires"
    );
}

/// Sanity check: with the restart tier left at its default (OFF), the same
/// over-hard-limit condition must NOT signal `shutdown_tx` — reclaim-only is
/// the default enforcement behaviour (an unsupervised daemon must never
/// self-terminate).
#[tokio::test]
#[serial_test::serial]
async fn restart_branch_does_not_fire_when_disabled_by_default() {
    let _guard = MemGuardEnv::capture();
    memguard::set_memory_limit_mb(Some(1));
    // SAFETY: see `MemGuardEnv`'s doc comment.
    unsafe { std::env::remove_var("TRUSTY_MEMORY_RESTART_ON_LIMIT") };

    let state = Arc::new(SearchAppState::new(IndexRegistry::new()));
    let mut shutdown_rx = state.shutdown_tx.subscribe();

    run_memory_pressure_tick(&state).await;

    let changed =
        tokio::time::timeout(std::time::Duration::from_millis(200), shutdown_rx.changed()).await;
    assert!(
        changed.is_err(),
        "restart tier defaults OFF — shutdown_tx must NOT be signalled"
    );
}
