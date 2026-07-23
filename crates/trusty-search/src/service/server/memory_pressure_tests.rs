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
use crate::core::corpus::CorpusStore;
use crate::core::indexer::CodeIndexer;
use crate::core::memguard;
use crate::core::registry::{IndexHandle, IndexId, IndexRegistry};
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use tokio::sync::RwLock as TokioRwLock;

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

/// Two-tick regression (issue #3683 slice 2, round-2 critic review HIGH):
/// an `EarlyStop` sweep must NOT let the hysteresis baseline wedge the next
/// tick, even when RSS is completely FLAT (the worst case for the strict
/// `rss_mb > last_reclaim_rss_mb` gate) — because the estimate that
/// justified stopping early ([`ESTIMATED_BYTES_FREED_PER_RECLAIMED_ENTRY`],
/// documented as uncalibrated) may have overestimated real freed bytes,
/// leaving genuinely reclaimable, untouched candidates behind.
///
/// Tick 1 (simulated): a sweep completes as `EarlyStop` at post-trim RSS
/// `after` — still over the high-water mark, by construction (an early stop
/// only happens while genuine memory pressure persists). Tick 2 (simulated):
/// RSS sampled identical to `after` — no rise at all.
///
/// Contrast case pinned in the same test: a genuinely `Exhausted` sweep DOES
/// trust `after` as the new baseline — flat RSS on the next tick correctly
/// skips, exactly as it always has (issue #2846's original hysteresis
/// design, unaffected by this fix).
#[test]
fn hysteresis_survives_early_stop_sweep_even_when_rss_is_flat() {
    let after = 9_500u64; // tick 1's post-trim RSS; still over an assumed high-water mark

    // EarlyStop: the baseline must reset to the "no sweep yet" sentinel...
    let baseline_after_early_stop =
        hysteresis_baseline_after_sweep(SweepCompletion::EarlyStop, after);
    assert_eq!(
        baseline_after_early_stop, 0,
        "an EarlyStop sweep must not trust its own RSS sample as a hysteresis baseline"
    );
    // ...so tick 2 re-sweeps even though RSS (`after`) did not rise at all
    // relative to itself — the exact scenario a naive `store(after)` would
    // have wedged forever (RSS plateaus above the ceiling, sweep never
    // re-fires, the 2.2x-limit gap this ticket exists to close).
    assert!(
        should_reclaim_now(after, baseline_after_early_stop),
        "tick 2 must re-sweep on flat RSS after an EarlyStop tick 1 — untouched candidates \
         (and a possibly-wrong estimate) deserve another attempt regardless of whether RSS rose"
    );

    // Exhausted: the baseline DOES trust `after` — flat RSS on the next tick
    // correctly skips, unchanged from pre-#3683-slice-2 behaviour.
    let baseline_after_exhausted =
        hysteresis_baseline_after_sweep(SweepCompletion::Exhausted, after);
    assert_eq!(baseline_after_exhausted, after);
    assert!(
        !should_reclaim_now(after, baseline_after_exhausted),
        "an Exhausted sweep's baseline must still gate flat RSS exactly as before this fix"
    );
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

// ---------------------------------------------------------------------------
// `run_pressure_sweep` — budgeted, oldest-idle-first, recency-exempt sweep
// (issue #3683 slice 2, critic review HIGH)
// ---------------------------------------------------------------------------

/// Build a bare, corpus-backed (BM25-only, no embedder/HNSW store) handle for
/// `id` — mirrors `residency_sweep_tests::bare_handle` /
/// `idle_eviction_tests::bare_corpus_handle`.
fn bare_corpus_handle(id: &str, redb_path: &std::path::Path) -> IndexHandle {
    let index_id = IndexId::new(id.to_string());
    let root = PathBuf::from(format!("/tmp/pressure-sweep-test-{id}"));
    let mut idx = CodeIndexer::new(id, &root);
    let store = CorpusStore::open(redb_path).expect("open corpus store");
    idx.set_corpus_store(Arc::new(store));
    let indexer = Arc::new(TokioRwLock::new(idx));
    IndexHandle::bare(index_id, indexer, root)
}

/// RAII guard saving/restoring `TRUSTY_MEMORY_PRESSURE_EXEMPT_IDLE_SECS` —
/// this env var's only reader/writer in the crate's test suite besides the
/// other two tests below (all three are `#[serial_test::serial]`, matching
/// this file's existing convention for shared process env state).
struct ExemptSecsEnvGuard(Option<String>);

impl ExemptSecsEnvGuard {
    fn set(v: &str) -> Self {
        let prior = std::env::var("TRUSTY_MEMORY_PRESSURE_EXEMPT_IDLE_SECS").ok();
        // SAFETY: see struct doc comment.
        unsafe { std::env::set_var("TRUSTY_MEMORY_PRESSURE_EXEMPT_IDLE_SECS", v) };
        Self(prior)
    }
}

impl Drop for ExemptSecsEnvGuard {
    fn drop(&mut self) {
        // SAFETY: see struct doc comment.
        unsafe {
            match &self.0 {
                Some(v) => std::env::set_var("TRUSTY_MEMORY_PRESSURE_EXEMPT_IDLE_SECS", v),
                None => std::env::remove_var("TRUSTY_MEMORY_PRESSURE_EXEMPT_IDLE_SECS"),
            }
        }
    }
}

/// Index `n` tiny files into `idx` (BM25-only ingest — no embedder/store
/// needed), producing `n` chunks + `n` BM25 documents (`2n` reclaimable
/// entries via `reclaim_memory_now`).
async fn index_n_files(idx: &CodeIndexer, prefix: &str, n: usize) {
    let files: Vec<(String, String)> = (0..n)
        .map(|i| {
            (
                format!("src/{prefix}_{i}.rs"),
                format!("fn f_{prefix}_{i}() {{}}"),
            )
        })
        .collect();
    idx.index_files_batch(&files).await.expect("index batch");
}

/// Core acceptance test: with the recency exemption disabled (isolating the
/// stop-early budget from the exemption mechanism), three equally-sized
/// indexes go idle at three DIFFERENT wall-clock times. A `target_freed_mb`
/// reachable by reclaiming just the single OLDEST-idle index must stop the
/// sweep there — the two less-idle indexes must survive untouched. This is
/// the direct fix for the critic's "no stop-after-target" finding: the sweep
/// must not clear every registered index once it has already freed enough.
#[tokio::test]
#[serial_test::serial]
async fn pressure_sweep_stops_early_once_target_reached_sparing_least_idle() {
    let _exempt_guard = ExemptSecsEnvGuard::set("0"); // disable exemption for this test

    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();
    let dir_c = tempfile::tempdir().unwrap();
    let state = SearchAppState::new(IndexRegistry::new());

    // 300 chunks + 300 BM25 docs = 600 entries/index ⇒
    // estimate_freed_mb(600) == 1 (600 * 2048 / 1_048_576 == 1), so a
    // target_freed_mb of 1 is satisfied by reclaiming ANY single index.
    let a = bare_corpus_handle("a", &dir_a.path().join("index.redb"));
    index_n_files(&*a.indexer.read().await, "a", 300).await;
    state.registry.register(a);
    tokio::time::sleep(Duration::from_millis(60)).await;

    let b = bare_corpus_handle("b", &dir_b.path().join("index.redb"));
    index_n_files(&*b.indexer.read().await, "b", 300).await;
    state.registry.register(b);
    tokio::time::sleep(Duration::from_millis(60)).await;

    let c = bare_corpus_handle("c", &dir_c.path().join("index.redb"));
    index_n_files(&*c.indexer.read().await, "c", 300).await;
    state.registry.register(c);

    // At this point: a is oldest-idle (~120ms), b is mid (~60ms), c is
    // freshest (~0ms) — oldest_idle_first must visit a, then b, then c.
    let (reclaimed, cleared, completion) = run_pressure_sweep(&Arc::new(state.clone()), 1).await;

    assert_eq!(cleared, 1, "must stop after clearing exactly one index");
    assert_eq!(
        reclaimed, 600,
        "the one cleared index contributes 600 entries"
    );
    assert_eq!(
        completion,
        SweepCompletion::EarlyStop,
        "b and c were spared with candidates left unvisited — must report EarlyStop, \
         not Exhausted (issue #3683 slice 2 round-2 critic review HIGH)"
    );

    let a_count = state
        .registry
        .get(&IndexId::new("a".to_string()))
        .unwrap()
        .indexer
        .read()
        .await
        .in_memory_chunk_count()
        .await;
    let b_count = state
        .registry
        .get(&IndexId::new("b".to_string()))
        .unwrap()
        .indexer
        .read()
        .await
        .in_memory_chunk_count()
        .await;
    let c_count = state
        .registry
        .get(&IndexId::new("c".to_string()))
        .unwrap()
        .indexer
        .read()
        .await
        .in_memory_chunk_count()
        .await;

    assert_eq!(
        a_count, 0,
        "the OLDEST-idle index ('a') must be the one cleared"
    );
    assert_eq!(
        b_count, 300,
        "'b' (mid-idle) must be spared once the target is met"
    );
    assert_eq!(
        c_count, 300,
        "'c' (least-idle) must be spared once the target is met"
    );
}

/// Mild pressure: a target reachable entirely from non-exempt (cold) indexes
/// must never touch a recently-queried (hot) index — the critic's "hot index
/// unconditionally cleared" finding.
#[tokio::test]
#[serial_test::serial]
async fn pressure_sweep_exempts_hot_indexes_under_mild_pressure() {
    let _exempt_guard = ExemptSecsEnvGuard::set("1"); // 1s exemption floor

    let dir_cold = tempfile::tempdir().unwrap();
    let dir_hot = tempfile::tempdir().unwrap();
    let state = SearchAppState::new(IndexRegistry::new());

    let cold = bare_corpus_handle("cold", &dir_cold.path().join("index.redb"));
    index_n_files(&*cold.indexer.read().await, "cold", 300).await;
    state.registry.register(cold);

    // Push "cold" past the 1s exemption floor before "hot" is even indexed.
    tokio::time::sleep(Duration::from_millis(1_100)).await;

    let hot = bare_corpus_handle("hot", &dir_hot.path().join("index.redb"));
    index_n_files(&*hot.indexer.read().await, "hot", 300).await;
    state.registry.register(hot); // "hot" was just touched — idle ~0s

    // Target reachable from "cold" alone (see the stop-early test's math).
    let (reclaimed, cleared, completion) = run_pressure_sweep(&Arc::new(state.clone()), 1).await;

    assert_eq!(
        cleared, 1,
        "only the non-exempt (cold) index should be cleared"
    );
    assert_eq!(reclaimed, 600);
    assert_eq!(
        completion,
        SweepCompletion::EarlyStop,
        "the exempt (hot) index was spared, unvisited — must report EarlyStop"
    );

    let cold_count = state
        .registry
        .get(&IndexId::new("cold".to_string()))
        .unwrap()
        .indexer
        .read()
        .await
        .in_memory_chunk_count()
        .await;
    let hot_count = state
        .registry
        .get(&IndexId::new("hot".to_string()))
        .unwrap()
        .indexer
        .read()
        .await
        .in_memory_chunk_count()
        .await;

    assert_eq!(cold_count, 0, "the non-exempt cold index must be cleared");
    assert_eq!(
        hot_count, 300,
        "a recently-queried (hot) index must survive mild pressure"
    );
}

/// Extreme pressure ("desperation"): a target UNREACHABLE from non-exempt
/// indexes alone must fall through to clearing exempt (hot) indexes too —
/// avoiding an OOM kill outweighs a hot index's warm cache.
#[tokio::test]
#[serial_test::serial]
async fn pressure_sweep_desperation_pass_clears_hot_indexes_under_extreme_pressure() {
    let _exempt_guard = ExemptSecsEnvGuard::set("1"); // 1s exemption floor

    let dir_cold = tempfile::tempdir().unwrap();
    let dir_hot = tempfile::tempdir().unwrap();
    let state = SearchAppState::new(IndexRegistry::new());

    let cold = bare_corpus_handle("cold", &dir_cold.path().join("index.redb"));
    index_n_files(&*cold.indexer.read().await, "cold", 300).await;
    state.registry.register(cold);

    tokio::time::sleep(Duration::from_millis(1_100)).await;

    let hot = bare_corpus_handle("hot", &dir_hot.path().join("index.redb"));
    index_n_files(&*hot.indexer.read().await, "hot", 300).await;
    state.registry.register(hot);

    // Target requires BOTH indexes (600 entries each ⇒ 1MB each ⇒ need 2MB).
    let (reclaimed, cleared, completion) = run_pressure_sweep(&Arc::new(state.clone()), 2).await;

    assert_eq!(
        cleared, 2,
        "desperation pass must clear the hot index too once cold alone can't reach the target"
    );
    assert_eq!(reclaimed, 1_200);
    assert_eq!(
        completion,
        SweepCompletion::Exhausted,
        "both cold and hot were fully visited — nothing left untouched, so this must report \
         Exhausted, trusting the post-trim RSS as the next hysteresis baseline"
    );

    let cold_count = state
        .registry
        .get(&IndexId::new("cold".to_string()))
        .unwrap()
        .indexer
        .read()
        .await
        .in_memory_chunk_count()
        .await;
    let hot_count = state
        .registry
        .get(&IndexId::new("hot".to_string()))
        .unwrap()
        .indexer
        .read()
        .await
        .in_memory_chunk_count()
        .await;

    assert_eq!(cold_count, 0);
    assert_eq!(
        hot_count, 0,
        "extreme pressure must clear even a recently-queried (hot) index"
    );
}

// ---------------------------------------------------------------------------
// `run_memory_pressure_tick` — EarlyStop hysteresis wiring (issue #3683
// slice 2, round-3 critic review MEDIUM)
// ---------------------------------------------------------------------------

/// RAII guard saving/restoring `TRUSTY_MEMORY_HIGH_WATER_PCT` — this test is
/// the only mutator of this env var in the crate's test suite (mirrors
/// [`ExemptSecsEnvGuard`] above).
struct HighWaterPctEnvGuard(Option<String>);

impl HighWaterPctEnvGuard {
    fn set(v: &str) -> Self {
        let prior = std::env::var("TRUSTY_MEMORY_HIGH_WATER_PCT").ok();
        // SAFETY: see struct doc comment.
        unsafe { std::env::set_var("TRUSTY_MEMORY_HIGH_WATER_PCT", v) };
        Self(prior)
    }
}

impl Drop for HighWaterPctEnvGuard {
    fn drop(&mut self) {
        // SAFETY: see struct doc comment.
        unsafe {
            match &self.0 {
                Some(v) => std::env::set_var("TRUSTY_MEMORY_HIGH_WATER_PCT", v),
                None => std::env::remove_var("TRUSTY_MEMORY_HIGH_WATER_PCT"),
            }
        }
    }
}

/// Round-3 critic review MEDIUM: `hysteresis_survives_early_stop_sweep_even_when_rss_is_flat`
/// (above) pins the PURE mapping (`hysteresis_baseline_after_sweep`) in
/// isolation, but nothing drove `run_memory_pressure_tick` ITSELF through an
/// `EarlyStop` path and asserted `state.last_reclaim_rss_mb` afterward. A
/// wiring regression reverting `run_memory_pressure_tick` back to
/// unconditionally `state.last_reclaim_rss_mb.store(after, ..)` (discarding
/// the `SweepCompletion` match entirely) would still pass every OTHER
/// tick-level test in this file, because those use an empty `IndexRegistry`
/// — trivially `Exhausted` (nothing to sweep), never exercising the
/// `EarlyStop` branch's wiring at all. This test closes that gap
/// end-to-end, through the real tick function.
///
/// Setup mirrors `pressure_sweep_exempts_hot_indexes_under_mild_pressure`'s
/// cold+hot shape, but drives [`run_memory_pressure_tick`] directly instead
/// of calling [`run_pressure_sweep`]: `TRUSTY_MEMORY_HIGH_WATER_PCT=100` plus
/// a soft limit derived from the test process's OWN RSS, SAMPLED RIGHT
/// BEFORE the tick call (minimizing drift between this sample and the tick's
/// own internal sample — all index setup/sleeping happens BEFORE sampling,
/// not between sampling and the call), make `target_freed_mb` a small,
/// (comfortably-bounded) positive number — satisfiable by the cold index
/// alone (1 200 files ⇒ 2 400 entries ⇒ `estimate_freed_mb` == 4, well over
/// the 2 MB target with margin for any residual jitter), so the hot index
/// (300 files ⇒ 600 entries, far too small to be needed) is spared,
/// unvisited, and the sweep must report `EarlyStop`.
#[tokio::test]
#[serial_test::serial]
async fn run_memory_pressure_tick_resets_hysteresis_baseline_on_early_stop() {
    let _mem_guard = MemGuardEnv::capture();
    let _pct_guard = HighWaterPctEnvGuard::set("100");
    let _exempt_guard = ExemptSecsEnvGuard::set("1");
    // SAFETY: mirrors `MemGuardEnv`'s own convention — keep the last-resort
    // restart tier OFF so it never fires here (irrelevant to this test).
    unsafe { std::env::remove_var("TRUSTY_MEMORY_RESTART_ON_LIMIT") };

    let state = Arc::new(SearchAppState::new(IndexRegistry::new()));

    let dir_cold = tempfile::tempdir().unwrap();
    let dir_hot = tempfile::tempdir().unwrap();

    // 1 200 files ⇒ 2 400 entries ⇒ estimate_freed_mb(2400) == 4 (2400 * 2048
    // == 4_915_200 bytes ⇒ floor 4 MB) — comfortably covers a target of 2 MB
    // even with a few MB of RSS jitter either way.
    let cold = bare_corpus_handle("cold", &dir_cold.path().join("index.redb"));
    index_n_files(&*cold.indexer.read().await, "cold", 1_200).await;
    state.registry.register(cold);

    // Push "cold" past the 1s exemption floor before "hot" is even indexed.
    tokio::time::sleep(Duration::from_millis(1_100)).await;

    let hot = bare_corpus_handle("hot", &dir_hot.path().join("index.redb"));
    index_n_files(&*hot.indexer.read().await, "hot", 300).await;
    state.registry.register(hot); // "hot" was just touched — idle ~0s

    // Sample RSS and derive the soft limit HERE — immediately before the
    // tick call, after all setup/sleeping above, so the gap between this
    // sample and the tick's own internal `current_rss_mb()` call is just a
    // handful of instructions (registry snapshot + Vec allocations), not an
    // entire index-setup-plus-1.1s-sleep window.
    let rss = memguard::current_rss_mb().expect("sample this test process's own RSS");
    let limit = rss.saturating_sub(2).max(1); // target_freed_mb == 2 at pct=100
    memguard::set_memory_limit_mb(Some(limit));

    run_memory_pressure_tick(&state).await;

    let cold_count = state
        .registry
        .get(&IndexId::new("cold".to_string()))
        .unwrap()
        .indexer
        .read()
        .await
        .in_memory_chunk_count()
        .await;
    let hot_count = state
        .registry
        .get(&IndexId::new("hot".to_string()))
        .unwrap()
        .indexer
        .read()
        .await
        .in_memory_chunk_count()
        .await;
    assert_eq!(
        cold_count, 0,
        "sanity: the cold index must have been cleared"
    );
    assert_eq!(
        hot_count, 300,
        "sanity: the hot index must have been spared (EarlyStop, not desperation)"
    );

    assert_eq!(
        state.last_reclaim_rss_mb.load(Ordering::Relaxed),
        0,
        "an EarlyStop sweep must reset the hysteresis baseline to the 0 sentinel, NOT store \
         the post-trim RSS — a wiring regression reverting to the unconditional store would \
         wedge the next tick's re-sweep on flat RSS (issue #3683 slice 2, round-2/3 critic review)"
    );
}
