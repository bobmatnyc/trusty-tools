//! Daemon discovery + reachability helpers shared across CLI subcommands.
//!
//! Why: every subcommand that talks to the running daemon needs the same
//! "where is it listening?" logic -- preferring the canonical `http_addr`
//! discovery file, falling back to the legacy port lockfile, and finally to
//! the compiled-in default port. Centralising it removes duplication and
//! gives `main.rs` a thinner footprint.
//!
//! Issue #3545: `daemon_base_url()` previously preferred
//! `trusty_common::read_daemon_addr("trusty-search")` /
//! `write_daemon_addr("trusty-search", ...)` -- a generic per-app resolver
//! (`trusty_common::resolve_data_dir`) that only honours the test-only
//! `TRUSTY_DATA_DIR_OVERRIDE` env var, never `TRUSTY_DATA_DIR`. That path is
//! also a *third*, distinct location from the one the daemon itself writes
//! (`trusty_search::service::http_addr_path()`), so it almost never matched
//! what `start` actually wrote -- and once the TCP-probe fallback below
//! populated it (from whichever daemon happened to be reachable on the
//! default port), it stuck as a stale cross-instance cache that silently
//! outranked an isolated `TRUSTY_DATA_DIR` instance on every later call. The
//! fix: read and refresh `trusty_search::service::http_addr_path()` directly
//! -- the exact same, now `TRUSTY_DATA_DIR`-aware resolver `run_daemon()` uses
//! to write the file -- so client subcommands and `start` always agree on
//! which file they mean.
//!
//! What: pure path resolvers and one async TCP probe.
//! Test: covered indirectly by every CLI subcommand that calls into the
//! daemon -- `status`, `index`, `query`, `doctor`, etc.

use std::time::Duration;

/// Resolve the daemon's base URL.
///
/// Why: stdio MCP servers and CLI subcommands need to find the running daemon
/// without configuration. We check the canonical address-discovery file
/// (`trusty_search::service::http_addr_path()`, issue #3545) first, then fall
/// back to the legacy port file (`daemon.port`) for backward compatibility,
/// and finally to `127.0.0.1:7878` if neither exists. Both the discovery file
/// and the port file honour `TRUSTY_DATA_DIR`, so an isolated instance's
/// clients never fall through to the production daemon's address.
///
/// Defensive TCP probe (issue #117): if the discovery file points at a dead
/// address (e.g. left behind by a SIGKILL'd `serve --http`, or by a stopped
/// daemon whose cleanup did not run), we fall back to `daemon.port` and
/// overwrite the discovery file with the live address so future callers are
/// fast. The probe is 200 ms -- short enough to keep CLI startup snappy when
/// the file is current, long enough to tolerate a busy machine.
///
/// What: returns `http://{host}:{port}` (no trailing slash).
/// Test: `daemon_base_url_falls_back_when_http_addr_dead` exercises this path;
/// `daemon_base_url_prefers_isolated_instance_over_stale_default_cache`
/// (issue #3545) proves an isolated `TRUSTY_DATA_DIR` instance is addressed
/// instead of a stale, non-isolated discovery cache.
pub fn daemon_base_url() -> String {
    if let Some(path) = trusty_search::service::http_addr_path() {
        if let Ok(raw) = std::fs::read_to_string(&path) {
            let addr = raw.trim();
            if !addr.is_empty() && address_reachable_blocking(addr) {
                return format!("http://{addr}");
            }
        }
        // Missing or stale file -- fall through to the port-file fallback and refresh.
    }
    let port = daemon_port_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| s.trim().parse::<u16>().ok())
        .unwrap_or(trusty_search::service::DEFAULT_PORT);

    // Refresh the discovery file so subsequent calls skip the TCP probe.
    // Best-effort: a write failure (no $HOME, read-only fs) is non-fatal.
    let live_addr = format!("127.0.0.1:{port}");
    if address_reachable_blocking(&live_addr) {
        if let Some(path) = trusty_search::service::http_addr_path() {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(&path, &live_addr);
        }
    }
    format!("http://{live_addr}")
}

/// Synchronous, time-boxed TCP reachability check used by `daemon_base_url()`.
///
/// Why: `daemon_base_url()` is called from sync contexts (e.g. main.rs CLI
/// dispatch) and cannot easily `.await`. A blocking `TcpStream::connect_timeout`
/// is the simplest correct primitive -- 200 ms is well below the perceptual
/// threshold for CLI startup.
/// What: parses `host:port`, attempts a TCP connect with a 200 ms deadline,
/// returns true on success. Any parse or connect error returns false.
/// Test: `address_reachable_returns_false_for_dead_port` unit test below.
fn address_reachable_blocking(host_port: &str) -> bool {
    use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
    let Ok(mut iter) = host_port.to_socket_addrs() else {
        return false;
    };
    let Some(addr): Option<SocketAddr> = iter.next() else {
        return false;
    };
    TcpStream::connect_timeout(&addr, Duration::from_millis(200)).is_ok()
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
/// otherwise `<data_local_dir>/trusty-search/daemon.port`.
/// Test: set `TRUSTY_DATA_DIR=/tmp/ts-x`; assert path equals
/// `/tmp/ts-x/daemon.port`.
pub fn daemon_port_path() -> Option<std::path::PathBuf> {
    if let Ok(dir) = std::env::var("TRUSTY_DATA_DIR") {
        return Some(std::path::PathBuf::from(dir).join("daemon.port"));
    }
    dirs::data_local_dir().map(|d| d.join("trusty-search").join("daemon.port"))
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

    #[test]
    fn address_reachable_returns_false_for_dead_port() {
        // Why: regression coverage for issue #117 -- `daemon_base_url()` must
        // detect a dead address read from the discovery file so it can fall back
        // to the port file instead of returning a URL nobody can connect to.
        // What: port 1 is reserved and unbound on every developer machine.
        // Test: the probe returns false in well under the 200 ms deadline.
        let start = std::time::Instant::now();
        assert!(!address_reachable_blocking("127.0.0.1:1"));
        assert!(
            start.elapsed() < Duration::from_millis(1500),
            "probe took too long: {:?}",
            start.elapsed()
        );
    }

    #[test]
    fn address_reachable_returns_false_for_garbage_input() {
        // Why: defence-in-depth -- a corrupted discovery file (zero bytes,
        // partial write, hand-edited typo) must not panic the resolver.
        assert!(!address_reachable_blocking("not-a-host:port"));
        assert!(!address_reachable_blocking(""));
        assert!(!address_reachable_blocking("127.0.0.1"));
    }

    #[test]
    fn address_reachable_returns_true_for_live_listener() {
        // Why: positive control -- a real bound port must register as reachable
        // so we don't fall back unnecessarily.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        assert!(address_reachable_blocking(&addr.to_string()));
    }

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
