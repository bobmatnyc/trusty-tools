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
//! Test: `cargo test -p trusty-common --features unconditional-only --
//! shutdown` runs the compilation smoke test. Signal delivery itself cannot be
//! triggered inside a unit test without `raise(SIGTERM)`, which is unsafe; the
//! integration tests in trusty-search exercise the full axum
//! `with_graceful_shutdown` path.

/// Seconds a trusty-* daemon is granted between SIGTERM and SIGKILL (#4393).
///
/// Why: this is the ONE number every terminator and every terminated daemon has
/// to agree on, and before #4393 nobody stated it. launchd's `ExitTimeOut`
/// default is documented only as "system-defined" and measures **5 s** on
/// macOS; `trusty-search stop` allowed 5 s; its orphan reaper allowed 3 s.
/// Meanwhile trusty-search's shutdown flush floors each index's budget at 30 s
/// (`service::shutdown_flush::MIN_FLUSH_TIMEOUT_SECS`), so no flush that had
/// real work to do could ever run to completion — it was SIGKILLed mid-write on
/// every path. Publishing the window as a shared constant is what lets the
/// plist renderer, the CLI stop path, the reaper, and the daemon's own flush
/// planner be checked against each other instead of drifting silently.
///
/// What: 60 s. Deliberately modest — long enough to clear the 30 s per-index
/// floor, short enough that a wedged daemon does not stall reboot or
/// `launchctl bootout` for minutes. Not derived from the flush budget's
/// 20-minute ceiling: a multi-minute `ExitTimeOut` in a static plist trades one
/// operator-visible failure for a worse one.
///
/// Test: `termination_grace_clears_the_measured_launchd_default`,
/// `render_plist_declares_exit_timeout`. trusty-search additionally asserts
/// this window covers its own per-index flush floor, in
/// `service::shutdown_budget`, `commands::stop`, and
/// `commands::start::reap_orphans`.
pub const TERMINATION_GRACE_SECS: u64 = 60;

/// Operator override for [`TERMINATION_GRACE_SECS`].
///
/// Why: a host whose launchd `ExitTimeOut` cannot be raised (a stale installed
/// plist, a container supervisor with its own `TimeoutStopSec`) needs to tell
/// the daemon the truth about its window, or the daemon plans a 55 s flush
/// inside a 5 s life and loses more than it would have with an accurate,
/// smaller budget. The env var is how the terminator declares the real number.
/// What: `TRUSTY_TERMINATION_GRACE_SECS`, a positive integer count of seconds.
/// Test: `termination_grace_honours_a_valid_override`,
/// `termination_grace_ignores_junk_overrides`.
pub const TERMINATION_GRACE_ENV: &str = "TRUSTY_TERMINATION_GRACE_SECS";

/// The termination window this process should plan for.
///
/// Why: reads the override at the one place that owns the policy so no caller
/// re-implements the parse. See [`TERMINATION_GRACE_SECS`] for why the number
/// exists at all.
/// What: [`termination_grace_from`] applied to [`TERMINATION_GRACE_ENV`].
/// Test: covered through `termination_grace_from`'s tests; this wrapper only
/// supplies the env read.
pub fn termination_grace() -> std::time::Duration {
    termination_grace_from(std::env::var(TERMINATION_GRACE_ENV).ok().as_deref())
}

/// Pure half of [`termination_grace`], over an already-read env value.
///
/// Why: pure so the parse and its fallbacks are testable without mutating the
/// process environment (which races every other test in the binary).
/// What: a trimmed, positive integer wins; unset, empty, `0`, and unparseable
/// all fall back to [`TERMINATION_GRACE_SECS`]. A junk value must never shorten
/// the window to zero — a zero-length grace makes every flush a no-op.
/// Test: `termination_grace_honours_a_valid_override`,
/// `termination_grace_ignores_junk_overrides`.
pub fn termination_grace_from(value: Option<&str>) -> std::time::Duration {
    let secs = value
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|&s| s > 0)
        .unwrap_or(TERMINATION_GRACE_SECS);
    std::time::Duration::from_secs(secs)
}

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
    /// Test: `cargo test -p trusty-common --features unconditional-only --
    /// shutdown::tests`.
    #[test]
    fn shutdown_signal_is_callable() {
        // Just constructing the future (without awaiting) confirms the function
        // compiles and has the expected signature: `async fn() -> ()`.
        let _fut = super::shutdown_signal();
        // The future is dropped here without being polled — no signal is sent.
    }

    /// Why (#4393): the measured launchd `ExitTimeOut` default on macOS is 5 s,
    /// and trusty-search's shutdown flush floors each index at 30 s. A grace
    /// constant at or below either number reproduces the defect this issue
    /// reports — the flush cannot finish before SIGKILL. Pinning the floor here
    /// means a later "let's shorten it a bit" edit fails loudly.
    /// What: asserts the constant clears both the measured 5 s system default
    /// and trusty-search's 30 s per-index floor.
    /// Test: itself.
    #[test]
    fn termination_grace_clears_the_measured_launchd_default() {
        // `const` blocks: both operands are compile-time constants, so the
        // check fires at build time rather than waiting for the test binary.
        const {
            assert!(
                super::TERMINATION_GRACE_SECS > 5,
                "the grace window must exceed launchd's measured 5 s ExitTimeOut default"
            );
        }
        const {
            assert!(
                super::TERMINATION_GRACE_SECS >= 30,
                "the grace window must cover trusty-search's 30 s per-index flush floor"
            );
        }
    }

    /// Why (#4393): a host that cannot raise its supervisor's real window has
    /// to be able to tell the daemon the truth, or the daemon over-plans.
    /// What: a valid positive override wins over the constant.
    /// Test: itself.
    #[test]
    fn termination_grace_honours_a_valid_override() {
        assert_eq!(
            super::termination_grace_from(Some(" 12 ")),
            std::time::Duration::from_secs(12)
        );
    }

    /// Why (#4393): a zero or unparseable override must never collapse the
    /// window to nothing — a zero grace turns every shutdown flush into a no-op,
    /// which is the data loss this issue exists to stop, self-inflicted.
    /// What: unset, empty, `0`, and junk all fall back to the constant.
    /// Test: itself.
    #[test]
    fn termination_grace_ignores_junk_overrides() {
        let default = std::time::Duration::from_secs(super::TERMINATION_GRACE_SECS);
        for value in [None, Some(""), Some("0"), Some("soon"), Some("-5")] {
            assert_eq!(
                super::termination_grace_from(value),
                default,
                "junk override {value:?} must fall back to the constant"
            );
        }
    }
}
