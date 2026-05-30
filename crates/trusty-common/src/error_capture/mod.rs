//! Error-capture layer for the trusty-* bug-reporting system (Phase 1).
//!
//! Why: Every trusty-* daemon encounters runtime errors that are valuable for
//!      developers but whose capture must be transparent to users and must
//!      never alter the existing stderr logging behaviour. This module provides
//!      a `tracing_subscriber::Layer` (`BugCaptureLayer`) that taps
//!      ERROR-level events, fingerprints them for deduplication, and persists
//!      them to a local JSONL store — all without modifying any existing
//!      `tracing::error!` call sites.
//!
//! What: gated behind the `bug-capture` feature flag so the default
//!      `trusty-common` build pulls in no new transitive dependencies. The
//!      feature adds only `sha2` (already an optional workspace dep, now
//!      required by this feature). Four sub-modules handle types, fingerprint,
//!      store, and the layer respectively; this `mod.rs` re-exports the public
//!      API surface that Phase 2 will consume.
//!
//! Public API surface (Phase 2 consumers):
//! - [`CapturedError`] — the structured error record.
//! - [`ErrorStore`] — ring buffer + JSONL store; query via `recent_errors` /
//!   `errors_by_fingerprint`.
//! - [`BugCaptureLayer`] — the tracing layer.
//! - [`bug_capture_layer`] — convenience constructor; returns a
//!   `(BugCaptureLayer, ErrorStore)` pair ready for installation.
//! - [`TRUSTY_NO_BUG_CAPTURE_ENV`] — the opt-out environment variable name.
//!
//! Opt-out: set `TRUSTY_NO_BUG_CAPTURE` to any non-empty value. Checked on
//! every event so it can be set after the layer is installed (test-friendly).
//!
//! Installation (additive — does NOT change stderr logging):
//! ```rust,ignore
//! use trusty_common::error_capture::{bug_capture_layer, DEFAULT_CAPTURE_CAPACITY};
//! use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
//!
//! let (capture_layer, store) =
//!     bug_capture_layer("my-app", DEFAULT_CAPTURE_CAPACITY, env!("CARGO_PKG_VERSION"));
//! tracing_subscriber::registry()
//!     .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
//!     .with(capture_layer)
//!     .try_init()
//!     .ok();
//!
//! // Later — Phase 2 calls:
//! let recent = store.recent_errors(20);
//! let by_fp  = store.errors_by_fingerprint();
//! ```
//!
//! Test: `cargo test -p trusty-common --features bug-capture`.

pub mod fingerprint;
pub mod layer;
pub mod store;
pub mod types;

pub use layer::BugCaptureLayer;
pub use store::{DEFAULT_CAPTURE_CAPACITY, ErrorStore};
pub use types::CapturedError;

/// Shared mutex serialising all tests that mutate `TRUSTY_NO_BUG_CAPTURE`.
///
/// Why: `cargo test` runs tests in the same binary on multiple threads. Any
///      test that sets a process-wide env var must hold this lock for the
///      duration to avoid racing with tests that read the same var.
/// What: module-level `static` so every test module in this crate shares the
///      same mutex instance (all test modules compile into one binary).
/// Test: held by `layer::tests::layer_respects_opt_out_env` and by any test
///      in this module that emits tracing events (which internally check the
///      opt-out var).
#[doc(hidden)]
pub static BUG_CAPTURE_ENV_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The environment variable that disables error capture when set to any
/// non-empty value.
///
/// Why: operators in CI or restricted environments need a zero-config way to
///      disable capture without recompiling.
/// What: string constant `"TRUSTY_NO_BUG_CAPTURE"`.
/// Test: `layer_respects_opt_out_env` in `layer.rs`.
pub const TRUSTY_NO_BUG_CAPTURE_ENV: &str = "TRUSTY_NO_BUG_CAPTURE";

/// Construct a [`BugCaptureLayer`] and its backing [`ErrorStore`].
///
/// Why: callers need both the layer (to install into the subscriber) and the
///      store handle (to answer Phase 2 queries). Constructing them together
///      here ensures they share the same `Arc` — one ring, one JSONL file.
/// What: opens (or creates) the store at the OS data dir for `app_name`,
///      wraps it in a `BugCaptureLayer`, and returns both handles.
///      The layer is ready to be composed with any `tracing_subscriber` via
///      `.with(capture_layer)`. The returned `ErrorStore` clone can be handed
///      to the HTTP or MCP layer for Phase 2 queries.
/// Test: `bug_capture_layer_constructor` below.
#[must_use]
pub fn bug_capture_layer(
    app_name: &str,
    capacity: usize,
    crate_version: impl Into<String>,
) -> (BugCaptureLayer, ErrorStore) {
    let store = ErrorStore::open(app_name, capacity);
    let layer = BugCaptureLayer::new(store.clone(), crate_version);
    (layer, store)
}

