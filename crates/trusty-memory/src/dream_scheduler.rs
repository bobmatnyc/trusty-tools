//! Autonomous Dreamer scheduler — spawns per-palace dream loops on daemon startup.
//!
//! Why: `Dreamer::start_with_shutdown()` was fully implemented but never called
//! in production (issue #1529, epic #1531). Without this module every dream
//! cycle required a manual `POST /api/v1/dream/run`; memory consolidation,
//! corpus pruning, and semantic consolidation never ran autonomously. Each
//! palace now gets a background dream loop that wakes every `idle_secs`
//! (default 300 s), checks the idle clock, and runs one cycle when the palace
//! has been inactive long enough.
//!
//! What: exports `spawn_dream_scheduler`, a function that iterates the palaces
//! already loaded in the `PalaceRegistry`, spawns one `Dreamer` loop per
//! palace wired to the daemon's graceful-shutdown signal, and returns
//! immediately. A global `TRUSTY_DREAM_DISABLED=1` env var disables all
//! scheduling (useful for tests and deployments that prefer explicit runs).
//! Failures in one palace's dream loop never crash other loops — they log and
//! continue.
//!
//! Test: `tests::dream_scheduler_spawns_per_palace_loop`,
//! `tests::dream_scheduler_honors_disable_flag`,
//! `tests::dream_scheduler_shutdown_stops_all_loops`.

use std::sync::Arc;

use tokio::sync::watch;
use tracing::info;
use trusty_common::memory_core::dream::{DreamConfig, Dreamer};
use trusty_common::memory_core::PalaceRegistry;

/// Environment variable that disables autonomous dream scheduling when set to
/// any non-empty value (convention: set to `"1"`).
///
/// Why: CI environments, integration tests, and deployments that prefer
/// explicit `POST /api/v1/dream/run` calls need a way to opt out without
/// recompiling. An env var at daemon startup is the lightest-weight gate.
/// What: `spawn_dream_scheduler` checks this at startup; if set, it logs and
/// returns immediately without spawning any loops.
/// Test: `dream_scheduler_honors_disable_flag`.
pub const DREAM_DISABLED_ENV: &str = "TRUSTY_DREAM_DISABLED";

/// Spawn per-palace dream loops for every palace currently open in `registry`.
///
/// Why: issue #1529 — `Dreamer::start_with_shutdown()` was never called so
/// memory consolidation only ran via manual API. This function closes that gap
/// by wiring the scheduler into the daemon startup sequence.
///
/// What:
/// 1. Checks `TRUSTY_DREAM_DISABLED`; returns immediately (no loops) when set.
/// 2. Iterates every `PalaceId` currently registered in `registry`.
/// 3. For each palace: resolves the `Arc<PalaceHandle>` via `registry.get()`,
///    constructs a fresh `Dreamer` with `DreamConfig::default()`, and spawns
///    a background loop via `Dreamer::start_with_shutdown(handle, rx.clone())`.
/// 4. Spawns a bridge task that awaits `shutdown_rx` (a `watch::Receiver<bool>`
///    driven by the daemon's SIGTERM / SIGINT signal) and broadcasts the stop
///    signal to all dream loops.
///
/// Callers must provide `shutdown_rx`, a `watch::Receiver<bool>` that flips to
/// `true` (or is closed) when the daemon should stop. See `make_shutdown_watch`
/// for the bridge that converts `trusty_common::shutdown_signal()` into this
/// channel.
///
/// Returns the number of dream loops spawned.
///
/// Test: `dream_scheduler_spawns_per_palace_loop`.
pub fn spawn_dream_scheduler(
    registry: &PalaceRegistry,
    shutdown_rx: watch::Receiver<bool>,
) -> usize {
    if std::env::var(DREAM_DISABLED_ENV).is_ok_and(|v| !v.is_empty()) {
        info!(
            env = DREAM_DISABLED_ENV,
            "autonomous dream scheduling disabled ({DREAM_DISABLED_ENV} is set)"
        );
        return 0;
    }

    let palace_ids = registry.list();
    let mut spawned: usize = 0;

    for palace_id in palace_ids {
        let config = DreamConfig::default();
        let idle_secs = config.idle_secs;
        let dreamer = Arc::new(Dreamer::new(config));
        // Unpin (idle-to-disk): the loop takes the registry + id and resolves
        // the handle each cycle via `peek` (no reopen), so it never captures an
        // `Arc<PalaceHandle>` for the process lifetime. A palace idle-evicted to
        // disk is simply skipped until the next access re-opens it — the loop
        // keeps running and picks it back up. This is what lets the idle-evict
        // sweep actually free a cold palace's heavy RAM.
        dreamer.start_with_shutdown(registry.clone(), palace_id.clone(), shutdown_rx.clone());
        spawned += 1;

        info!(
            palace = %palace_id,
            idle_secs,
            "dream_scheduler: spawned background dream loop"
        );
    }

    info!(
        loops_spawned = spawned,
        "dream_scheduler: all per-palace loops running"
    );
    spawned
}

