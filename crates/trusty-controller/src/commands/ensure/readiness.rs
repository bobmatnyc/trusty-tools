//! `tctl ensure --wait` readiness polling (#1341).
//!
//! Why: with `--wait`, `ensure` must block until the project's search index is
//! registered + the daemons report ready, returning exit 0 only when ready and
//! the distinct exit 4 on timeout. Polling against a bounded deadline turns
//! "wait forever for a daemon that will never come up" into a fast, scriptable
//! failure that CI can branch on.
//!
//! What: [`PollConfig`] (poll interval + overall timeout, env-overridable),
//! [`poll_until_ready`] (the generic deadline-bounded loop over an injected async
//! readiness probe — pure of HTTP so it is unit-testable), and
//! [`probe_ready`] (the real probe: both trusty-search and trusty-memory
//! `/health` return 2xx).
//!
//! Test: `tests` drive `poll_until_ready` with a probe that flips ready after N
//! attempts (success) and one that never readies (timeout), asserting the
//! returned readiness and that the loop respects the deadline.

use std::future::Future;
use std::time::{Duration, Instant};

use super::daemon::{health_ok, resolve_base_url, MEMORY_APP, SEARCH_APP};

/// Env var overriding the `--wait` overall timeout, in seconds.
///
/// Why: CI environments differ — a cold daemon on a slow box may need longer,
/// while a fast unit harness wants a short timeout. An env override avoids a new
/// CLI flag while keeping the default sensible.
/// What: `"TCTL_ENSURE_WAIT_TIMEOUT_SECS"`; parsed as `u64` seconds when set.
/// Test: `tests::poll_config_reads_env_timeout`.
pub const WAIT_TIMEOUT_ENV: &str = "TCTL_ENSURE_WAIT_TIMEOUT_SECS";

/// Env var overriding the `--wait` poll interval, in milliseconds.
///
/// Why: lets the tests poll fast (so they finish quickly) and operators slow the
/// cadence on constrained hosts, without a CLI flag.
/// What: `"TCTL_ENSURE_WAIT_INTERVAL_MS"`; parsed as `u64` milliseconds when set.
/// Test: `tests::poll_config_reads_env_interval`.
pub const WAIT_INTERVAL_ENV: &str = "TCTL_ENSURE_WAIT_INTERVAL_MS";

/// Default `--wait` overall timeout.
///
/// Why: long enough for a cold daemon to finish warm-boot + first index, short
/// enough that a stuck daemon fails a CI step in bounded time.
/// What: 120 seconds.
/// Test: `tests::poll_config_default`.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(120);

/// Default `--wait` poll interval.
///
/// Why: frequent enough to return promptly once ready, infrequent enough to
/// avoid hammering `/health`.
/// What: 1 second.
/// Test: `tests::poll_config_default`.
const DEFAULT_INTERVAL: Duration = Duration::from_millis(1000);

/// Poll cadence + deadline for the readiness wait.
///
/// Why: bundling the two knobs keeps `poll_until_ready`'s signature small and
/// puts the env-override resolution in one tested place.
/// What: `interval` between probes and the overall `timeout`.
/// Test: `tests::poll_config_default`, `tests::poll_config_reads_env_*`.
#[derive(Clone, Copy, Debug)]
pub struct PollConfig {
    /// Delay between readiness probes.
    pub interval: Duration,
    /// Overall deadline; the loop gives up (timeout) after this elapses.
    pub timeout: Duration,
}

impl PollConfig {
    /// Resolve the poll config from the environment, falling back to defaults.
    ///
    /// Why: the single place env overrides are read so the loop body stays pure
    /// of `std::env`.
    /// What: reads [`WAIT_TIMEOUT_ENV`] / [`WAIT_INTERVAL_ENV`]; a missing or
    /// unparseable value falls back to [`DEFAULT_TIMEOUT`] / [`DEFAULT_INTERVAL`].
    /// Test: `tests::poll_config_default`, `tests::poll_config_reads_env_*`.
    pub fn from_env() -> Self {
        let timeout = std::env::var(WAIT_TIMEOUT_ENV)
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .map(Duration::from_secs)
            .unwrap_or(DEFAULT_TIMEOUT);
        let interval = std::env::var(WAIT_INTERVAL_ENV)
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .map(Duration::from_millis)
            .unwrap_or(DEFAULT_INTERVAL);
        Self { interval, timeout }
    }
}

