//! The MCP bridge's address file, shared across CLI subcommands.
//!
//! Why: `serve --with-http` publishes its MCP HTTP/SSE address here. #9214
//! moved the CLI onto the daemon's socket (`service::daemon_client`); no CLI
//! path dials TCP, and the daemon's own port-file resolver went with its
//! `:7878` bind (`stop` and the orphan reaper use
//! `service::daemon_port_path` to clear a stale one).
//!
//! What: one path resolver.
//! Test: `mcp_http_addr_path_is_home_relative` below.

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