/// Spawn all background maintenance loops (dream scheduler + idle-to-disk
/// eviction ticker) on the shared shutdown watch, then wire the shutdown bridge.
///
/// Why: keeps the startup wiring — and the `#1529` ordering guarantee (spawn the
/// loops BEFORE the bridge so an early SIGTERM cannot pre-cancel them) — in one
/// place instead of scattering it across `main::spawn_startup_tasks`. The
/// idle-evict ticker rides the SAME `watch` receiver as the dream loops so both
/// stop cleanly on SIGTERM/SIGINT.
/// What: clones `dream_shutdown_rx` for the idle-evict ticker, spawns the
/// per-palace dream loops, spawns the idle-evict ticker
/// (`TRUSTY_MEMORY_IDLE_EVICT_SECS`-gated), then spawns the shutdown bridge on
/// `dtx`. Returns the number of dream loops spawned (for the caller's log line).
/// Test: exercised end-to-end by the daemon startup path; the ticker and
/// scheduler are unit-tested in their own modules.
pub fn spawn_background_maintenance(
    registry: &Arc<PalaceRegistry>,
    dream_shutdown_rx: watch::Receiver<bool>,
    dtx: watch::Sender<bool>,
) -> usize {
    let idle_evict_rx = dream_shutdown_rx.clone();
    let loops = spawn_dream_scheduler(registry, dream_shutdown_rx);
    crate::idle_evict::spawn_idle_evict_ticker(registry.clone(), idle_evict_rx);
    // Bridge is spawned AFTER the loops exist (see #1529 ordering note).
    spawn_shutdown_bridge(dtx);
    loops
}

/// Build the `(Sender, Receiver)` pair for the dream-scheduler shutdown signal.
///
/// Why: `trusty_common::shutdown_signal()` is a one-shot `async fn` that
/// resolves when SIGTERM or SIGINT fires — it cannot be fanned out to N
/// receivers directly. A `tokio::sync::watch` channel is the standard way
/// to fan one cancel signal out to many background tasks without cloning a
/// one-shot future.
///
/// What: returns `(tx, rx)` where `tx` is held by the daemon's shutdown
/// bridge task (see `spawn_shutdown_bridge`) and every dream loop holds a
/// clone of `rx`. When the bridge task flips `tx` to `true`, all dream loops
/// see the change on their next `shutdown.changed()` / `shutdown.borrow()`
/// poll and exit cleanly.
///
/// Test: `dream_scheduler_shutdown_stops_all_loops`.
pub fn make_shutdown_watch() -> (watch::Sender<bool>, watch::Receiver<bool>) {
    watch::channel(false)
}

