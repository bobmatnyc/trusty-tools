//! Daemon discovery + reachability helpers shared across CLI subcommands.
//!
//! Why: every subcommand that talks to the running daemon needs the same
//! "where is it listening?" logic. That logic no longer lives here: #5670
//! promoted it to `trusty_common::daemon_guard::DaemonAddrLayout`, because
//! `tga` has to probe this daemon and cannot depend on this crate. What is
//! left is the thin binding to `DaemonAddrLayout::TRUSTY_SEARCH` plus the two
//! path helpers that are genuinely local to this CLI.
//!
//! Why the promoted resolver prefers `http_addr_path()` over
//! `trusty_common::read_daemon_addr("trusty-search")`: see #3545. The generic
//! per-app resolver honours only the test-only `TRUSTY_DATA_DIR_OVERRIDE`,
//! never `TRUSTY_DATA_DIR`, and names a third location distinct from the one
//! the daemon writes — so it went stale as a cross-instance cache that
//! outranked an isolated instance on every later call.
//!
//! What: two pure path resolvers, one async TCP probe, and one delegation.
//! Test: `trusty_common::daemon_guard::addr_tests` covers the resolution
//! paths; `daemon_base_url_falls_back_when_http_addr_dead` and
//! `daemon_base_url_prefers_isolated_instance_over_stale_default_cache` below
//! prove this crate's binding still satisfies the #117 / #3545 contracts.

use std::time::Duration;

/// Resolve the daemon's base URL.
///
/// Why: stdio MCP servers and CLI subcommands need to find the running daemon
/// without configuration.
/// What: delegates to the shared resolver (#5670). Returns
/// `http://{host}:{port}`, no trailing slash; see
/// [`trusty_common::daemon_guard::DaemonAddrLayout::resolve_base_url`] for the
/// discovery-file / port-file / default-port precedence and the #117
/// reachability probe.
/// Test: `daemon_base_url_falls_back_when_http_addr_dead`,
/// `daemon_base_url_prefers_isolated_instance_over_stale_default_cache`.
pub fn daemon_base_url() -> String {
    // #5670: one implementation, in trusty-common — tga probes the same daemon.
    trusty_common::daemon_guard::DaemonAddrLayout::TRUSTY_SEARCH.resolve_base_url()
}

/// Path to `~/.trusty-search/mcp_http_addr` -- the MCP HTTP/SSE listener's
/// address-discovery file, written by `trusty-search serve --http`.
///
/// Why: distinct from the daemon's `http_addr` (written via
/// `trusty_common::write_daemon_addr`) so two unrelated processes (the daemon
/// and a `serve --http` MCP transport) cannot clobber each other. Before
/// issue #117 both wrote the same file; a SIGKILL'd `serve --http` would
/// leave a dead-address file behind, stranding subsequent
/// `trusty-search dash`/`status` calls in a 60s timeout loop.
/// What: returns `$HOME/.trusty-search/mcp_http_addr`. This is intentionally
/// in `$HOME/.trusty-search/` (not the platform data dir) because it is a
/// per-session file that must be discovered by both the MCP client process and
/// the `serve` process across a potential `$TRUSTY_DATA_DIR_OVERRIDE` boundary.
/// Test: `mcp_http_addr_path_is_home_relative` unit test below.
pub fn mcp_http_addr_path() -> Option<std::path::PathBuf> {
    dirs::home_dir().map(|h| h.join(".trusty-search").join("mcp_http_addr"))
}

/// Path to the daemon port file (`daemon.port` under the resolved data dir).
///
/// Why: the port file records which TCP port the running daemon bound, so CLI
/// subcommands (`status`, `index`, `query`) can discover the daemon without
/// configuration. When `TRUSTY_DATA_DIR` is set (by `--data-dir` or the env
/// var), the port file lives in that directory so an isolated test/cert daemon
/// does not collide with the production daemon's port file (issue #281).
/// What: returns `$TRUSTY_DATA_DIR/daemon.port` when the env var is set,
/// otherwise `<data_local_dir>/trusty-search/daemon.port`. #5670 moved that
/// rule into `DaemonAddrLayout::TRUSTY_SEARCH`, which the promoted resolver
/// reads through, so both agree by construction.
/// Test: set `TRUSTY_DATA_DIR=/tmp/ts-x`; assert path equals
/// `/tmp/ts-x/daemon.port`.
pub fn daemon_port_path() -> Option<std::path::PathBuf> {
    trusty_common::daemon_guard::DaemonAddrLayout::TRUSTY_SEARCH.port_file_path()
}

