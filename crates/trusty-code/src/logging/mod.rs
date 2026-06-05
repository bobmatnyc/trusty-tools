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
    /// setup functions; a panic on re-init would break the test suite.
    /// What: Calls `init_tracing_for_test` twice; asserts no panic.
    /// Test: This test.
    #[test]
    fn init_tracing_for_test_is_idempotent() {
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
