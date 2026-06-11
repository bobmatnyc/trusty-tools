//! Global tracing subscriber initialisation helpers.
//!
//! Why: Every trusty-* binary wants the same verbosity ladder and the same
//! `RUST_LOG` override semantics, and every daemon needs the same log-buffer
//! + stderr composition. Defining this once removes the boilerplate.

use crate::log_buffer;

/// Initialise the global tracing subscriber.
///
/// Why: Every trusty-* binary wants the same verbosity ladder and the same
/// `RUST_LOG` override semantics. Defining it once removes the boilerplate
/// from every `main.rs`.
/// What: `verbose_count` maps `0 → warn`, `1 → info`, `2 → debug`, `3+ →
/// trace`. If `RUST_LOG` is set in the environment it wins. Logs go to
/// stderr so stdout stays clean for MCP JSON-RPC.
/// Test: side-effecting (global subscriber) — covered by integration with
/// `cargo run -- -v status` in downstream crates.
pub fn init_tracing(verbose_count: u8) {
    let default_filter = match verbose_count {
        0 => "warn",
        1 => "info",
        2 => "debug",
        _ => "trace",
    };
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(default_filter));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_target(false)
        .try_init();
}

/// Initialise the global tracing subscriber and capture events into a
/// [`log_buffer::LogBuffer`] so the daemon can serve recent logs over HTTP.
///
/// Why: daemons expose `GET /logs/tail`, which needs an in-memory ring of
/// recent log lines. Routing capture through the subscriber means every
/// existing `tracing::info!` / `warn!` call site is mirrored automatically —
/// no second logging API to keep in sync. The stderr `fmt` layer is retained
/// so operators still see live logs in the terminal / launchd log file.
/// What: builds a `tracing_subscriber::registry` with two layers — the
/// standard stderr `fmt` layer (same verbosity ladder + `RUST_LOG` override
/// as [`init_tracing`]) and a [`log_buffer::LogBufferLayer`] feeding the
/// returned [`log_buffer::LogBuffer`]. Uses `try_init`, so a process that has
/// already installed a subscriber keeps it; the returned buffer is still
/// valid (just empty) in that case.
/// Test: `cargo test -p trusty-common log_buffer` covers the layer; the
/// daemon `/logs/tail` integration tests cover the wired path end-to-end.
#[must_use]
pub fn init_tracing_with_buffer(verbose_count: u8, capacity: usize) -> log_buffer::LogBuffer {
    use tracing_subscriber::Layer as _;
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    let default_filter = match verbose_count {
        0 => "warn",
        1 => "info",
        2 => "debug",
        _ => "trace",
    };
    let stderr_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(default_filter));

    // The log-buffer layer must capture activity even when the stderr filter
    // is set to `warn` (the default for `trusty-search start` without `-v`).
    // `RUST_LOG_BUFFER` lets ops widen or narrow the buffer independently of
    // stderr; the default of `info` matches the activity feed's intent.
    let buffer_filter = tracing_subscriber::EnvFilter::try_from_env("RUST_LOG_BUFFER")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));

    let buffer = log_buffer::LogBuffer::new(capacity);
    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stderr)
        .with_target(false)
        .with_filter(stderr_filter);
    let buf_layer = log_buffer::LogBufferLayer::new(buffer.clone()).with_filter(buffer_filter);
    let _ = tracing_subscriber::registry()
        .with(fmt_layer)
        .with(buf_layer)
        .try_init();
    buffer
}

/// Initialise the global tracing subscriber with a [`log_buffer::LogBuffer`]
/// **and** a [`crate::error_capture::BugCaptureLayer`] composed in one `try_init` call.
///
/// Why: `tracing_subscriber::registry().try_init()` can only succeed once per
///      process. Callers that need both the HTTP log-tail buffer (issue #35)
///      and Phase 1 bug capture must compose all three layers in a single call;
///      two separate `try_init` calls would leave the second one silently ignored.
///      This helper is the canonical entry-point for daemon binaries that want
///      both features wired together at startup.
/// What: builds an `EnvFilter`-gated stderr `fmt` layer, an info-level
///      `LogBufferLayer`, and a `BugCaptureLayer` for `app_name`/`crate_version`;
///      installs them together via `try_init`. Returns `(LogBuffer, ErrorStore)`
///      so the caller can stash both handles in the daemon's `AppState`.
///      All capture is to a JSONL file under `<dirs::data_dir()>/<app_name>/`
///      and an in-memory ring — nothing is written to stdout, so this is
///      MCP-safe. Honours `TRUSTY_NO_BUG_CAPTURE` for opt-out.
/// Test: `cargo test -p trusty-common --features bug-capture -- init_tracing_with_capture`.
#[cfg(feature = "bug-capture")]
#[must_use]
pub fn init_tracing_with_buffer_and_capture(
    verbose_count: u8,
    capacity: usize,
    app_name: &str,
    crate_version: impl Into<String>,
) -> (log_buffer::LogBuffer, crate::error_capture::ErrorStore) {
    use tracing_subscriber::Layer as _;
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    let default_filter = match verbose_count {
        0 => "warn",
        1 => "info",
        2 => "debug",
        _ => "trace",
    };
    let stderr_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(default_filter));
    let buffer_filter = tracing_subscriber::EnvFilter::try_from_env("RUST_LOG_BUFFER")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));

    let buffer = log_buffer::LogBuffer::new(capacity);
    let (capture_layer, store) = crate::error_capture::bug_capture_layer(
        app_name,
        crate::error_capture::DEFAULT_CAPTURE_CAPACITY,
        crate_version,
    );

    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stderr)
        .with_target(false)
        .with_filter(stderr_filter);
    let buf_layer = log_buffer::LogBufferLayer::new(buffer.clone()).with_filter(buffer_filter);
    let _ = tracing_subscriber::registry()
        .with(fmt_layer)
        .with(buf_layer)
        .with(capture_layer)
        .try_init();
    (buffer, store)
}

/// Disable coloured terminal output when requested or when stdout is not a TTY.
///
/// Why: Pipe-friendly output is mandatory for scripting (`trusty-search list
/// | jq …`). `NO_COLOR` / `TERM=dumb` are the canonical signals; passing
/// `--no-color` should override too.
/// What: calls `colored::control::set_override(false)` when the caller asks
/// for it or when the standard heuristics indicate no colour.
/// Test: side-effecting global; trivially covered by manual `NO_COLOR=1 cargo
/// run -- list`.
pub fn maybe_disable_color(no_color: bool) {
    let env_says_no =
        std::env::var("NO_COLOR").is_ok() || std::env::var("TERM").as_deref() == Ok("dumb");
    if no_color || env_says_no {
        colored::control::set_override(false);
    }
}
