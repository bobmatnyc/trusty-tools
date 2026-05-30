//! `tracing_subscriber::Layer` that taps ERROR-level events into the store.
//!
//! Why: wiring a layer into the subscriber means every `tracing::error!` call
//!      site is captured automatically with zero changes to existing code.
//!      The layer is strictly additive — it does not alter stderr output.
//! What: [`BugCaptureLayer`] implements `tracing_subscriber::Layer<S>` and
//!      in `on_event` checks that the event is ERROR-level, then builds a
//!      [`CapturedError`] and hands it to the [`ErrorStore`]. All work is
//!      synchronous and lock-bounded (no async, no channel, no heap alloc
//!      hot path beyond what tracing already pays). The layer NEVER emits
//!      tracing events of its own to avoid infinite recursion.
//! Test: `layer_captures_error_not_info`, `layer_captures_correct_fields`,
//!      `layer_respects_opt_out_env`.

use std::time::SystemTime;

use tracing::field::{Field, Visit};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::Context;

use crate::error_capture::fingerprint::compute_fingerprint;
use crate::error_capture::store::ErrorStore;
use crate::error_capture::types::CapturedError;

/// `tracing_subscriber::Layer` that captures ERROR-level events into an
/// [`ErrorStore`].
///
/// Why: zero-intrusion capture — every `tracing::error!` in any crate that
///      uses this subscriber contributes to the local error store without any
///      changes to call sites. The layer is opt-in via the `bug-capture`
///      feature flag and disabled at runtime when `TRUSTY_NO_BUG_CAPTURE` is
///      set.
/// What: on each event, if the level is `ERROR` and the opt-out env is not
///      set, builds a `CapturedError` and calls `ErrorStore::append`.
/// Test: see `tests` module below.
pub struct BugCaptureLayer {
    store: ErrorStore,
    /// Caller-supplied crate version string (typically `env!("CARGO_PKG_VERSION")`).
    crate_version: String,
}

impl BugCaptureLayer {
    /// Construct a new layer around an existing [`ErrorStore`].
    ///
    /// Why: the store is constructed separately so callers can also hand a
    ///      clone to the HTTP query endpoint without a second `Arc`.
    /// What: stores the `ErrorStore` handle and the caller-supplied version.
    /// Test: `layer_captures_error_not_info`.
    #[must_use]
    pub fn new(store: ErrorStore, crate_version: impl Into<String>) -> Self {
        Self {
            store,
            crate_version: crate_version.into(),
        }
    }
}

/// Field visitor for the bug-capture layer.
///
/// Why: tracing events expose their payload only through the `Visit` callback.
///      We collect the message and extra fields separately so we can use the
///      message for fingerprinting without the extra fields (which often
///      contain volatile runtime values).
/// What: `message` accumulates the canonical `message` field; `extras`
///      accumulates every other key=value pair.
/// Test: exercised indirectly via `layer_captures_correct_fields`.
struct CaptureVisitor {
    message: String,
    extras: String,
}

impl CaptureVisitor {
    fn new() -> Self {
        Self {
            message: String::new(),
            extras: String::new(),
        }
    }
}

impl Visit for CaptureVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        use std::fmt::Write as _;
        if field.name() == "message" {
            let _ = write!(self.message, "{value:?}");
            // Strip surrounding `"` that `{:?}` adds for plain string messages.
            if self.message.starts_with('"')
                && self.message.ends_with('"')
                && self.message.len() >= 2
            {
                self.message = self.message[1..self.message.len() - 1].to_string();
            }
        } else {
            let _ = write!(self.extras, " {}={value:?}", field.name());
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        use std::fmt::Write as _;
        if field.name() == "message" {
            self.message.push_str(value);
        } else {
            let _ = write!(self.extras, " {}={value}", field.name());
        }
    }
}

impl<S: tracing::Subscriber> Layer<S> for BugCaptureLayer {
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        let meta = event.metadata();

        // Only capture ERROR-level events.
        if *meta.level() != tracing::Level::ERROR {
            return;
        }

        // Honour the opt-out environment variable.  We check the env on
        // every event (not at construction) so the env can be set after the
        // layer is installed (useful in tests).
        if is_opt_out_set() {
            return;
        }

        let mut visitor = CaptureVisitor::new();
        event.record(&mut visitor);

        let crate_target = meta.target().to_string();
        let file = meta.file().map(str::to_string);
        let line = meta.line();

        let fingerprint =
            compute_fingerprint(&crate_target, &visitor.message, file.as_deref(), line);

        let timestamp_secs = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        let record = CapturedError {
            timestamp_secs,
            crate_target,
            crate_version: self.crate_version.clone(),
            message: visitor.message,
            fields: visitor.extras.trim().to_string(),
            file,
            line,
            os: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
            fingerprint,
        };

        self.store.append(record);
    }
}

