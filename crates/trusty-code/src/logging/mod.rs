//! Tracing and logging initialisation for tcode.
//!
//! Why: All tcode binaries and the library's test harness need consistent
//! tracing initialisation that writes to stderr (never stdout — stdout is the
//! API transport channel). Centralising it here prevents duplicated setup and
//! ensures the `try_init` pattern is used everywhere so test binaries remain
//! idempotent.
//! What: `init_tracing` initialises the global tracing subscriber from the
//! `RUST_LOG` env var. `init_tracing_for_test` is a lightweight variant for
//! test binaries that silently drops duplicate init errors.
//! Test: `init_tracing_for_test_is_idempotent` calls the function twice and
//! asserts no panic.
//!
//! ## Log-level convention (#2857)
//!
//! trusty-code makes control-flow decisions that change a run's outcome —
//! caps tripping, retries, delegation refusals, fallbacks, truncations,
//! skips, gate interceptions. Every such decision point MUST log at the
//! level below when it fires; the organising principle is:
//!
//! - **`warn!`** — the decision silently changes the run's outcome (a cap
//!   trips, a retry budget is exhausted, a gate refuses/intercepts an
//!   action, a floor is exceeded). Minimum level for anything that could
//!   otherwise only be diagnosed via cross-run forensics (the #2852 class
//!   of bug this ticket exists to prevent).
//! - **`info!`** — a degradation the model or user acts on, but the run
//!   proceeds normally (a fallback lane serves results, a redundant
//!   re-run is suppressed, a budget truncation drops lowest-scored
//!   content, cadence compression fires on schedule).
//! - **`debug!`** — detail useful for tracing a run's mechanics but not
//!   itself a decision that changes the outcome (byte-level truncation of
//!   a single tool's output, already surfaced in-band to the model).
//! - **`error!`** — reserved for regression signals: a safeguard fired
//!   under conditions where it should have been structurally unreachable
//!   (e.g. threshold compaction firing while the cadence compressor is
//!   supposed to keep the transcript under budget at all times).
//!
//! All logs go to **stderr** via `tracing` (never stdout — see the module
//! doc above) and are filtered by `RUST_LOG` (`EnvFilter::from_default_env`).
//!
//! **Logs vs. events:** structured [`crate::events`] are telemetry for the
//! UI (a typed, machine-consumed fact stream); `tracing` logs here are for
//! an operator reading stderr while debugging a run. Both may describe the
//! same underlying fact, but one must never substitute for the other — an
//! event emission does not excuse a silent decision point from also logging.

use tracing_subscriber::EnvFilter;

/// Initialise the global tracing subscriber.
///
/// Why: All daemons and CLI entry points call this once at startup to ensure
/// log output is consistently routed to stderr with the `RUST_LOG` filter.
/// What: Installs a stderr-bound fmt subscriber with `EnvFilter::from_default_env()`.
/// Panics if called twice (use `init_tracing_for_test` in test binaries).
/// Test: Called in `main.rs`; correctness is verified by observing log output.
pub fn init_tracing() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();
}

/// Initialise the global tracing subscriber, silently ignoring duplicate inits.
///
/// Why: Test binaries may link multiple test modules that all call setup code.
/// `try_init` returns an error on the second call instead of panicking, so
/// test runs remain stable regardless of execution order.
/// What: Calls `try_init()`; swallows the error if already initialised.
/// Test: `init_tracing_for_test_is_idempotent`.
pub fn init_tracing_for_test() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .try_init();
}

/// Log level used when no `RUST_LOG` env var is set.
///
/// Why: Documents and centralises the default so operators know what to expect
/// without reading source.
/// What: A static string literal; the default filter used by `EnvFilter::from_default_env()`.
/// Test: Not directly tested — the value is advisory documentation.
pub const DEFAULT_LOG_LEVEL: &str = "info";

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// `init_tracing_for_test` must be idempotent across multiple calls.
    ///
    /// Why: Test binaries are often multi-crate and may call init from several
    /// setup functions; a panic on re-init would break the test suite. (#2857)
    /// This is the ONLY other site in this crate's `--lib` test binary that
    /// installs a GLOBAL tracing subscriber via `try_init`. Calling
    /// `crate::test_support::begin_capture()` first guarantees the #2857
    /// capture module's own `set_global_default` call always wins that
    /// one-shot race — this test's own `try_init()` then simply no-ops
    /// (already documented/expected behaviour), regardless of which test the
    /// harness happens to schedule first.
    /// What: Calls `init_tracing_for_test` twice; asserts no panic.
    /// Test: This test.
    #[test]
    fn init_tracing_for_test_is_idempotent() {
        crate::test_support::begin_capture();
        init_tracing_for_test();
        init_tracing_for_test(); // second call must not panic
    }

    /// The default log level constant is non-empty.
    ///
    /// Why: Guard against accidental empty string.
    /// What: Asserts `DEFAULT_LOG_LEVEL` is non-empty.
    /// Test: This test.
    #[test]
    fn default_log_level_is_non_empty() {
        assert!(!DEFAULT_LOG_LEVEL.is_empty());
    }
}
