//! The daemon's HTTP address, for the subcommands still on the HTTP listener.
//!
//! Why: #9214 B2(a) moved `daemon_utils` and `daemon_guard` onto the socket,
//! but about twenty subcommands still send their requests over HTTP until
//! phases B2(b)–(d) move them. They need the HTTP base URL, and the resolver
//! they used fell back to `127.0.0.1:7878` when the daemon published no
//! address. That fallback reached whatever held the port — a different
//! instance, or nothing — so it is removed here: this module fails closed.
//! The file is listed in `NOT_YET_MOVED` and is deleted with its last caller.
//!
//! What: [`daemon_base_url`] reads the daemon's own discovery files and errors
//! when neither names an address. [`ensure_daemon_http_base`] is the auto-start
//! guard for those callers: an HTTP fast path, then the socket guard, then the
//! fail-closed resolve.
//! Test: `daemon_base_url_refuses_when_no_address_is_published`,
//! `daemon_base_url_prefers_isolated_instance_over_stale_default_cache`,
//! `daemon_base_url_falls_back_when_http_addr_dead`.

use std::time::Duration;

use anyhow::{anyhow, Result};
use trusty_common::daemon_guard::{probe_once, write_addr_file_atomic, DaemonAddrLayout};
use trusty_search::service::daemon_client::DaemonClient;

/// Budget for one discovery-file reachability probe.
const ADDR_PROBE_TIMEOUT: Duration = Duration::from_millis(200);

/// Resolve the daemon's HTTP base URL from the discovery files it wrote.
///
/// Why: same precedence as the shared resolver (#3545, #5670) — the
/// `http_addr` file when it is live, else the `daemon.port` file — minus its
/// compiled-in default port. #9214: a daemon that published neither file is
/// stopped or runs with `--no-http`; guessing `:7878` then reached a different
/// daemon, so the caller now gets an error instead.
/// What: returns `http://{host}:{port}`, no trailing slash. A live `http_addr`
/// wins. Otherwise a readable port file decides the port on `127.0.0.1`, and
/// the `http_addr` file is refreshed when that address answers (#117).
///
/// # Errors
///
/// When neither discovery file names an address. The message names both files.
///
/// Test: `daemon_base_url_refuses_when_no_address_is_published`,
/// `daemon_base_url_prefers_isolated_instance_over_stale_default_cache`,
/// `daemon_base_url_falls_back_when_http_addr_dead`.
pub fn daemon_base_url() -> Result<String> {
    let layout = DaemonAddrLayout::TRUSTY_SEARCH;
    let addr_file = layout.discovery_file_path();
    if let Some(raw) = addr_file
        .as_ref()
        .and_then(|p| std::fs::read_to_string(p).ok())
    {
        let addr = raw.trim();
        if !addr.is_empty() && reachable(addr) {
            return Ok(format!("http://{addr}"));
        }
    }
    let port_file = layout.port_file_path();
    // #9214: no default port — an unpublished address is an error, not :7878.
    let Some(port) = port_file
        .as_ref()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| s.trim().parse::<u16>().ok())
    else {
        return Err(anyhow!(
            "the trusty-search daemon has published no HTTP address ({} and {} \
             are absent) — it is stopped, or was started with --no-http; start it \
             with `trusty-search start`",
            display(addr_file.as_deref()),
            display(port_file.as_deref()),
        ));
    };
    let live_addr = format!("127.0.0.1:{port}");
    // #3602: the refresh goes through the atomic writer; best-effort.
    if let Some(path) = addr_file.as_ref().filter(|_| reachable(&live_addr)) {
        let _ = write_addr_file_atomic(path, &live_addr);
    }
    Ok(format!("http://{live_addr}"))
}

/// Ensure the daemon is up, then return its HTTP base URL.
///
/// Why: the auto-start guard for subcommands still on HTTP. The old guard
/// probed the base URL it was handed, which the resolver had already guessed
/// when no daemon was up; with the guess gone (#9214), the URL can only be
/// resolved after the daemon has published it.
/// What: when the published address answers `GET /health`, returns it (the
/// pre-#9214 fast path, which the mock-HTTP integration tests rely on).
/// Otherwise waits on the socket through
/// [`super::daemon_guard::ensure_daemon_up_with_device`], spawning the daemon
/// when none is running, and resolves the address the daemon then published.
///
/// # Errors
///
/// When the daemon cannot be started, or answers on its socket but publishes
/// no HTTP address.
///
/// Test: `daemon_base_url_refuses_when_no_address_is_published` covers the
/// resolve; the guard is covered by the CLI integration tests.
pub async fn ensure_daemon_http_base() -> Result<String> {
    ensure_with_device(None).await
}

/// [`ensure_daemon_http_base`] for the indexing flow, which starts the daemon
/// on the indexing device (issue #24).
///
/// # Errors
///
/// The same set as [`ensure_daemon_http_base`].
pub async fn ensure_daemon_http_base_for_indexing() -> Result<String> {
    let device = super::daemon_guard::indexing_device();
    ensure_with_device(device.as_deref()).await
}

/// The shared body of the two guards above.
async fn ensure_with_device(device: Option<&str>) -> Result<String> {
    if let Ok(base) = daemon_base_url() {
        if probe_once(&format!("{base}/health")).await {
            return Ok(base);
        }
    }
    let client = DaemonClient::resolve()?;
    super::daemon_guard::ensure_daemon_up_with_device(&client, device).await?;
    daemon_base_url().map_err(|e| {
        anyhow!(
            "the trusty-search daemon answers on socket {} but {e}",
            client.socket().display()
        )
    })
}

