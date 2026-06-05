//! Handler for `trusty-search dashboard` — open the admin panel in a browser.
//!
//! Why: `dashboard` / `dash` / `ui` is a convenience entrypoint: the user
//! should never have to know which port the daemon chose or whether it is
//! running yet. Mirrors the trusty-analyze pattern from PR #685.
//!
//! What: calls `ensure_daemon_running_or_exit` which spawns the daemon in the
//! background if it is not yet running and polls `/health` until ready (60s
//! budget with a braille spinner). Then discovers the bound address via
//! `daemon_base_url()` (which reads `~/.trusty-search/http_addr` with TCP
//! fallbacks) and opens `http://<addr>/ui` in the default browser. On
//! browser-open failure (headless env) degrades gracefully by printing the
//! URL to stderr rather than returning an error.
//!
//! Test: `cargo run -- dashboard` with no daemon running spawns the daemon
//! and opens the browser; with the daemon already running, the probe returns
//! immediately and no spawn occurs; headless (no GUI) prints the URL.

use super::daemon_utils::daemon_base_url;
use anyhow::Result;
use colored::Colorize;

/// Open the admin panel of the running daemon in the default browser.
///
/// Why: provides a one-command path from "is the daemon up?" to "show me the
/// UI" without the user having to memorise ports or run `trusty-search start`
/// first. Auto-starts the daemon when absent, matching the trusty-analyze
/// dashboard (#685) for a consistent UX across the suite.
/// What: ensures the daemon is running (spawning it in the background if
/// needed), resolves the bound address via `daemon_base_url()`, then opens
/// `http://<addr>/ui`. Browser-open failure degrades to a printed URL.
/// Test: `cargo test -p trusty-search -- dashboard` exercises the
/// already-healthy path. Manual: `cargo run -- dashboard` with no daemon
/// should print the "Starting…" spinner and then open the browser.
pub async fn handle_dashboard() -> Result<()> {
    // daemon_base_url() builds the URL from whatever address discovery info is
    // available (http_addr file → port file → compiled-in default). We pass
    // this to ensure_daemon_running_or_exit so it probes the right endpoint.
    let base = daemon_base_url();
    crate::commands::daemon_guard::ensure_daemon_running_or_exit(&base).await?;

    // Re-resolve after the daemon is confirmed ready: if the daemon just
    // started, it will have written `http_addr` by now and daemon_base_url()
    // will return the exact bound address instead of the default fallback.
    let base = daemon_base_url();
    open_dashboard_url(&base)
}

/// Construct the `/ui` URL from `base` and return it as a `String`.
///
/// Why: pure URL construction extracted as its own function so tests can
/// verify the correct path suffix is appended without triggering any I/O.
/// What: trims a trailing slash from `base` (guards against `http://h:p//ui`)
/// then appends `/ui`, returning the resulting `String`.
/// Test: `dashboard_url_is_constructed_correctly` and
/// `dashboard_url_has_no_double_slash` in this module cover the two
/// interesting inputs (plain base and base with trailing slash).
pub(crate) fn dashboard_url(base: &str) -> String {
    format!("{}/ui", base.trim_end_matches('/'))
}

/// Open `base`'s `/ui` path using the provided opener closure.
///
/// Why: extracted so tests can inject a fake opener that never calls the real
/// OS browser API — the historical `open_dashboard_url` called `open::that`
/// directly, which meant every `cargo test` on a macOS GUI session spawned a
/// dead browser tab to `http://127.0.0.1:19999/ui`.
/// What: constructs the URL via `dashboard_url`, prints it to stderr, then
/// calls `open_fn(&url)`. If `open_fn` returns `Err`, degrades gracefully by
/// printing a warning to stderr rather than propagating the error. Always
/// returns `Ok(())`.
/// Test: `open_dashboard_url_degrades_gracefully_on_headless` in this module
/// passes a closure that returns `Err` and asserts the result is `Ok(())`.
pub(crate) fn open_dashboard_url_with<F>(open_fn: F, base: &str) -> Result<()>
where
    F: FnOnce(&str) -> std::io::Result<()>,
{
    let url = dashboard_url(base);
    eprintln!("{} Opening {} …", "◉".green(), url.cyan());
    if let Err(e) = open_fn(&url) {
        eprintln!(
            "{} could not launch browser ({e}). Open this URL manually: {}",
            "⚠".yellow(),
            url
        );
    }
    Ok(())
}

