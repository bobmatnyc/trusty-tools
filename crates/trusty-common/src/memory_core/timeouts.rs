//! Bounded-timeout configuration for memory operations (issue #906).
//!
//! Why: The remember/recall path has several `await` points that can hang
//! indefinitely — most importantly the CoreML cold-compile inside
//! `FastEmbedder::new()` (30-120 s on Apple Silicon) and the per-call
//! `embed_batch` invocation. Without explicit bounds a single stuck embedder
//! blocks every concurrent memory operation in the process forever. This
//! module centralises the three timeout thresholds and their env-var overrides
//! so callers get a single import and defaults can be tuned per deployment.
//!
//! What: Exports three `std::time::Duration` functions:
//! `embedder_init_timeout()`, `embed_batch_timeout()`, `write_lock_timeout()`,
//! plus a `lock_with_timeout` async helper that applies the write-lock timeout
//! at every call site uniformly.
//!
//! Test: `embedder_init_timeout_default`, `embed_batch_timeout_default`,
//! `write_lock_timeout_default`, `parse_secs_with_falls_back_on_bad_value`,
//! `parse_secs_with_reads_custom_value`, `parse_secs_with_uses_default_when_absent`
//! (unit tests at the bottom of this file; run with
//! `cargo test -p trusty-common --features memory-core`).

use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, MutexGuard};

/// Default ceiling for `FastEmbedder::new()` cold init.
///
/// Why: CoreML graph compilation on Apple Silicon can take 30-120 s on first
/// run; 180 s gives ample headroom without risking an indefinite hang.
const DEFAULT_EMBEDDER_INIT_SECS: u64 = 180;

/// Default ceiling for a single `embed_batch` call.
///
/// Why: Normal batches take 10-30 ms; even worst-case single-item batches
/// should complete well under 10 s. 30 s gives a 100x safety margin.
const DEFAULT_EMBED_BATCH_SECS: u64 = 30;

/// Default ceiling for per-palace write-mutex acquisition.
///
/// Why: The mutex is held only during the embed+upsert+persist pipeline
/// (< 1 s normally). A long queue of writers could push acquisition time
/// above 1 s; 60 s is conservative without risking an indefinite cascade.
const DEFAULT_WRITE_LOCK_SECS: u64 = 60;

/// Return the `FastEmbedder::new()` init timeout.
///
/// Why: Overridable via `TRUSTY_EMBEDDER_INIT_TIMEOUT_SECS` so operators on
/// slow CI machines or cold CUDA hosts can extend the ceiling without
/// recompiling.
/// What: Reads the env var; falls back to `DEFAULT_EMBEDDER_INIT_SECS` (180).
/// Test: `embedder_init_timeout_default`, `parse_secs_with_reads_custom_value`.
pub fn embedder_init_timeout() -> Duration {
    parse_secs_env(
        "TRUSTY_EMBEDDER_INIT_TIMEOUT_SECS",
        DEFAULT_EMBEDDER_INIT_SECS,
    )
}

/// Return the per-call `embed_batch` timeout.
///
/// Why: Overridable via `TRUSTY_EMBED_BATCH_TIMEOUT_SECS` so high-throughput
/// or GPU-backed deployments can tune the ceiling.
/// What: Reads the env var; falls back to `DEFAULT_EMBED_BATCH_SECS` (30).
/// Test: `embed_batch_timeout_default`.
pub fn embed_batch_timeout() -> Duration {
    parse_secs_env("TRUSTY_EMBED_BATCH_TIMEOUT_SECS", DEFAULT_EMBED_BATCH_SECS)
}

/// Return the per-palace write-lock acquisition timeout.
///
/// Why: Overridable via `TRUSTY_WRITE_LOCK_TIMEOUT_SECS` to accommodate
/// unusually deep write queues on write-heavy deployments.
/// What: Reads the env var; falls back to `DEFAULT_WRITE_LOCK_SECS` (60).
/// Test: `write_lock_timeout_default`.
pub fn write_lock_timeout() -> Duration {
    parse_secs_env("TRUSTY_WRITE_LOCK_TIMEOUT_SECS", DEFAULT_WRITE_LOCK_SECS)
}

