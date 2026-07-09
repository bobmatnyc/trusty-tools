//! Idle-to-disk eviction ticker — periodically drops cold palace handles.
//!
//! Why: a long-lived daemon hydrates every palace's drawer table, HNSW graph,
//! and KG adjacency into RAM (~90 MB each). Even under the LRU open-handle cap
//! the resident set stays fully loaded regardless of query activity, so a host
//! with dozens of palaces sits at multiple GB of idle RSS. The durable redb
//! store is the source of truth, so a palace nobody has queried in a while can
//! have its whole handle dropped and be lazily re-opened on the next access
//! (`PalaceRegistry::open_palace`). This module runs the periodic sweep that
//! calls `PalaceRegistry::evict_idle`, mirroring `dream_scheduler`'s
//! watch-channel shutdown wiring.
//!
//! What: `spawn_idle_evict_ticker` reads `TRUSTY_MEMORY_IDLE_EVICT_SECS`
//! (default 300; `0` disables) and, when enabled, spawns a background task that
//! evicts palaces idle past that threshold every `min(threshold, 60)` seconds.
//!
//! Test: `tests::idle_evict_secs_from_env_defaults_and_parses`,
//! `tests::spawn_disabled_returns_none`,
//! `tests::ticker_evicts_idle_palace`.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;
use tracing::info;
use trusty_common::memory_core::PalaceRegistry;

/// Environment variable controlling the idle-to-disk eviction TTL, in seconds.
///
/// Why: operators need to trade idle RSS against cold-reopen latency without a
/// rebuild; a shorter TTL frees RAM sooner at the cost of more reopens.
/// What: parsed by [`idle_evict_secs_from_env`]. A value of `0` (or an
/// unset/invalid value that resolves to the default of a positive number)
/// controls whether the ticker runs — `0` disables it entirely.
/// Test: `tests::idle_evict_secs_from_env_defaults_and_parses`.
pub const IDLE_EVICT_ENV: &str = "TRUSTY_MEMORY_IDLE_EVICT_SECS";

/// Default idle-to-disk TTL when the env var is unset or invalid.
///
/// Why: 300 s (5 min) matches the dream-scheduler idle window and is long
/// enough that an interactively-used palace is never evicted mid-session, yet
/// short enough to reclaim RAM from truly dormant palaces within minutes.
/// What: a compile-time constant, overridable via [`IDLE_EVICT_ENV`].
/// Test: `tests::idle_evict_secs_from_env_defaults_and_parses`.
pub const DEFAULT_IDLE_EVICT_SECS: u64 = 300;

/// Resolve the idle-to-disk TTL (seconds) from the environment.
///
/// Why: centralises the env parse so the ticker and diagnostics agree.
/// What: reads [`IDLE_EVICT_ENV`]. An explicit `0` disables eviction and is
/// returned verbatim. Any other valid non-negative integer is used as the TTL.
/// An unset, empty, or non-numeric value falls back to
/// [`DEFAULT_IDLE_EVICT_SECS`].
/// Test: `tests::idle_evict_secs_from_env_defaults_and_parses`.
pub fn idle_evict_secs_from_env() -> u64 {
    match std::env::var(IDLE_EVICT_ENV) {
        Ok(v) => v.trim().parse::<u64>().unwrap_or(DEFAULT_IDLE_EVICT_SECS),
        Err(_) => DEFAULT_IDLE_EVICT_SECS,
    }
}

/// Spawn the idle-to-disk eviction ticker using the env-configured TTL.
///
/// Why: wires the sweep into daemon startup (see `spawn_startup_tasks`)
/// alongside `spawn_dream_scheduler`.
/// What: reads [`idle_evict_secs_from_env`]; if `0`, logs that eviction is
/// disabled and returns `None` (no task). Otherwise delegates to
/// [`spawn_idle_evict_ticker_with`] and returns its `JoinHandle`.
/// Test: `tests::spawn_disabled_returns_none`.
pub fn spawn_idle_evict_ticker(
    registry: Arc<PalaceRegistry>,
    shutdown_rx: watch::Receiver<bool>,
) -> Option<tokio::task::JoinHandle<()>> {
    let secs = idle_evict_secs_from_env();
    if secs == 0 {
        info!(
            env = IDLE_EVICT_ENV,
            "idle-to-disk eviction disabled ({IDLE_EVICT_ENV}=0)"
        );
        return None;
    }
    Some(spawn_idle_evict_ticker_with(registry, secs, shutdown_rx))
}

