//! Handler for `trusty-search serve` — MCP server (stdio + optional HTTP/SSE).

use super::daemon_utils::{daemon_base_url, mcp_http_addr_path};
use anyhow::Result;
use colored::Colorize;

/// Why: extracted from `main()`. The HTTP path involves a discovery file
/// (`~/.trusty-search/mcp_http_addr`) and cleanup-on-exit logic that's easier
/// to follow in isolation.
/// What: routes between stdio-only (the default — issue #123) and HTTP modes;
/// HTTP is opt-in via `--with-http` (or the legacy explicit `--http <addr>`).
/// In stdio mode, exits the process immediately when the MCP client closes its
/// pipe (stdin EOF), so the process never lingers as an orphan after Claude
/// Code's session ends (issue #457).
/// Test: `cargo run -- serve` runs MCP over stdio only; `serve --with-http`
/// additionally binds HTTP and the discovery file appears at
/// `~/.trusty-search/mcp_http_addr` then is removed on shutdown. Note: the
/// MCP SSE listener writes its address to `mcp_http_addr` (distinct from the
/// daemon's `http_addr` file) so a crashed `serve` cannot clobble the daemon's
/// discovery file (issue #117). EOF self-exit is unit-tested in
/// `crates/trusty-common/src/mcp/mod.rs` (`stdio_loop_exits_on_eof`).
pub async fn handle_serve(with_http: bool, port: u16, http: Option<String>) -> Result<()> {
    let daemon_url = daemon_base_url();

    // Resolve the HTTP bind address. HTTP is OFF by default (issue #123) —
    // Claude Code MCP hooks only need stdio. Precedence:
    //   1. legacy `--http <addr>`   → explicit bind (implies HTTP on)
    //   2. `--with-http`            → 127.0.0.1:port (port 0 → OS picks)
    //   3. neither                  → stdio only
    let bind_addr: Option<String> = if let Some(addr) = http {
        Some(addr)
    } else if with_http {
        Some(format!("127.0.0.1:{port}"))
    } else {
        None
    };

    let server = crate::mcp::McpServer::new(daemon_url.clone());

    match bind_addr {
        Some(addr) => serve_http(server, addr, &daemon_url).await,
        None => {
            eprintln!(
                "{} MCP stdio (no HTTP) → daemon {}",
                "◉".green(),
                daemon_url.dimmed()
            );
            crate::mcp::stdio::run(server).await?;
            // Why: the reqwest connection pool and tokio background threads can
            // keep the runtime alive for up to 90 s after the stdio loop exits
            // (reqwest's default pool_idle_timeout). In MCP stdio mode the
            // client has already disconnected (stdin hit EOF), so lingering is
            // never useful — the process is an orphan at this point. Calling
            // exit(0) immediately tears it down so workers never accumulate
            // across Claude Code session restarts (issue #457). HTTP serve mode
            // does NOT call exit here; it has an explicit cleanup path and the
            // axum serve loop is the natural lifetime anchor.
            std::process::exit(0);
        }
    }
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
            eprintln!("{} could not write {}: {e}", "⚠".yellow(), path.display());
        }
    }

    eprintln!(
        "trusty-search v{} — HTTP admin panel: http://{}",
        env!("CARGO_PKG_VERSION"),
        local,
    );
    eprintln!(
        "{} MCP HTTP/SSE on {} → daemon {}",
        "◉".green(),
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
