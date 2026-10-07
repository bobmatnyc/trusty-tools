//! Daemon discovery + reachability helpers shared across CLI subcommands.
//!
//! Why: every subcommand that talks to the running daemon needs the same
//! "where is it?" logic. #5670 promoted the address-file layout to
//! `trusty_common::daemon_guard::DaemonAddrLayout`, because `tga` has to probe
//! this daemon and cannot depend on this crate. #9214 moved the CLI onto the
//! daemon's socket (`service::daemon_client`); the HTTP base resolver the
//! remaining HTTP subcommands use now lives in `commands::daemon_http`, which
//! fails closed rather than guessing a port.
//!
//! What: two path resolvers and one async TCP probe.
//! Test: `mcp_http_addr_path_is_home_relative` below.

use std::time::Duration;

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
}