/// Acquire a `tokio::sync::Mutex` with a bounded timeout, returning an error
/// on expiry.
///
/// Why: The write-lock acquisition pattern (get timeout, call
/// `tokio::time::timeout`, map the elapsed error to a formatted message) was
/// duplicated at four call sites (retrieval remember + forget paths and
/// tools.rs memory_remember + memory_note handlers). A single helper
/// eliminates the duplication and guarantees a consistent error message shape
/// (issue #906).
/// What: Calls `tokio::time::timeout(duration, mutex.lock())`. On success
/// returns the `MutexGuard`. On expiry returns `anyhow::Error` with a message
/// that includes the palace label and the configured duration.
/// Test: `write_lock_timeout_returns_error_when_held` in
/// `memory_core::retrieval::timeout_tests` exercises this path end-to-end.
pub async fn lock_with_timeout<'a>(
    mutex: &'a Arc<Mutex<()>>,
    duration: Duration,
    label: &str,
) -> anyhow::Result<MutexGuard<'a, ()>> {
    tokio::time::timeout(duration, mutex.lock())
        .await
        .map_err(|_| {
            anyhow::anyhow!(
                "palace '{}' write-lock acquisition timed out after {:?} \
                 (issue #906); a previous writer may be stuck — retry or \
                 increase TRUSTY_WRITE_LOCK_TIMEOUT_SECS",
                label,
                duration
            )
        })
}