/// Spawn a bridge task that converts the daemon's SIGTERM/SIGINT signal into
/// a `watch::Sender<bool>` flip.
///
/// Why: `trusty_common::shutdown_signal()` is `async fn() -> ()` — it
/// resolves exactly once and cannot be shared across tasks. The watch channel
/// is the fan-out primitive. The bridge task awaits the one-shot signal and
/// then sets the watch to `true`, which wakes every subscribed dream loop.
///
/// What: spawns a `tokio::spawn` that awaits `trusty_common::shutdown_signal()`,
/// then sends `true` on `tx`. When the sender is dropped (e.g. if the task is
/// cancelled), every receiver's `changed()` future returns `Err` which
/// `start_with_shutdown` treats as a shutdown signal — so the bridge is also
/// crash-safe.
///
/// Test: `dream_scheduler_shutdown_stops_all_loops` sends `true` directly on
/// the channel (skipping the real signal) to avoid signal delivery in tests.
pub fn spawn_shutdown_bridge(tx: watch::Sender<bool>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        trusty_common::shutdown_signal().await;
        // Signal fired — broadcast stop to all dream loops.
        let _ = tx.send(true);
    })
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::sync::Arc;
    use std::time::Duration;
    use trusty_common::memory_core::dream::DreamConfig;
    use trusty_common::memory_core::dream::Dreamer;
    use trusty_common::memory_core::palace::{Palace, PalaceId};
    use trusty_common::memory_core::PalaceRegistry;

    /// Helper: build a minimal `Palace` + open its `PalaceHandle` into a temp
    /// dir, register it in the given registry, and return the `PalaceId`.
    ///
    /// Why: each test that exercises per-palace scheduling needs at least one
    /// registered palace handle.
    /// What: creates the palace under a `tempfile::tempdir()`, opens the handle
    /// via `PalaceRegistry::create_palace`, and returns the id.
    /// Test: used by the other tests in this module.
    fn register_temp_palace(registry: &PalaceRegistry, tmp: &std::path::Path) -> PalaceId {
        let palace = Palace {
            id: PalaceId::new(format!("test-palace-{}", uuid::Uuid::new_v4())),
            name: "Test Palace".to_string(),
            description: None,
            created_at: chrono::Utc::now(),
            data_dir: tmp.to_path_buf(),
        };
        let id = palace.id.clone();
        registry
            .create_palace(tmp, palace)
            .expect("create test palace");
        id
    }

    /// Why: spawn_dream_scheduler must return 0 when TRUSTY_DREAM_DISABLED is
    /// set, with no loops started.
    /// What: sets the env var, calls spawn_dream_scheduler, asserts the count.
    ///       Uses `current_thread` flavor + `#[serial]` to eliminate the data
    ///       race that the default multi-threaded runtime would introduce when
    ///       multiple env-mutating tests run concurrently.
    /// Test: itself.
    #[tokio::test(flavor = "current_thread")]
    #[serial]
    async fn dream_scheduler_honors_disable_flag() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let registry = PalaceRegistry::new();
        register_temp_palace(&registry, tmp.path());

        // SAFETY: single-threaded tokio runtime + #[serial] ensures no other
        // thread reads DREAM_DISABLED_ENV concurrently. Restore on every path.
        unsafe {
            std::env::set_var(DREAM_DISABLED_ENV, "1");
        }
        let (_tx, rx) = make_shutdown_watch();
        let spawned = spawn_dream_scheduler(&registry, rx);
        unsafe {
            std::env::remove_var(DREAM_DISABLED_ENV);
        }

        assert_eq!(
            spawned, 0,
            "expect 0 loops when TRUSTY_DREAM_DISABLED=1; got {spawned}"
        );
    }

    /// Why: spawn_dream_scheduler must spawn exactly one loop per palace in the
    /// registry and return the correct count.
    /// What: registers two palaces, calls spawn_dream_scheduler, asserts count=2.
    ///       Uses `current_thread` flavor + `#[serial]` to eliminate the data
    ///       race that the default multi-threaded runtime would introduce when
    ///       multiple env-mutating tests run concurrently.
    /// Test: itself.
    #[tokio::test(flavor = "current_thread")]
    #[serial]
    async fn dream_scheduler_spawns_per_palace_loop() {
        // SAFETY: single-threaded tokio runtime + #[serial] ensures no other
        // thread reads DREAM_DISABLED_ENV concurrently. Restore on every path.
        unsafe {
            std::env::remove_var(DREAM_DISABLED_ENV);
        }

        let tmp = tempfile::tempdir().expect("tempdir");
        let tmp2 = tempfile::tempdir().expect("tempdir2");
        let registry = PalaceRegistry::new();
        register_temp_palace(&registry, tmp.path());
        register_temp_palace(&registry, tmp2.path());

        let (tx, rx) = make_shutdown_watch();
        let spawned = spawn_dream_scheduler(&registry, rx);

        // Stop the loops.
        let _ = tx.send(true);

        assert_eq!(
            spawned, 2,
            "expect 1 loop per palace (2 total); got {spawned}"
        );
    }

    /// Why: dream loops spawned with start_with_shutdown must exit when the
    /// watch channel flips to true. This guards the graceful-shutdown contract.
    /// What: spawns one loop with a short idle_secs, sends shutdown=true, and
    /// awaits the join handle to confirm it exits within a reasonable deadline.
    /// Test: itself.
    #[tokio::test]
    async fn dream_scheduler_shutdown_stops_loop() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let registry = PalaceRegistry::new();
        let id = register_temp_palace(&registry, tmp.path());

        let config = DreamConfig {
            idle_secs: 1,
            ..DreamConfig::default()
        };
        let dreamer = Arc::new(Dreamer::new(config));
        let (tx, rx) = make_shutdown_watch();

        let join = dreamer.start_with_shutdown(registry, id, rx);

        // Signal shutdown.
        tx.send(true).expect("send shutdown");

        // The loop should exit well within 2 s after receiving the signal.
        let result = tokio::time::timeout(Duration::from_secs(2), join).await;
        assert!(
            result.is_ok(),
            "dream loop did not exit within 2s after shutdown signal"
        );
    }

    /// Why: an error in one palace's dream cycle must not abort the loop for
    /// that palace or crash any other palace's loop. The loop logs and continues.
    /// What: relies on the Dreamer implementation's `warn!` path on `dream_cycle`
    /// error — we can only verify indirectly here that the scheduler spawns
    /// without panic even when a palace is in a degraded state. We test the
    /// per-loop error isolation at the `Dreamer` level in trusty-common.
    /// Test: itself (smoke test — would panic or deadlock on regression).
    #[tokio::test]
    async fn dream_scheduler_no_panic_with_empty_registry() {
        unsafe {
            std::env::remove_var(DREAM_DISABLED_ENV);
        }
        let registry = PalaceRegistry::new();
        let (_tx, rx) = make_shutdown_watch();
        let count = spawn_dream_scheduler(&registry, rx);
        assert_eq!(count, 0, "empty registry should produce 0 loops");
    }

    /// Why: verify `make_shutdown_watch` returns a functioning watch pair.
    /// What: sends `true`, asserts receiver sees it.
    /// Test: itself.
    #[tokio::test]
    async fn make_shutdown_watch_pair_works() {
        let (tx, mut rx) = make_shutdown_watch();
        assert!(!*rx.borrow(), "initial value should be false");
        tx.send(true).expect("send");
        rx.changed().await.expect("changed");
        assert!(*rx.borrow(), "value should flip to true after send");
    }

    /// Why: verify that dream_cycle actually executes and returns stats when
    /// called on a real PalaceHandle — this guards against regressions where
    /// the cycle silently no-ops or panics.
    /// What: creates a fresh palace, constructs a Dreamer with idle_secs=0
    /// (always considered idle), calls dream_cycle directly, and asserts the
    /// returned DreamStats fields are accessible (the call returned Ok, not a
    /// panic or error). Also verifies that PersistedDreamStats was written to
    /// the palace's data_dir (i.e. `<data_root>/<palace_id>/`).
    /// Test: itself.
    #[tokio::test]
    async fn dream_scheduler_loops_actually_run_cycles() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let registry = PalaceRegistry::new();
        let id = register_temp_palace(&registry, tmp.path());
        let handle = registry.get(&id).expect("handle after create");

        let config = DreamConfig {
            idle_secs: 0, // always idle — cycle fires immediately
            ..DreamConfig::default()
        };
        let dreamer = Arc::new(Dreamer::new(config));

        // Verify the dreamer is immediately idle with idle_secs=0.
        assert!(dreamer.is_idle(), "idle_secs=0 should mean always idle");

        // Call dream_cycle directly and assert it actually ran.
        let stats = dreamer
            .dream_cycle(&handle)
            .await
            .expect("dream_cycle should not error on a fresh empty palace");

        // A real cycle returns Ok — the important assertion is that it didn't
        // panic or error. duration_ms may be 0 on very fast machines.
        let _ = stats.merged;
        let _ = stats.pruned;
        let _ = stats.duration_ms;

        // PersistedDreamStats is written to handle.data_dir, which is
        // `<data_root>/<palace_id>/` (not `<data_root>/` directly).
        let palace_data_dir = tmp.path().join(id.as_str());
        let persisted =
            trusty_common::memory_core::dream::PersistedDreamStats::load(&palace_data_dir)
                .expect("load persisted stats")
                .expect("persisted stats should exist after a cycle");
        assert_eq!(
            persisted.stats.merged, stats.merged,
            "persisted merged should match returned stats"
        );
    }
}
