//! Handler for `trusty-search serve` -- MCP server (stdio + optional HTTP/SSE).

use super::daemon_utils::{daemon_base_url, mcp_http_addr_path};
use anyhow::Result;
use colored::Colorize;
use trusty_common::mcp::DaemonBridgeConfig;

/// Why: extracted from `main()`. The HTTP path involves a discovery file
/// (`~/.trusty-search/mcp_http_addr`) and cleanup-on-exit logic that's easier
/// to follow in isolation.
/// What: routes between stdio-only (the default -- issue #123) and HTTP modes;
/// HTTP is opt-in via `--with-http` (or the legacy explicit `--http <addr>`).
/// In stdio mode, ensures the daemon is running (auto-starting it if absent via
/// the shared `trusty_common::mcp::ensure_daemon_up` helper) before entering
/// the MCP stdio loop; exits the process immediately when the MCP client closes
/// its pipe (stdin EOF), so the process never lingers as an orphan after Claude
/// Code's session ends (issue #457).
/// Test: `cargo run -- serve` runs MCP over stdio only; `serve --with-http`
/// additionally binds HTTP and the discovery file appears at
/// `~/.trusty-search/mcp_http_addr` then is removed on shutdown. Note: the
/// MCP SSE listener writes its address to `mcp_http_addr` (distinct from the
/// daemon's `http_addr` file) so a crashed `serve` cannot clobber the daemon's
/// discovery file (issue #117). EOF self-exit is unit-tested in
/// `crates/trusty-common/src/mcp/mod.rs` (`stdio_loop_exits_on_eof`).
/// Auto-start behavior covered by `trusty_common::mcp::daemon_bridge` tests.
pub async fn handle_serve(with_http: bool, port: u16, http: Option<String>) -> Result<()> {
    // Resolve the HTTP bind address. HTTP is OFF by default (issue #123) --
    // Claude Code MCP hooks only need stdio. Precedence:
    //   1. legacy `--http <addr>`   -> explicit bind (implies HTTP on)
    //   2. `--with-http`            -> 127.0.0.1:port (port 0 -> OS picks)
    //   3. neither                  -> stdio only
    let bind_addr: Option<String> = if let Some(addr) = http {
        Some(addr)
    } else if with_http {
        Some(format!("127.0.0.1:{port}"))
    } else {
        None
    };

    match bind_addr {
        Some(addr) => {
            let daemon_url = daemon_base_url();
            let server = crate::mcp::McpServer::new(daemon_url.clone());
            serve_http(server, addr, &daemon_url).await
        }
        None => {
            // Stdio mode: ensure the daemon is running before entering the
            // MCP dispatch loop. The McpServer forwards every tool call to
            // the daemon's REST API, so the daemon MUST be reachable.
            let base_url = ensure_search_daemon_up().await?;
            let server = crate::mcp::McpServer::new(base_url.clone());
            eprintln!(
                "{} MCP stdio (no HTTP) -> daemon {}",
                "\u{25c9}".green(),
                base_url.dimmed()
            );
            crate::mcp::stdio::run(server).await?;
            // Why: the reqwest connection pool and tokio background threads can
            // keep the runtime alive for up to 90 s after the stdio loop exits
            // (reqwest's default pool_idle_timeout). In MCP stdio mode the
            // client has already disconnected (stdin hit EOF), so lingering is
            // never useful -- the process is an orphan at this point. Calling
            // exit(0) immediately tears it down so workers never accumulate
            // across Claude Code session restarts (issue #457). HTTP serve mode
            // does NOT call exit here; it has an explicit cleanup path and the
            // axum serve loop is the natural lifetime anchor.
            std::process::exit(0);
        }
    }
}

/// Ensure the trusty-search daemon is running; return its live base URL.
///
/// Why: the MCP stdio bridge forwards every tool call to the daemon's REST API.
/// If the daemon is not running the bridge would emit connection errors on every
/// tool call, which is confusing. Auto-starting matches the UX of the memory
/// bridge (issue #1078) and aligns all three daemon-backed MCP servers.
/// What: uses the shared `trusty_common::mcp::ensure_daemon_up` helper with the
/// trusty-search-specific config: health path `/health`, spawn args
/// `start --foreground` (which binds a fixed port written to the discovery
/// file), and a `base_url_fn` that re-reads the address file on every poll.
/// Test: covered by `trusty_common::mcp::daemon_bridge` unit tests; the live
/// path is exercised by `cargo run -- serve` with no daemon running.
async fn ensure_search_daemon_up() -> Result<String> {
    let config = DaemonBridgeConfig {
        service_name: "trusty-search".to_string(),
        // `start --foreground` launches the HTTP daemon inline (blocking) in
        // the spawned child. The daemon writes its bound `host:port` to
        // `{data_dir}/http_addr`; our `base_url_fn` reads that file on each
        // iteration to discover the live address.
        spawn_args: vec!["start".to_string(), "--foreground".to_string()],
        health_path: "/health".to_string(),
        base_url_fn: Box::new(|| match trusty_common::read_daemon_addr("trusty-search") {
            Ok(Some(addr)) if !addr.is_empty() => {
                if addr.starts_with("http://") || addr.starts_with("https://") {
                    addr
                } else {
                    format!("http://{addr}")
                }
            }
            _ => daemon_base_url(),
        }),
        startup_timeout: None,
        poll_interval: None,
    };
    trusty_common::mcp::ensure_daemon_up(&config).await
}

/// Run the MCP HTTP/SSE listener on `addr`. Writes the discovery file before
/// serving and removes it on exit (clean or crashed).
async fn serve_http(server: crate::mcp::McpServer, addr: String, daemon_url: &str) -> Result<()> {
    // Bind first so we can report the OS-chosen port when 0.
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    let local = listener.local_addr()?;

    // Write `~/.trusty-search/mcp_http_addr` so MCP HTTP/SSE clients can find
    // this MCP server's transport. Distinct from the daemon's `http_addr` file
    // (issue #117): two processes writing the same file caused stale-address
    // races where a SIGKILL'd `serve --http` would leave a dead address that
    // `daemon_base_url()` reads first, then waits 60s for. Best-effort:
    // a missing $HOME is reported but doesn't abort.
    let addr_file = mcp_http_addr_path();
    if let Some(ref path) = addr_file {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(e) = std::fs::write(path, format!("{local}\n")) {
            eprintln!(
                "{} could not write {}: {e}",
                "\u{26a0}".yellow(),
                path.display()
            );
        }
    }

    eprintln!(
        "trusty-search v{} -- HTTP admin panel: http://{}",
        env!("CARGO_PKG_VERSION"),
        local,
    );
    eprintln!(
        "{} MCP HTTP/SSE on {} -> daemon {}",
        "\u{25c9}".green(),
        local.to_string().cyan(),
        daemon_url.dimmed()
    );

    let app = crate::mcp::sse::router(server);
    let serve_result = axum::serve(listener, app).await;

    // Clean up the discovery file regardless of the serve outcome so a
    // crashed `serve` doesn't leave a stale pointer.
    if let Some(path) = addr_file {
        let _ = std::fs::remove_file(&path);
    }
    serve_result?;
    Ok(())
}