/// Pure parser: return a `Duration` from a lookup-provided optional string.
///
/// Why: Separating the env-lookup side-effect from the parse logic makes the
/// core behaviour testable without any `unsafe` env mutation. The public
/// `parse_secs_env` and `embedder_init_timeout` / `embed_batch_timeout` /
/// `write_lock_timeout` functions delegate here and are themselves trivially
/// correct once this function is verified.
/// What: Calls `lookup(key)` to get an optional `String`. If present, tries
/// `u64` parse; on success returns `Duration::from_secs(parsed)`. Falls back
/// to `Duration::from_secs(default_secs)` when the key is absent or the value
/// is non-numeric.
/// Test: `parse_secs_with_falls_back_on_bad_value`,
///       `parse_secs_with_reads_custom_value`,
///       `parse_secs_with_uses_default_when_absent`.
pub fn parse_secs_with(
    lookup: impl Fn(&str) -> Option<String>,
    key: &str,
    default_secs: u64,
) -> Duration {
    lookup(key)
        .and_then(|v| v.parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or_else(|| Duration::from_secs(default_secs))
}

/// Parse a duration from `$name` (seconds), returning `default_secs` on
/// missing or malformed values.
///
/// Why: Centralising the parse keeps each public function a one-liner and
/// ensures consistent fallback semantics across all three timeouts. Delegates
/// to `parse_secs_with` with the real env lookup so the pure logic is tested
/// separately (no env mutation in pure-logic tests).
/// What: Calls `parse_secs_with` with `std::env::var` as the lookup.
/// Test: Public-function default tests verify the end-to-end path; pure-logic
///       tests cover `parse_secs_with` directly without env mutations.
fn parse_secs_env(name: &str, default_secs: u64) -> Duration {
    parse_secs_with(|k| std::env::var(k).ok(), name, default_secs)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -------------------------------------------------------------------------
    // Pure-logic tests — exercise `parse_secs_with` with an injected lookup.
    // These tests never touch process env: no `unsafe`, no races under the
    // multi-threaded test harness.
    // -------------------------------------------------------------------------

    /// Why: Guard that `parse_secs_with` returns the default when the lookup
    /// returns `None` (key absent).
    /// What: Pass a lookup that always returns `None`, assert default returned.
    /// Test: itself.
    #[test]
    fn parse_secs_with_uses_default_when_absent() {
        let t = parse_secs_with(|_| None, "ANY_KEY", DEFAULT_EMBEDDER_INIT_SECS);
        assert_eq!(t, Duration::from_secs(DEFAULT_EMBEDDER_INIT_SECS));
    }

    /// Why: Guard that a non-numeric value falls back to the default rather
    /// than panicking.
    /// What: Pass a lookup returning `Some("notanumber")`, assert default.
    /// Test: itself.
    #[test]
    fn parse_secs_with_falls_back_on_bad_value() {
        let t = parse_secs_with(
            |_| Some("notanumber".to_string()),
            "ANY_KEY",
            DEFAULT_EMBEDDER_INIT_SECS,
        );
        assert_eq!(t, Duration::from_secs(DEFAULT_EMBEDDER_INIT_SECS));
    }

    /// Why: Guard that a valid numeric value is respected.
    /// What: Pass a lookup returning `Some("5")`, assert 5 s returned.
    /// Test: itself.
    #[test]
    fn parse_secs_with_reads_custom_value() {
        let t = parse_secs_with(
            |_| Some("5".to_string()),
            "ANY_KEY",
            DEFAULT_EMBED_BATCH_SECS,
        );
        assert_eq!(t, Duration::from_secs(5));
    }

    // -------------------------------------------------------------------------
    // Default-value tests — exercise the public timeout functions to ensure
    // the defaults are what the module documentation promises. These tests
    // rely on the env vars being absent at test time; they are serialised
    // behind a process-wide mutex to avoid interleaving with any other test
    // that sets those vars (e.g. the `#[ignore]` timeout-fire tests).
    // -------------------------------------------------------------------------

    /// Serialises tests that read the real env so they cannot interleave.
    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static ENV_MUTEX: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        ENV_MUTEX
            .get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .expect("env_lock mutex poisoned")
    }

    /// Why: Guard that the default is 180 s when the env var is absent.
    /// What: Hold the env mutex, verify the var is unset (or unset it), call
    /// `embedder_init_timeout()`, assert 180 s.
    /// Test: itself.
    #[test]
    fn embedder_init_timeout_default() {
        let _guard = env_lock();
        // Clear the var so we're testing the absent-key code path.
        // SAFETY: we hold `env_lock()` which serialises all env-touching
        // tests in this module; no other thread mutates this var while
        // the mutex is held.
        unsafe { std::env::remove_var("TRUSTY_EMBEDDER_INIT_TIMEOUT_SECS") };
        let t = embedder_init_timeout();
        assert_eq!(t, Duration::from_secs(DEFAULT_EMBEDDER_INIT_SECS));
    }

    /// Why: Guard that the default is 30 s when the env var is absent.
    /// What: Hold the env mutex, unset the var, call `embed_batch_timeout()`,
    /// assert 30 s.
    /// Test: itself.
    #[test]
    fn embed_batch_timeout_default() {
        let _guard = env_lock();
        unsafe { std::env::remove_var("TRUSTY_EMBED_BATCH_TIMEOUT_SECS") };
        let t = embed_batch_timeout();
        assert_eq!(t, Duration::from_secs(DEFAULT_EMBED_BATCH_SECS));
    }

    /// Why: Guard that the default is 60 s when the env var is absent.
    /// What: Hold the env mutex, unset the var, call `write_lock_timeout()`,
    /// assert 60 s.
    /// Test: itself.
    #[test]
    fn write_lock_timeout_default() {
        let _guard = env_lock();
        unsafe { std::env::remove_var("TRUSTY_WRITE_LOCK_TIMEOUT_SECS") };
        let t = write_lock_timeout();
        assert_eq!(t, Duration::from_secs(DEFAULT_WRITE_LOCK_SECS));
    }
}
