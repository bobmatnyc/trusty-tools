//! Shared graceful-shutdown signal helper for trusty-* daemons.
//!
//! Why: trusty-search, trusty-memory, and trusty-analyze all need to wait for
//! SIGTERM (launchd `bootout`, `kill <pid>`) or SIGINT (Ctrl-C in dev) before
//! cleanly draining in-flight HTTP requests. Centralising the implementation
//! removes three-way duplication and ensures every daemon responds identically
//! to the same signals.
//!
//! What: exposes a single async `shutdown_signal()` function that returns once
//! EITHER SIGTERM (unix) OR SIGINT/Ctrl-C (all platforms) fires. On non-unix
//! platforms only Ctrl-C is watched.
//!
//! Test: `cargo test -p trusty-common -- shutdown` runs the compilation smoke
//! test. Signal delivery itself cannot be triggered inside a unit test without
//! `raise(SIGTERM)`, which is unsafe; the integration tests in trusty-search
//! exercise the full axum `with_graceful_shutdown` path.

/// Await SIGTERM (unix) or SIGINT/Ctrl-C (all platforms), whichever fires first.
///
/// Why: axum's `with_graceful_shutdown` takes an `async fn()` — it polls the
/// future and stops accepting new connections when it resolves. Passing
/// `shutdown_signal()` here lets every daemon drain in-flight requests before
/// the process exits, which is essential for connection-safe daemon upgrades
/// (issue #534). The shared helper guarantees trusty-search, trusty-memory, and
/// trusty-analyze all respond identically to `launchctl bootout` (SIGTERM).
///
/// What: on unix, registers handlers for both `SIGTERM` and `SIGINT` at
/// construction time and resolves when the first one fires. On non-unix
/// platforms (Windows), only Ctrl-C is watched. Signal registration errors
/// are downgraded to a warning; the function then falls back to watching
/// Ctrl-C only so the daemon still responds to interactive interrupts.
///
/// Test: compile with `cargo check -p trusty-common`; end-to-end coverage is
/// in `crates/trusty-search/tests/` which boots an axum daemon and sends SIGTERM.
pub async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};

        let mut term = match signal(SignalKind::terminate()) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(
                    "trusty-common: failed to install SIGTERM handler: {e}; \
                     falling back to SIGINT/Ctrl-C only"
                );
                let _ = tokio::signal::ctrl_c().await;
                return;
            }
        };

        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("trusty-common: received SIGINT/Ctrl-C — initiating graceful shutdown");
            }
            _ = term.recv() => {
                tracing::info!("trusty-common: received SIGTERM — initiating graceful shutdown");
            }
        }
    }

    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
        tracing::info!("trusty-common: received Ctrl-C — initiating graceful shutdown");
    }
}

#[cfg(test)]
mod tests {
    /// Why: confirm the module compiles and the public surface is callable.
    /// What: creates a future from `shutdown_signal()` without polling it
    ///   (which would block forever waiting for a real signal).
    /// Test: `cargo test -p trusty-common -- shutdown::tests`.
    #[test]
    fn shutdown_signal_is_callable() {
        // Just constructing the future (without awaiting) confirms the function
        // compiles and has the expected signature: `async fn() -> ()`.
        let _fut = super::shutdown_signal();
        // The future is dropped here without being polled — no signal is sent.
    }
}