/// Check whether the opt-out environment variable is set to a truthy value.
///
/// Why: operators may disable bug capture entirely (e.g. in CI or when
///      operating in highly restricted environments) without recompiling.
/// What: returns `true` when `TRUSTY_NO_BUG_CAPTURE` is set to any non-empty
///      string.
/// Test: `layer_respects_opt_out_env`.
fn is_opt_out_set() -> bool {
    matches!(
        std::env::var("TRUSTY_NO_BUG_CAPTURE").as_deref(),
        Ok(v) if !v.is_empty()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error_capture::BUG_CAPTURE_ENV_TEST_LOCK;
    use crate::error_capture::store::ErrorStore;
    use tracing_subscriber::layer::SubscriberExt as _;

    fn make_store() -> ErrorStore {
        ErrorStore::with_path(None, 50)
    }

    #[test]
    fn layer_captures_error_not_info() {
        let _guard = BUG_CAPTURE_ENV_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let store = make_store();
        let layer = BugCaptureLayer::new(store.clone(), "0.1.0");
        let subscriber = tracing_subscriber::registry().with(layer);
        tracing::subscriber::with_default(subscriber, || {
            tracing::error!("this is an error");
            tracing::info!("this is info — should NOT be captured");
            tracing::warn!("this is warn — should NOT be captured");
        });
        let records = store.recent_errors(10);
        assert_eq!(records.len(), 1, "only ERROR events should be captured");
        assert_eq!(records[0].message, "this is an error");
    }

    #[test]
    fn layer_captures_correct_fields() {
        let _guard = BUG_CAPTURE_ENV_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let store = make_store();
        let layer = BugCaptureLayer::new(store.clone(), "1.2.3");
        let subscriber = tracing_subscriber::registry().with(layer);
        tracing::subscriber::with_default(subscriber, || {
            tracing::error!(user_id = 42, "database connection failed");
        });
        let records = store.recent_errors(10);
        assert_eq!(records.len(), 1);
        let rec = &records[0];
        assert_eq!(rec.message, "database connection failed");
        assert!(
            rec.fields.contains("user_id"),
            "fields should contain user_id: {:?}",
            rec.fields
        );
        assert_eq!(rec.crate_version, "1.2.3");
        assert!(!rec.fingerprint.is_empty());
        assert_eq!(
            rec.fingerprint.len(),
            64,
            "fingerprint must be 64 hex chars"
        );
        assert!(!rec.os.is_empty(), "os must be populated");
        assert!(!rec.arch.is_empty(), "arch must be populated");
    }

    #[test]
    fn layer_captures_crate_target() {
        let _guard = BUG_CAPTURE_ENV_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let store = make_store();
        let layer = BugCaptureLayer::new(store.clone(), "0.0.1");
        let subscriber = tracing_subscriber::registry().with(layer);
        tracing::subscriber::with_default(subscriber, || {
            tracing::error!(target: "my_crate::module", "targeted error");
        });
        let records = store.recent_errors(10);
        assert_eq!(records.len(), 1);
        // The crate_target should reflect the tracing target.
        assert_eq!(records[0].crate_target, "my_crate::module");
    }

    #[test]
    fn layer_respects_opt_out_env() {
        // Hold the env lock for the entire duration of this test so no other
        // layer test can observe the opt-out variable.
        let _guard = BUG_CAPTURE_ENV_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        // SAFETY: env mutation is serialised by BUG_CAPTURE_ENV_TEST_LOCK above.
        unsafe {
            std::env::set_var("TRUSTY_NO_BUG_CAPTURE", "1");
        }
        let store = make_store();
        let layer = BugCaptureLayer::new(store.clone(), "0.1.0");
        let subscriber = tracing_subscriber::registry().with(layer);
        tracing::subscriber::with_default(subscriber, || {
            tracing::error!("this error must not be captured");
        });
        unsafe {
            std::env::remove_var("TRUSTY_NO_BUG_CAPTURE");
        }
        let records = store.recent_errors(10);
        assert!(records.is_empty(), "opt-out env must disable capture");
    }

    #[test]
    fn layer_generates_deterministic_fingerprint() {
        let _guard = BUG_CAPTURE_ENV_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        // Two ERROR events with the same logical message but different
        // volatile tokens should produce the same fingerprint.
        //
        // We verify this by checking that the fingerprint computation over
        // two messages that differ only in a digit (port number) yields the
        // same result when given the same location. The layer test exercises
        // the full capture path; deterministic fingerprint across messages is
        // separately covered by `fingerprint::tests::fingerprint_same_for_
        // logically_identical_errors`.
        let store = make_store();
        let layer = BugCaptureLayer::new(store.clone(), "0.1.0");
        let subscriber = tracing_subscriber::registry().with(layer);

        // Emit two events via a helper so both originate from the same
        // source file and line number — ensuring the location component of
        // the fingerprint is identical. The messages differ only in the port
        // digit, which normalise_message strips.
        #[inline(never)]
        fn emit_port_error(port: u16) {
            tracing::error!("failed to connect to port {port}");
        }

        tracing::subscriber::with_default(subscriber, || {
            emit_port_error(8080);
            emit_port_error(9090);
        });
        let records = store.recent_errors(10);
        assert_eq!(records.len(), 2);
        // Both events share the same crate_target (from the helper's tracing
        // target), same code location (same macro invocation in emit_port_error),
        // and the same normalised message ("failed to connect to port N" →
        // "failed to connect to port N" after digit stripping). Fingerprints
        // must be equal.
        assert_eq!(
            records[0].fingerprint, records[1].fingerprint,
            "fingerprints must match for logically identical errors; got:\n  fp1={}\n  fp2={}",
            records[0].fingerprint, records[1].fingerprint,
        );
    }
}
