//! Shared test helper for cadence environment variable mutation tests.
//!
//! Serializes tests that mutate the process-wide cadence env vars, mirroring
//! `crate::mode::MODE_ENV_LOCK`'s identical rationale. All cadence-related
//! tests (across cadence_tests.rs and other test modules) must use
//! `with_cadence_env` to prevent env-var races under cargo's parallel scheduler.

/// Serializes tests that mutate the process-wide cadence env vars, mirroring
/// `crate::mode::MODE_ENV_LOCK`'s identical rationale.
pub(crate) static CADENCE_ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Wraps a test function to safely mutate cadence env vars with serialization.
///
/// Holds `CADENCE_ENV_LOCK` for the duration of the closure, preventing
/// concurrent env-var races. Sets the specified env vars before the closure
/// and clears them after, ensuring hermetic test isolation.
///
/// # Arguments
/// - `turns`: If `Some`, sets `TCODE_CADENCE_TURNS` to this value; if `None`, removes it.
/// - `pct`: If `Some`, sets `TCODE_CADENCE_MAX_OVERHEAD_FRACTION_PCT` to this value; if `None`, removes it.
/// - `f`: The closure to execute with the env vars set.
pub(crate) async fn with_cadence_env<T>(
    turns: Option<&str>,
    pct: Option<&str>,
    f: impl FnOnce() -> T,
) -> T {
    use super::cadence::{CADENCE_OVERHEAD_FRACTION_ENV_VAR, CADENCE_TURNS_ENV_VAR};

    let _guard = CADENCE_ENV_LOCK.lock().await;
    // SAFETY: test-only env mutation, serialized by `CADENCE_ENV_LOCK`.
    unsafe {
        match turns {
            Some(v) => std::env::set_var(CADENCE_TURNS_ENV_VAR, v),
            None => std::env::remove_var(CADENCE_TURNS_ENV_VAR),
        }
        match pct {
            Some(v) => std::env::set_var(CADENCE_OVERHEAD_FRACTION_ENV_VAR, v),
            None => std::env::remove_var(CADENCE_OVERHEAD_FRACTION_ENV_VAR),
        }
    }
    let result = f();
    unsafe {
        std::env::remove_var(CADENCE_TURNS_ENV_VAR);
        std::env::remove_var(CADENCE_OVERHEAD_FRACTION_ENV_VAR);
    }
    result
}
