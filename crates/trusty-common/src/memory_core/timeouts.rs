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
//! `embedder_init_timeout()`, `embed_batch_timeout()`, `write_lock_timeout()`.
//! Each reads an environment variable and falls back to a sane default.
//!
//! Test: `embedder_init_timeout_default`, `embed_batch_timeout_default`,
//! `write_lock_timeout_default`, `parse_secs_env_falls_back_on_bad_value`,
//! `parse_secs_env_reads_custom_value` (unit tests at the bottom of this
//! file; run with `cargo test -p trusty-common --features memory-core`).

use std::time::Duration;

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
/// Test: `embedder_init_timeout_default`, `parse_secs_env_reads_custom_value`.
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

/// Parse a duration from `$name` (seconds), returning `default_secs` on
/// missing or malformed values.
///
/// Why: Centralising the parse keeps each public function a one-liner and
/// ensures consistent fallback semantics across all three timeouts.
/// What: Reads `std::env::var(name)`, attempts `u64` parse, returns
/// `Duration::from_secs(parsed)` or `Duration::from_secs(default_secs)`.
/// Test: `parse_secs_env_falls_back_on_bad_value`,
///       `parse_secs_env_reads_custom_value`.
fn parse_secs_env(name: &str, default_secs: u64) -> Duration {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or_else(|| Duration::from_secs(default_secs))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: Guard that the default is 180 s when the env var is absent.
    /// What: Remove the env var, call `embedder_init_timeout()`, assert 180 s.
    /// Test: itself.
    #[test]
    fn embedder_init_timeout_default() {
        // SAFETY: single-threaded test; env mutation is safe in this context.
        unsafe { std::env::remove_var("TRUSTY_EMBEDDER_INIT_TIMEOUT_SECS") };
        let t = embedder_init_timeout();
        assert_eq!(t, Duration::from_secs(DEFAULT_EMBEDDER_INIT_SECS));
    }

    /// Why: Guard that the default is 30 s when the env var is absent.
    /// What: Remove the env var, call `embed_batch_timeout()`, assert 30 s.
    /// Test: itself.
    #[test]
    fn embed_batch_timeout_default() {
        unsafe { std::env::remove_var("TRUSTY_EMBED_BATCH_TIMEOUT_SECS") };
        let t = embed_batch_timeout();
        assert_eq!(t, Duration::from_secs(DEFAULT_EMBED_BATCH_SECS));
    }

    /// Why: Guard that the default is 60 s when the env var is absent.
    /// What: Remove the env var, call `write_lock_timeout()`, assert 60 s.
    /// Test: itself.
    #[test]
    fn write_lock_timeout_default() {
        unsafe { std::env::remove_var("TRUSTY_WRITE_LOCK_TIMEOUT_SECS") };
        let t = write_lock_timeout();
        assert_eq!(t, Duration::from_secs(DEFAULT_WRITE_LOCK_SECS));
    }

    /// Why: A non-numeric env var must fall back to the default rather than
    /// panicking.
    /// What: Set the var to "notanumber", call `parse_secs_env`, assert default.
    /// Test: itself.
    #[test]
    fn parse_secs_env_falls_back_on_bad_value() {
        unsafe {
            std::env::set_var("TRUSTY_EMBEDDER_INIT_TIMEOUT_SECS", "notanumber");
        }
        let t = parse_secs_env(
            "TRUSTY_EMBEDDER_INIT_TIMEOUT_SECS",
            DEFAULT_EMBEDDER_INIT_SECS,
        );
        assert_eq!(t, Duration::from_secs(DEFAULT_EMBEDDER_INIT_SECS));
        unsafe { std::env::remove_var("TRUSTY_EMBEDDER_INIT_TIMEOUT_SECS") };
    }

    /// Why: A valid numeric env var must be respected.
    /// What: Set the var to "5", call `parse_secs_env`, assert 5 s.
    /// Test: itself.
    #[test]
    fn parse_secs_env_reads_custom_value() {
        unsafe {
            std::env::set_var("TRUSTY_EMBED_BATCH_TIMEOUT_SECS", "5");
        }
        let t = parse_secs_env("TRUSTY_EMBED_BATCH_TIMEOUT_SECS", DEFAULT_EMBED_BATCH_SECS);
        assert_eq!(t, Duration::from_secs(5));
        unsafe { std::env::remove_var("TRUSTY_EMBED_BATCH_TIMEOUT_SECS") };
    }
}