/// Construct the `/ui` URL from `base` and open it in the default browser.
///
/// Why: thin public wrapper over `open_dashboard_url_with` using the real
/// `open::that` opener — keeps production behaviour identical while the inner
/// function remains testable via closure injection.
/// What: delegates to `open_dashboard_url_with(|u| open::that(u), base)`.
/// Browser-open failure degrades to a stderr warning; `Ok(())` is always
/// returned.
/// Test: not tested directly (the real `open::that` fires a browser). The
/// logic is fully covered by `open_dashboard_url_with` tests which inject a
/// fake opener.
fn open_dashboard_url(base: &str) -> Result<()> {
    open_dashboard_url_with(|u| open::that(u), base)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: the URL construction is the only logic we can exercise without a
    /// real daemon or OS browser. A regression here would silently send users
    /// to the wrong path (e.g. the root `/` instead of `/ui`).
    /// What: asserts that `dashboard_url` builds `<base>/ui` for a plain
    /// base address with no trailing slash.
    /// Test: this function — pure, no I/O.
    #[test]
    fn dashboard_url_is_constructed_correctly() {
        assert_eq!(
            dashboard_url("http://127.0.0.1:7878"),
            "http://127.0.0.1:7878/ui"
        );
    }

    /// Why: guards against the base URL gaining a trailing slash that would
    /// produce a double-slash in the final URL (`http://127.0.0.1:7878//ui`).
    /// What: asserts that `dashboard_url` strips a trailing slash from `base`
    /// before appending `/ui`, producing exactly one slash before `ui`.
    /// Test: this function — pure, no I/O.
    #[test]
    fn dashboard_url_has_no_double_slash() {
        let url = dashboard_url("http://127.0.0.1:7878/");
        assert_eq!(url, "http://127.0.0.1:7878/ui");
        assert!(
            !url.contains("//ui"),
            "URL must not contain double-slash before ui: {url}"
        );
    }

    /// Why: the real `open::that` call succeeds on macOS GUI sessions, so any
    /// test that passes a real URL to `open_dashboard_url` / `open::that`
    /// fires a browser tab — polluting every local test run. This test
    /// verifies the graceful-degradation path by injecting a fake opener that
    /// always returns `Err`, confirming the function returns `Ok(())` without
    /// ever calling the real browser API.
    /// What: calls `open_dashboard_url_with` with a closure that returns
    /// `Err(io::Error::other("headless"))`, then asserts the return value is
    /// `Ok(())`.
    /// Test: this function — no real `open::that` is ever called.
    #[test]
    fn open_dashboard_url_degrades_gracefully_on_headless() {
        let result = open_dashboard_url_with(
            |_url| Err(std::io::Error::other("headless: no display")),
            "http://127.0.0.1:19999",
        );
        assert!(
            result.is_ok(),
            "headless browser-open failure must not surface as Err"
        );
    }

    /// Why: confirms that a successful opener (simulating a working GUI
    /// session) still results in `Ok(())` — the happy path is not accidentally
    /// broken by the refactor.
    /// What: calls `open_dashboard_url_with` with a no-op closure that returns
    /// `Ok(())`, then asserts the result is `Ok(())` and the URL passed to
    /// the opener has the expected `/ui` suffix.
    /// Test: this function — no real `open::that` is ever called.
    #[test]
    fn open_dashboard_url_with_succeeds_on_working_opener() {
        let mut received_url = String::new();
        let result = open_dashboard_url_with(
            |url| {
                received_url = url.to_string();
                Ok(())
            },
            "http://127.0.0.1:7878",
        );
        assert!(result.is_ok(), "working opener must return Ok");
        assert_eq!(
            received_url, "http://127.0.0.1:7878/ui",
            "opener must receive the /ui URL"
        );
    }
}
