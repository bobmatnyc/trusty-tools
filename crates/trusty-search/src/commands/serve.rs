//! Handler for `trusty-search serve` -- MCP server (stdio + optional HTTP/SSE).
//!
//! #9168: the bridge reaches the daemon only through `DaemonClient` on its
//! Unix socket. The optional `--with-http` listener is the MCP transport's own
//! endpoint for MCP clients, not a route to the daemon.

use super::daemon_utils::mcp_http_addr_path;
use super::serve_scope::{auto_pin_from_cwd, PinChoice};
use anyhow::Result;
use colored::Colorize;
use trusty_search::service::daemon_client::DaemonClient;

pub(crate) use super::serve_scope::resolve_pinned_index;

/// Why: extracted from `main()`. The HTTP path involves a discovery file
/// (`~/.trusty-search/mcp_http_addr`) and cleanup-on-exit logic that's easier
/// to follow in isolation.
/// What: routes between stdio-only (the default -- issue #123) and HTTP modes;
/// HTTP is opt-in via `--with-http` (or the legacy explicit `--http <addr>`).
/// In stdio mode, ensures the daemon answers on its socket (auto-starting it
/// if absent via `daemon_guard::ensure_daemon_up`, which waits on the socket,
/// never on an HTTP address — #9168) before entering the MCP stdio loop; exits the process immediately when the MCP client closes
/// its pipe (stdin EOF), so the process never lingers as an orphan after Claude
/// Code's session ends (issue #457).
/// Test: `cargo run -- serve` runs MCP over stdio only; `serve --with-http`
/// additionally binds HTTP and the discovery file appears at
/// `~/.trusty-search/mcp_http_addr` then is removed on shutdown. Note: the
/// MCP SSE listener writes its address to `mcp_http_addr` (distinct from the
/// daemon's `http_addr` file) so a crashed `serve` cannot clobber the daemon's
/// discovery file (issue #117). EOF self-exit is unit-tested in
/// `crates/trusty-common/src/mcp/mod.rs` (`stdio_loop_exits_on_eof`).
/// Socket wait covered by `ensure_daemon_up_names_the_socket_when_it_never_answers`.
pub async fn handle_serve(
    with_http: bool,
    port: u16,
    http: Option<String>,
    pinned_index: Option<PinChoice>,
) -> Result<()> {
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

    // Apply the optional index pin (#1373) to the dispatcher so omitted
    // `index_id`s default to it and fan-out tools scope to it.
    let pin = |server: crate::mcp::McpServer, choice: Option<&PinChoice>| match choice {
        Some(c) => server.with_pinned_index(c.index_id.clone()),
        None => server,
    };

    // #9168: the one route to the daemon — its socket, resolved the way the
    // daemon derives it (`TRUSTY_DATA_DIR`, or `TRUSTY_SEARCH_SOCKET`).
    let daemon = DaemonClient::resolve()?;

    match bind_addr {
        Some(addr) => {
            // #5264: the working-directory tier is deliberately stdio-only. An
            // HTTP listener is a shared, multi-client endpoint; deriving its
            // scope from whichever directory happened to launch it would apply
            // one client's project to every other client.
            let server = pin(
                crate::mcp::McpServer::new(daemon.clone()),
                pinned_index.as_ref(),
            );
            if let Some(ref choice) = pinned_index {
                eprintln!("{} {}", "\u{25c9}".green(), choice.report());
            }
            serve_http(server, addr, &daemon).await
        }
        None => {
            // Stdio mode: ensure the daemon answers on its socket before
            // entering the MCP dispatch loop. Every tool call goes over that
            // socket, so the daemon MUST be reachable there.
            super::daemon_guard::ensure_daemon_up(&daemon).await?;

            // #5264: with no explicit flag, scope the session to the working
            // directory — confirmed against the daemon first, so an unindexed
            // or basename-colliding directory declines to pin rather than
            // silently serving another project. Resolved once: `serve` is a
            // long-lived stdio process whose cwd cannot change after exec.
            let resolved = match pinned_index {
                Some(choice) => Some(super::serve_scope::AutoPin::Pinned(choice)),
                None => match std::env::current_dir() {
                    Ok(cwd) => auto_pin_from_cwd(&daemon, &cwd).await,
                    Err(e) => Some(super::serve_scope::AutoPin::Unpinned {
                        reason: format!(
                            "MCP session UNPINNED — could not read the working directory \
                             ({e}). Pass index_id explicitly."
                        ),
                    }),
                },
            };

            let server = pin(
                crate::mcp::McpServer::new(daemon.clone()),
                resolved.as_ref().and_then(|r| r.choice()),
            );
            if let Some(ref r) = resolved {
                let marker = if r.choice().is_some() {
                    "\u{25c9}".green()
                } else {
                    "\u{26a0}".yellow()
                };
                eprintln!("{} {}", marker, r.report());
            }
            eprintln!(
                "{} MCP stdio -> daemon socket {}",
                "\u{25c9}".green(),
                daemon.socket().display().to_string().dimmed()
            );
            crate::mcp::stdio::run(server).await?;
            // Why: tokio background threads can keep the runtime alive after
            // the stdio loop exits. In MCP stdio mode the
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

/// Run the MCP HTTP/SSE listener on `addr`. Writes the discovery file before
/// serving and removes it on exit (clean or crashed).
async fn serve_http(
    server: crate::mcp::McpServer,
    addr: String,
    daemon: &DaemonClient,
) -> Result<()> {
    // Bind first so we can report the OS-chosen port when 0.
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    let local = listener.local_addr()?;

    // Write `~/.trusty-search/mcp_http_addr` so MCP HTTP/SSE clients can find
    // this MCP server's transport. Distinct from the daemon's `http_addr` file
    // (issue #117): two processes writing the same file caused stale-address
    // races where a SIGKILL'd `serve --http` would leave a dead address that
    // the daemon-address resolver read first, then waited 60s for. Best-effort:
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

    // #9168: this listener is the MCP transport's own endpoint; the daemon is
    // reached through its socket, named here.
    eprintln!(
        "{} trusty-search v{} MCP HTTP/SSE on {} -> daemon socket {}",
        "\u{25c9}".green(),
        env!("CARGO_PKG_VERSION"),
        local.to_string().cyan(),
        daemon.socket().display().to_string().dimmed()
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

#[cfg(test)]
#[path = "serve_index_env_tests.rs"]
mod index_env_tests;

#[cfg(test)]
#[path = "serve_scope_tests.rs"]
mod scope_tests;