/// Check whether a TCP port is open (non-blocking connect with 500 ms timeout).
pub async fn port_reachable(host: &str, port: u16) -> bool {
    let addr = format!("{}:{}", host, port);
    tokio::time::timeout(
        Duration::from_millis(500),
        tokio::net::TcpStream::connect(&addr),
    )
    .await
    .ok()
    .and_then(|r| r.ok())
    .is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    // #5670: the three `address_reachable_blocking` unit tests moved with the
    // probe itself to `trusty_common::daemon_guard::addr_tests`.

    #[test]
    fn mcp_http_addr_path_is_home_relative() {
        // Why: the MCP HTTP/SSE file must live in `$HOME/.trusty-search/` (not
        // the platform data dir) so it is accessible to both the `serve` and
        // the MCP client processes regardless of `TRUSTY_DATA_DIR_OVERRIDE`.
        // Test: verify the path ends with the expected basename.
        if let Some(p) = mcp_http_addr_path() {
            assert!(p.ends_with(".trusty-search/mcp_http_addr"));
        }
    }

    /// Regression for issue #3545: an isolated `TRUSTY_DATA_DIR` instance must
    /// be addressed by `daemon_base_url()` even when a *different* daemon's
    /// address is cached at the old, non-isolated location that pre-fix code
    /// consulted first (`trusty_common::write_daemon_addr("trusty-search",
    /// ...)`, resolved via `resolve_data_dir` -- keyed only to the test-only
    /// `TRUSTY_DATA_DIR_OVERRIDE`, never `TRUSTY_DATA_DIR`).
    ///
    /// Why: this is exactly the production incident from PR #3529 -- an
    /// operator ran an isolated instance with `TRUSTY_DATA_DIR` set and a
    /// non-default port, but the CLI silently reconnected to whatever daemon
    /// was cached at the generic discovery location (standing in for the
    /// default/production daemon here as `decoy_addr`) and mutated its index.
    /// What: binds two independent real TCP listeners (`isolated_addr` and
    /// `decoy_addr`); seeds the OLD wrong cache with `decoy_addr` (guarded by
    /// `TRUSTY_DATA_DIR_OVERRIDE` so the write lands in a tempdir, never a
    /// real `$HOME`/platform data dir); seeds the isolated instance's real
    /// `http_addr` file (`{TRUSTY_DATA_DIR}/http_addr`) with `isolated_addr`;
    /// asserts `daemon_base_url()` returns `isolated_addr`, proving the
    /// isolated instance wins and the decoy (standing in for the default
    /// daemon) is never targeted. This test fails against the pre-fix
    /// implementation (it would return `decoy_addr`).
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

        unsafe {
            std::env::set_var("TRUSTY_DATA_DIR_OVERRIDE", override_tmp.path());
        }
        // Simulate a stale cache at the OLD, non-isolated discovery location --
        // this is what any prior non-isolated invocation would have left behind.
        trusty_common::write_daemon_addr("trusty-search", &decoy_addr).unwrap();

        unsafe {
            std::env::set_var("TRUSTY_DATA_DIR", data_dir_tmp.path());
        }
        // Simulate the isolated instance's real discovery file, as `run_daemon()`
        // would write it under the fixed `http_addr_path()`.
        let isolated_http_addr = trusty_search::service::http_addr_path().unwrap();
        std::fs::write(&isolated_http_addr, &isolated_addr).unwrap();

        let url = daemon_base_url();

        unsafe {
            std::env::remove_var("TRUSTY_DATA_DIR");
            std::env::remove_var("TRUSTY_DATA_DIR_OVERRIDE");
        }

        assert_eq!(
            url,
            format!("http://{isolated_addr}"),
            "daemon_base_url() must target the isolated TRUSTY_DATA_DIR instance \
             ({isolated_addr}), not the stale default-location cache ({decoy_addr})"
        );
    }

    /// Regression for issue #3545: when the isolated instance's discovery file
    /// points at a dead address, `daemon_base_url()` must fall back to the
    /// isolated `daemon.port` file -- never to a different `TRUSTY_DATA_DIR`
    /// instance or the production default.
    ///
    /// Why: covers the reachability-probe + refresh path
    /// (`address_reachable_blocking` returning false) entirely within an
    /// isolated `TRUSTY_DATA_DIR`, so the refresh write never touches a real
    /// `$HOME`/platform-data-dir file.
    /// What: sets `TRUSTY_DATA_DIR` to a tempdir; writes a dead address to its
    /// `http_addr` file and a live listener's port to its `daemon.port` file;
    /// asserts `daemon_base_url()` returns the live listener's address.
    /// Test: this function.
    #[test]
    #[serial]
    fn daemon_base_url_falls_back_when_http_addr_dead() {
        let live_listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let live_port = live_listener.local_addr().unwrap().port();

        let data_dir_tmp = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("TRUSTY_DATA_DIR", data_dir_tmp.path());
        }

        let http_addr_path = trusty_search::service::http_addr_path().unwrap();
        std::fs::write(&http_addr_path, "127.0.0.1:1").unwrap(); // dead: reserved port
        let port_path = daemon_port_path().unwrap();
        std::fs::write(&port_path, live_port.to_string()).unwrap();

        let url = daemon_base_url();

        unsafe {
            std::env::remove_var("TRUSTY_DATA_DIR");
        }

        assert_eq!(
            url,
            format!("http://127.0.0.1:{live_port}"),
            "must fall back to the isolated instance's daemon.port when http_addr is dead"
        );
        // The refresh write should have corrected the discovery file in place.
        let refreshed = std::fs::read_to_string(&http_addr_path).unwrap();
        assert_eq!(refreshed.trim(), format!("127.0.0.1:{live_port}"));
    }
}