/// Time-boxed TCP reachability check of a `host:port` string.
fn reachable(host_port: &str) -> bool {
    use std::net::{TcpStream, ToSocketAddrs};
    host_port
        .to_socket_addrs()
        .ok()
        .and_then(|mut it| it.next())
        .is_some_and(|addr| TcpStream::connect_timeout(&addr, ADDR_PROBE_TIMEOUT).is_ok())
}

/// A discovery-file path for an error message.
fn display(path: Option<&std::path::Path>) -> String {
    path.map_or_else(|| "<unresolvable>".to_string(), |p| p.display().to_string())
}

#[cfg(test)]
mod tests {
    //! The fail-closed HTTP base resolver (#3545, #9214).

    use super::*;
    use serial_test::serial;

    /// Point `TRUSTY_DATA_DIR` at `dir` for the life of the guard.
    struct DataDir;

    impl DataDir {
        fn set(dir: &std::path::Path) -> Self {
            // SAFETY: every caller is `#[serial]` — the crate's one env group.
            unsafe { std::env::set_var("TRUSTY_DATA_DIR", dir) };
            DataDir
        }
    }

    impl Drop for DataDir {
        fn drop(&mut self) {
            // SAFETY: as above.
            unsafe { std::env::remove_var("TRUSTY_DATA_DIR") };
        }
    }

    /// #9214: with neither discovery file present the resolver errors, naming
    /// the files, instead of guessing the default port.
    ///
    /// Why: the guess reached whatever held `127.0.0.1:7878` — a different
    /// instance, or nothing — and a `--no-http` daemon publishes no address at
    /// all. Fails against the pre-#9214 resolver, which returned the default URL.
    /// What: an empty isolated data dir; asserts `Err`, that the message names
    /// both files, and that it carries no URL.
    /// Test: this function.
    #[test]
    #[serial]
    fn daemon_base_url_refuses_when_no_address_is_published() {
        let dir = tempfile::tempdir().unwrap();
        let _env = DataDir::set(dir.path());

        let err = daemon_base_url().expect_err("nothing was published");
        let text = err.to_string();

        assert!(text.contains("http_addr"), "{text}");
        assert!(text.contains("daemon.port"), "{text}");
        assert!(text.contains("--no-http"), "{text}");
        assert!(!text.contains("http://"), "no URL is guessed: {text}");
    }

    /// Regression for issue #3545: an isolated `TRUSTY_DATA_DIR` instance must
    /// be addressed by `daemon_base_url()` even when a *different* daemon's
    /// address is cached at the old, non-isolated location that pre-fix code
    /// consulted first.
    ///
    /// Why: the PR #3529 incident — the CLI reconnected to whatever daemon was
    /// cached at the generic discovery location and mutated its index.
    /// What: two live listeners; the decoy is seeded at the OLD cache (inside a
    /// `TRUSTY_DATA_DIR_OVERRIDE` tempdir), the isolated one in the isolated
    /// `http_addr`; asserts the isolated address wins.
    /// Test: this function.
    #[test]
    #[serial]
    fn daemon_base_url_prefers_isolated_instance_over_stale_default_cache() {
        let isolated_listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let isolated_addr = isolated_listener.local_addr().unwrap().to_string();
        let decoy_listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let decoy_addr = decoy_listener.local_addr().unwrap().to_string();

        let override_tmp = tempfile::tempdir().unwrap();
        let data_dir_tmp = tempfile::tempdir().unwrap();

        // SAFETY: `#[serial]`.
        unsafe { std::env::set_var("TRUSTY_DATA_DIR_OVERRIDE", override_tmp.path()) };
        trusty_common::write_daemon_addr("trusty-search", &decoy_addr).unwrap();

        let env = DataDir::set(data_dir_tmp.path());
        let isolated_http_addr = trusty_search::service::http_addr_path().unwrap();
        std::fs::write(&isolated_http_addr, &isolated_addr).unwrap();

        let url = daemon_base_url();

        drop(env);
        // SAFETY: `#[serial]`.
        unsafe { std::env::remove_var("TRUSTY_DATA_DIR_OVERRIDE") };

        assert_eq!(
            url.expect("the isolated address is published"),
            format!("http://{isolated_addr}"),
            "must target the isolated instance ({isolated_addr}), not the decoy ({decoy_addr})"
        );
    }

    /// Regression for issue #3545: a dead `http_addr` falls back to the isolated
    /// `daemon.port` file — never to another instance or the default port — and
    /// the discovery file is refreshed in place.
    ///
    /// What: a dead address in `http_addr`, a live listener's port in
    /// `daemon.port`; asserts the live address and the refreshed file.
    /// Test: this function.
    #[test]
    #[serial]
    fn daemon_base_url_falls_back_when_http_addr_dead() {
        let live_listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let live_port = live_listener.local_addr().unwrap().port();

        let data_dir_tmp = tempfile::tempdir().unwrap();
        let _env = DataDir::set(data_dir_tmp.path());

        let http_addr_path = trusty_search::service::http_addr_path().unwrap();
        std::fs::write(&http_addr_path, "127.0.0.1:1").unwrap(); // dead: reserved port
        let port_path = super::super::daemon_utils::daemon_port_path().unwrap();
        std::fs::write(&port_path, live_port.to_string()).unwrap();

        let url = daemon_base_url().expect("the port file names an address");

        assert_eq!(url, format!("http://127.0.0.1:{live_port}"));
        let refreshed = std::fs::read_to_string(&http_addr_path).unwrap();
        assert_eq!(refreshed.trim(), format!("127.0.0.1:{live_port}"));
    }
}
