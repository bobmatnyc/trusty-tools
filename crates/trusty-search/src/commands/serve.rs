//! Handler for `trusty-search serve` -- the MCP server, over stdio only.
//!
//! #9168: the bridge reaches the daemon only through `DaemonClient` on its
//! Unix socket. #9542: the `--with-http` MCP HTTP/SSE listener is retired;
//! MCP is stdio only; the daemon's Unix socket is the one other client path.

use super::serve_scope::{auto_pin_from_cwd, PinChoice};
use anyhow::Result;
use colored::Colorize;
use trusty_search::service::daemon_client::DaemonClient;

pub(crate) use super::serve_scope::resolve_pinned_index;

/// Refuse the retired MCP HTTP/SSE flags before anything is resolved or bound.
///
/// Why (#9542, ADR-0032): no trusty-search process listens on TCP. A retired
/// flag that was silently ignored would leave an MCP client pointed at an
/// address nothing serves, so each one fails closed and names the issue.
/// What: `Err` naming the first retired flag present (`--with-http`, `--http`,
/// `--port`) and the stdio and socket alternatives; `Ok` when none is.
/// Test: `serve_binds_no_tcp_listener_9542`.
fn refuse_retired_http(with_http: bool, port: Option<u16>, http: Option<&str>) -> Result<()> {
    let flag = if with_http {
        "--with-http"
    } else if http.is_some() {
        "--http"
    } else if port.is_some() {
        "--port"
    } else {
        return Ok(());
    };
    anyhow::bail!(
        "`serve {flag}` is retired (#9542): trusty-search binds no TCP listener (ADR-0032). \
         Run `trusty-search serve` with no flags for MCP over stdio, or talk to the \
         daemon on its Unix socket (`<TRUSTY_DATA_DIR>/trusty-search.sock`, or \
         `TRUSTY_SEARCH_SOCKET`). trusty-console has no MCP endpoint."
    )
}

/// Why: extracted from `main()`; the stdio MCP session has its own daemon
/// guard and working-directory scope.
/// What: refuses the retired HTTP flags (#9542), then ensures the daemon
/// answers on its socket (auto-starting it if absent via
/// `daemon_guard::ensure_daemon_up`, which waits on the socket, never on an
/// HTTP address — #9168) before entering the MCP stdio loop; exits the process
/// immediately when the MCP client closes its pipe (stdin EOF), so the process
/// never lingers as an orphan after Claude Code's session ends (issue #457).
/// Test: `serve_binds_no_tcp_listener_9542` for the refusal. EOF self-exit is
/// unit-tested in `crates/trusty-common/src/mcp/mod.rs`
/// (`stdio_loop_exits_on_eof`). Socket wait covered by
/// `ensure_daemon_up_names_the_socket_when_it_never_answers`.
pub async fn handle_serve(
    with_http: bool,
    port: Option<u16>,
    http: Option<String>,
    pinned_index: Option<PinChoice>,
) -> Result<()> {
    // #9542: fail closed before the socket is resolved; nothing is bound.
    refuse_retired_http(with_http, port, http.as_deref())?;

    // Apply the optional index pin (#1373) to the dispatcher so omitted
    // `index_id`s default to it and fan-out tools scope to it.
    let pin = |server: crate::mcp::McpServer, choice: Option<&PinChoice>| match choice {
        Some(c) => server.with_pinned_index(c.index_id.clone()),
        None => server,
    };

    // #9168: the one route to the daemon — its socket, resolved the way the
    // daemon derives it (`TRUSTY_DATA_DIR`, or `TRUSTY_SEARCH_SOCKET`).
    let daemon = DaemonClient::resolve()?;

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
    // across Claude Code session restarts (issue #457).
    std::process::exit(0);
}

#[cfg(test)]
#[path = "serve_index_env_tests.rs"]
mod index_env_tests;

#[cfg(test)]
#[path = "serve_scope_tests.rs"]
mod scope_tests;