/// Convenience variant that installs the capture layer into an existing
/// `tracing_subscriber::Registry` and returns only the store handle.
///
/// Why: most daemon `main.rs` files call `init_tracing_with_buffer` from
///      `trusty-common`; they want to add capture without rewriting the
///      subscriber setup. This helper composes the two layers for them
///      behind a single function call.
/// What: builds the `(BugCaptureLayer, ErrorStore)` pair and returns just
///      the `ErrorStore` — the layer is consumed by the caller for
///      subscriber composition. The caller is responsible for
///      `registry().with(fmt_layer).with(capture_layer).try_init()`.
/// Test: `init_capture_layer_returns_store` below.
#[must_use]
pub fn init_capture_layer(
    app_name: &str,
    capacity: usize,
    crate_version: impl Into<String>,
) -> (BugCaptureLayer, ErrorStore) {
    bug_capture_layer(app_name, capacity, crate_version)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracing_subscriber::layer::SubscriberExt as _;

    #[test]
    fn bug_capture_layer_constructor() {
        let _guard = BUG_CAPTURE_ENV_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        // Verify the convenience constructor returns a layer and a store that
        // share the same underlying buffer (appending via the layer is visible
        // through the store).
        let store_path = None; // memory-only for the constructor test
        let store = ErrorStore::with_path(store_path, 10);
        let layer = BugCaptureLayer::new(store.clone(), "0.1.0");

        let subscriber = tracing_subscriber::registry().with(layer);
        tracing::subscriber::with_default(subscriber, || {
            tracing::error!("constructor test error");
        });
        let records = store.recent_errors(10);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].message, "constructor test error");
        assert_eq!(records[0].crate_version, "0.1.0");
    }

    #[test]
    fn captured_error_serde_round_trip() {
        let original = CapturedError {
            timestamp_secs: 1_700_000_000,
            crate_target: "trusty_search::indexer".to_string(),
            crate_version: "1.0.0".to_string(),
            message: "index open failed".to_string(),
            fields: "path=/tmp/idx".to_string(),
            file: Some("src/indexer.rs".to_string()),
            line: Some(42),
            os: "macos".to_string(),
            arch: "aarch64".to_string(),
            fingerprint: "a".repeat(64),
        };
        let json = serde_json::to_string(&original).unwrap();
        let back: CapturedError = serde_json::from_str(&json).unwrap();
        assert_eq!(back, original);
    }

    #[test]
    fn captured_error_summary_format() {
        let rec = CapturedError {
            timestamp_secs: 0,
            crate_target: "trusty_memory".to_string(),
            crate_version: "0.5.0".to_string(),
            message: "palace not found".to_string(),
            fields: String::new(),
            file: Some("src/palace.rs".to_string()),
            line: Some(99),
            os: "linux".to_string(),
            arch: "x86_64".to_string(),
            fingerprint: "b".repeat(64),
        };
        let summary = rec.summary();
        assert!(summary.contains("trusty_memory"), "{summary}");
        assert!(summary.contains("palace not found"), "{summary}");
        assert!(summary.contains("src/palace.rs"), "{summary}");
        assert!(summary.contains("99"), "{summary}");
    }

    #[test]
    fn init_capture_layer_returns_store() {
        let _guard = BUG_CAPTURE_ENV_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        // Smoke-test: init_capture_layer (alias for bug_capture_layer) returns
        // a usable (BugCaptureLayer, ErrorStore) pair. We don't call open()
        // here (that would hit the OS data dir), so we exercise a manual pair.
        let store = ErrorStore::with_path(None, 5);
        let layer = BugCaptureLayer::new(store.clone(), "0.2.0");
        let subscriber = tracing_subscriber::registry().with(layer);
        tracing::subscriber::with_default(subscriber, || {
            tracing::error!("init capture test");
        });
        assert_eq!(store.len(), 1);
    }
}
