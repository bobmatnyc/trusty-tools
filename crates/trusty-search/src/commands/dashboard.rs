//! Handler for `trusty-search dashboard` and `monitor web` — open the admin
//! panel in a browser.
//!
//! Why: `dashboard` / `dash` / `ui` is a convenience entrypoint: the user
//! should never have to know which port the daemon chose or whether it is
//! running yet. Mirrors the trusty-analyze pattern from PR #685.
//!
//! What: `dashboard` ensures the daemon answers on its socket (spawning it in
//! the background when no daemon runs), then asks it over `search.health`
//! which HTTP address it bound (#9214) and opens `http://<addr>/ui`. A
//! socket-only daemon (`--no-http`) has no dashboard to serve until phase C,
//! so both commands fail naming the socket; they never guess a port. On
//! browser-open failure (headless env) degrades gracefully by printing the
//! URL to stderr rather than returning an error.
//!
//! Test: `dashboard_under_no_http_errors_and_opens_nothing` and
//! `open_dashboard_via_opens_the_address_the_daemon_reports` in this module;
//! `dashboard_never_dials_a_default_port` drives the binary end to end.

use super::port::{probe_http_listener, socket_only_message, HttpListener};
use anyhow::{anyhow, Result};
use colored::Colorize;
use trusty_search::service::daemon_client::DaemonClient;

/// Open the admin panel of the running daemon in the default browser.
///
/// Why: provides a one-command path from "is the daemon up?" to "show me the
/// UI" without the user having to memorise ports or run `trusty-search start`
/// first. Auto-starts the daemon when absent, matching the trusty-analyze
/// dashboard (#685) for a consistent UX across the suite.
/// What: ensures the daemon answers on its socket (spawning it when no daemon
/// process runs; never dials TCP), then [`open_dashboard_via`] with the real
/// `open::that` opener.
/// Test: `dashboard_never_dials_a_default_port`.
pub async fn handle_dashboard() -> Result<()> {
    // #9214: wait on the socket, not on a guessed `http://127.0.0.1:7878`.
    let client = DaemonClient::resolve()?;
    crate::commands::daemon_guard::ensure_daemon_up(&client).await?;
    open_dashboard_via(&client, |u| open::that(u)).await
}

/// `trusty-search monitor web`: print the admin panel URL and try to open it.
///
/// Why (#9214): it used to print the port-discovery URL, which falls back to
/// the default port when no daemon is discovered. It now shares the dashboard's
/// rule — the URL comes from the daemon, or the command fails.
/// What: [`dashboard_base`], then prints `<base>/ui` to stdout and opens it,
/// ignoring an open failure as before. Never starts a daemon.
/// Test: `dashboard_under_no_http_errors_and_opens_nothing` covers the shared
/// [`dashboard_base`] gate.
pub async fn handle_monitor_web() -> Result<()> {
    let client = DaemonClient::resolve()?;
    let url = dashboard_url(&dashboard_base(&client).await?);
    println!("{url}");
    open::that(&url).ok();
    Ok(())
}

/// The `http://host:port` base of the dashboard the daemon on `client` serves.
///
/// Why (#9214): only the daemon knows whether it bound HTTP. The dashboard
/// needs that listener until phase C moves it, so a socket-only daemon is an
/// error naming the socket — never a guessed `:7878`.
/// What: [`probe_http_listener`]; a bound address becomes `http://<addr>`.
///
/// # Errors
///
/// When no daemon answers on the socket, when it is socket-only, or when it
/// reports no HTTP address.
///
/// Test: `dashboard_under_no_http_errors_and_opens_nothing`.
pub(crate) async fn dashboard_base(client: &DaemonClient) -> Result<String> {
    let socket = client.socket().display();
    match probe_http_listener(client).await? {
        HttpListener::Bound(addr) => Ok(format!("http://{addr}")),
        HttpListener::SocketOnly => Err(anyhow!(
            "{}: the dashboard needs the daemon's HTTP listener; \
             restart the daemon without --no-http",
            socket_only_message(client.socket())
        )),
        HttpListener::Unreported => Err(anyhow!(
            "the daemon at socket {socket} reported no HTTP address; restart it"
        )),
    }
}

/// Resolve the dashboard through `client`, then open it with `open_fn`.
///
/// Why: the seam the tests use — `open_fn` stands in for the OS browser, so a
/// test proves no browser opens without ever launching one.
/// What: [`dashboard_base`], then [`open_dashboard_url_with`]. `open_fn` is
/// never called when the base cannot be resolved.
///
/// # Errors
///
/// As [`dashboard_base`].
///
/// Test: `dashboard_under_no_http_errors_and_opens_nothing`,
/// `open_dashboard_via_opens_the_address_the_daemon_reports`.
pub(crate) async fn open_dashboard_via<F>(client: &DaemonClient, open_fn: F) -> Result<()>
where
    F: FnOnce(&str) -> std::io::Result<()>,
{
    let base = dashboard_base(client).await?;
    open_dashboard_url_with(open_fn, &base)
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

    /// A mock daemon whose `search.health` reports `http_addr`; every other
    /// method is refused `-32601`, as an unknown method is by the real daemon.
    async fn daemon_reporting(
        http_addr: Option<&'static str>,
    ) -> crate::commands::mock_socket::MockDaemon {
        use trusty_common::uds::server::RpcError;
        crate::commands::mock_socket::mock_daemon(move |method, _params| match method {
            "search.health" => Ok(serde_json::json!({
                "status": "ok",
                "transport": { "socket_path": "ts.sock", "http_addr": http_addr },
            })),
            other => Err(RpcError::method_not_found(other, &["search.health"])),
        })
        .await
    }

    /// Why (#9214): a socket-only daemon has no dashboard to serve, so the
    /// command must fail naming the socket and open nothing.
    /// What: a mock daemon reporting `http_addr: null`; asserts an error that
    /// names the socket and `--no-http`, and that the opener never ran.
    /// Test: this function — the opener is a recording closure.
    #[tokio::test]
    async fn dashboard_under_no_http_errors_and_opens_nothing() {
        let daemon = daemon_reporting(None).await;
        let mut opened: Vec<String> = Vec::new();
        let result = open_dashboard_via(&daemon.client, |url| {
            opened.push(url.to_string());
            Ok(())
        })
        .await;

        let message = format!(
            "{:#}",
            result.expect_err("a socket-only daemon must be an error")
        );
        let socket = daemon.client.socket().display().to_string();
        assert!(
            message.contains(&format!(
                "no HTTP listener (socket-only daemon at {socket})"
            )),
            "the error must name the socket: {message}"
        );
        assert!(
            message.contains("--no-http"),
            "the error must name the flag: {message}"
        );
        assert!(opened.is_empty(), "no browser may open: {opened:?}");
    }

    /// Why (#9214): with HTTP up, the dashboard opens the address the daemon
    /// reports — not a discovery file and not a default port.
    /// What: a mock daemon reporting `127.0.0.1:41234`; asserts the opener
    /// receives that address's `/ui`.
    /// Test: this function.
    #[tokio::test]
    async fn open_dashboard_via_opens_the_address_the_daemon_reports() {
        let daemon = daemon_reporting(Some("127.0.0.1:41234")).await;
        let mut opened: Vec<String> = Vec::new();
        open_dashboard_via(&daemon.client, |url| {
            opened.push(url.to_string());
            Ok(())
        })
        .await
        .expect("a daemon with HTTP opens its dashboard");
        assert_eq!(opened, vec!["http://127.0.0.1:41234/ui".to_string()]);
    }
}