/// Spawn the idle-evict ticker with an explicit TTL (seconds).
///
/// Why: separated from the env wrapper so tests can drive a short, deterministic
/// threshold without mutating process env.
/// What: spawns a background task that, every `min(threshold_secs, 60)` seconds
/// (bounded so eviction latency never exceeds ~a minute), calls
/// [`PalaceRegistry::evict_idle`] with the TTL. The loop races its sleep against
/// `shutdown_rx`; a `true` value (or a closed sender) exits cleanly. A
/// `threshold_secs` of `0` is treated as disabled by `evict_idle` itself, so the
/// task simply no-ops, but callers should prefer `spawn_idle_evict_ticker` which
/// skips spawning entirely in that case.
/// Test: `tests::ticker_evicts_idle_palace`.
pub fn spawn_idle_evict_ticker_with(
    registry: Arc<PalaceRegistry>,
    threshold_secs: u64,
    mut shutdown_rx: watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()> {
    let threshold = Duration::from_secs(threshold_secs);
    let tick = Duration::from_secs(threshold_secs.clamp(1, 60));
    info!(
        threshold_secs,
        tick_secs = tick.as_secs(),
        "idle-to-disk eviction ticker started"
    );
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = tokio::time::sleep(tick) => {}
                res = shutdown_rx.changed() => {
                    if res.is_err() || *shutdown_rx.borrow() {
                        info!("idle-evict ticker shutting down");
                        return;
                    }
                }
            }
            if *shutdown_rx.borrow() {
                info!("idle-evict ticker shutting down");
                return;
            }
            let evicted = registry.evict_idle(threshold);
            if evicted > 0 {
                info!(
                    evicted,
                    "idle-evict ticker dropped {evicted} idle palace(s)"
                );
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::sync::atomic::Ordering;
    use trusty_common::memory_core::palace::{Palace, PalaceId};
    use trusty_common::memory_core::PalaceRegistry;

    /// Why: the env parse is the single source of truth for the TTL, including
    /// the `0`-disables contract and the default fallback.
    /// What: exercises unset (default), explicit `0` (disabled), and a parsed
    /// value. `#[serial]` guards the env mutation from parallel tests.
    /// Test: this test.
    #[test]
    #[serial]
    fn idle_evict_secs_from_env_defaults_and_parses() {
        // SAFETY: #[serial] serialises env mutation across tests in this crate.
        unsafe {
            std::env::remove_var(IDLE_EVICT_ENV);
        }
        assert_eq!(idle_evict_secs_from_env(), DEFAULT_IDLE_EVICT_SECS);

        unsafe {
            std::env::set_var(IDLE_EVICT_ENV, "0");
        }
        assert_eq!(idle_evict_secs_from_env(), 0, "0 must disable eviction");

        unsafe {
            std::env::set_var(IDLE_EVICT_ENV, "45");
        }
        assert_eq!(idle_evict_secs_from_env(), 45);

        unsafe {
            std::env::remove_var(IDLE_EVICT_ENV);
        }
    }

    /// Why: a `0` TTL must skip spawning the task entirely.
    /// What: sets the env to `0`, calls `spawn_idle_evict_ticker`, asserts
    /// `None` is returned (no background task).
    /// Test: this test.
    #[tokio::test]
    #[serial]
    async fn spawn_disabled_returns_none() {
        unsafe {
            std::env::set_var(IDLE_EVICT_ENV, "0");
        }
        let registry = Arc::new(PalaceRegistry::new());
        let (_tx, rx) = watch::channel(false);
        let handle = spawn_idle_evict_ticker(registry, rx);
        unsafe {
            std::env::remove_var(IDLE_EVICT_ENV);
        }
        assert!(handle.is_none(), "disabled ticker must not spawn a task");
    }

    /// Why: end-to-end proof the ticker actually reclaims an idle palace.
    /// What: registers a palace, forces its idle clock into the past, drops the
    /// creating reference so only the registry holds it, spawns the ticker with
    /// a 1 s threshold, and polls until the registry drops it (bounded ~5 s).
    /// Test: this test.
    #[tokio::test]
    async fn ticker_evicts_idle_palace() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let registry = Arc::new(PalaceRegistry::new());
        let id = PalaceId::new("idle-ticker");
        let palace = Palace {
            id: id.clone(),
            name: "Idle".to_string(),
            description: None,
            created_at: chrono::Utc::now(),
            data_dir: tmp.path().join(id.as_str()),
        };
        let handle = registry
            .create_palace(tmp.path(), palace)
            .expect("create palace");
        // Force the idle clock far into the past, then release our reference so
        // the handle's strong_count is 1 (only the registry) and it is eligible.
        handle.last_accessed.store(0, Ordering::Relaxed);
        drop(handle);
        assert_eq!(registry.len(), 1);

        let (_tx, rx) = watch::channel(false);
        let _join = spawn_idle_evict_ticker_with(registry.clone(), 1, rx);

        // Poll until the ticker drops the idle palace (bounded to avoid hangs).
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !registry.is_empty() {
            if std::time::Instant::now() >= deadline {
                panic!("idle-evict ticker did not drop the idle palace within 5s");
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(
            registry.get(&id).is_none(),
            "idle palace must be evicted by the ticker"
        );
    }
}