/// Poll an injected async readiness `probe` until it returns `true` or the
/// `cfg.timeout` deadline elapses.
///
/// Why: separating the deadline/cadence control flow from the actual HTTP probe
/// makes the loop unit-testable with a deterministic in-memory probe — the
/// hard-to-test bit (network) is injected. A ready stack returns on the first
/// probe without waiting an interval, while an already-expired deadline returns
/// `false` without spending a probe at all.
/// What: each iteration first checks the deadline (already past → `false`), then
/// calls `probe()` and returns `true` the first time it is ready; when the next
/// sleep would exceed the deadline it stops and returns `false` (timeout). Uses
/// `tokio::time::sleep` between attempts.
/// Test: `tests::poll_ready_returns_true_once_ready`,
/// `tests::poll_times_out_when_never_ready`,
/// `tests::poll_returns_immediately_when_ready`.
pub async fn poll_until_ready<F, Fut>(cfg: PollConfig, mut probe: F) -> bool
where
    F: FnMut() -> Fut,
    Fut: Future<Output = bool>,
{
    let deadline = Instant::now() + cfg.timeout;
    loop {
        // Guard the deadline before probing so an already-expired deadline
        // returns immediately without spending another probe — this caps the
        // worst-case wall-clock at the timeout rather than `timeout + one
        // probe duration`.
        if Instant::now() >= deadline {
            return false;
        }
        if probe().await {
            return true;
        }
        // Stop if sleeping another interval would pass the deadline.
        if Instant::now() + cfg.interval >= deadline {
            return false;
        }
        tokio::time::sleep(cfg.interval).await;
    }
}

/// The real readiness probe: both daemons' `/health` return 2xx.
///
/// Why: "ready" for a fully-provisioned project means the search daemon (serving
/// the just-registered index) and the memory daemon (serving the palace) are
/// both live. A daemon with no recorded address (never started) is not ready.
/// What: resolves each daemon's base URL; returns `true` only when BOTH resolve
/// AND both `GET /health` return 2xx. Any resolution error or down daemon → not
/// ready (so the loop keeps waiting until the deadline).
/// Test: side-effecting (network); the loop control flow is unit-tested via
/// `poll_until_ready` with an injected probe.
pub async fn probe_ready(client: &reqwest::Client) -> bool {
    let search = match resolve_base_url(SEARCH_APP) {
        Ok(Some(b)) => b,
        _ => return false,
    };
    let memory = match resolve_base_url(MEMORY_APP) {
        Ok(Some(b)) => b,
        _ => return false,
    };
    health_ok(client, &search).await && health_ok(client, &memory).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::ensure::ENV_TEST_LOCK;
    use std::cell::Cell;

    /// Why: defaults must hold when no env overrides are set.
    /// What: a config built with no env vars present yields the default knobs.
    /// Test: This is the test.
    #[test]
    fn poll_config_default() {
        let _g = ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // Ensure the env is clean for this assertion.
        unsafe {
            // SAFETY: serialised by ENV_TEST_LOCK; no concurrent env access in this crate's tests.
            std::env::remove_var(WAIT_TIMEOUT_ENV);
            std::env::remove_var(WAIT_INTERVAL_ENV);
        }
        let cfg = PollConfig::from_env();
        assert_eq!(cfg.timeout, DEFAULT_TIMEOUT);
        assert_eq!(cfg.interval, DEFAULT_INTERVAL);
    }

    /// Why: the env timeout override must be honoured (CI tuning).
    /// What: sets the timeout env and asserts it is parsed into seconds.
    /// Test: This is the test.
    #[test]
    fn poll_config_reads_env_timeout() {
        let _g = ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            // SAFETY: serialised by ENV_TEST_LOCK; no concurrent env access in this crate's tests.
            std::env::set_var(WAIT_TIMEOUT_ENV, "7");
        }
        let cfg = PollConfig::from_env();
        unsafe {
            // SAFETY: serialised by ENV_TEST_LOCK; no concurrent env access in this crate's tests.
            std::env::remove_var(WAIT_TIMEOUT_ENV);
        }
        assert_eq!(cfg.timeout, Duration::from_secs(7));
    }

    /// Why: the env interval override must be honoured (fast test cadence).
    /// What: sets the interval env and asserts it is parsed into millis.
    /// Test: This is the test.
    #[test]
    fn poll_config_reads_env_interval() {
        let _g = ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            // SAFETY: serialised by ENV_TEST_LOCK; no concurrent env access in this crate's tests.
            std::env::set_var(WAIT_INTERVAL_ENV, "25");
        }
        let cfg = PollConfig::from_env();
        unsafe {
            // SAFETY: serialised by ENV_TEST_LOCK; no concurrent env access in this crate's tests.
            std::env::remove_var(WAIT_INTERVAL_ENV);
        }
        assert_eq!(cfg.interval, Duration::from_millis(25));
    }

    /// Why: the loop must return `true` once the probe readies, even after a few
    /// not-ready attempts (the common warm-up case).
    /// What: a probe that flips ready on the 3rd call; assert `true`.
    /// Test: This is the test.
    #[tokio::test]
    async fn poll_ready_returns_true_once_ready() {
        let cfg = PollConfig {
            interval: Duration::from_millis(1),
            timeout: Duration::from_secs(5),
        };
        let calls = Cell::new(0u32);
        let ready = poll_until_ready(cfg, || {
            let n = calls.get() + 1;
            calls.set(n);
            async move { n >= 3 }
        })
        .await;
        assert!(ready);
        assert!(calls.get() >= 3);
    }

    /// Why: an already-ready stack must return immediately without sleeping a
    /// full interval (responsiveness).
    /// What: a probe that is ready on the first call with a long interval; the
    /// call returns `true` essentially instantly.
    /// Test: This is the test.
    #[tokio::test]
    async fn poll_returns_immediately_when_ready() {
        let cfg = PollConfig {
            interval: Duration::from_secs(3600),
            timeout: Duration::from_secs(3600),
        };
        let start = Instant::now();
        let ready = poll_until_ready(cfg, || async { true }).await;
        assert!(ready);
        assert!(start.elapsed() < Duration::from_secs(1));
    }

    /// Why: an already-expired deadline (a zero timeout) must return `false`
    /// immediately without spending a probe, so the worst-case wall-clock is
    /// bounded by the timeout rather than `timeout + one probe duration`.
    /// What: a zero-timeout config with a probe that panics if called; assert
    /// `false` and that the probe was never invoked.
    /// Test: This is the test.
    #[tokio::test]
    async fn poll_returns_false_without_probing_when_already_expired() {
        let cfg = PollConfig {
            interval: Duration::from_millis(5),
            timeout: Duration::from_millis(0),
        };
        let calls = Cell::new(0u32);
        let ready = poll_until_ready(cfg, || {
            calls.set(calls.get() + 1);
            async { true }
        })
        .await;
        assert!(!ready);
        assert_eq!(
            calls.get(),
            0,
            "probe must not run past an expired deadline"
        );
    }

    /// Why: a stack that never readies must time out and return `false` (→ the
    /// caller maps this to exit 4) within roughly the configured timeout.
    /// What: a probe that is always not-ready with a short timeout; assert
    /// `false` and that it returned promptly after the deadline.
    /// Test: This is the test.
    #[tokio::test]
    async fn poll_times_out_when_never_ready() {
        let cfg = PollConfig {
            interval: Duration::from_millis(5),
            timeout: Duration::from_millis(30),
        };
        let start = Instant::now();
        let ready = poll_until_ready(cfg, || async { false }).await;
        assert!(!ready);
        // It should give up near the deadline, not run unbounded.
        assert!(start.elapsed() < Duration::from_secs(2));
    }
}
