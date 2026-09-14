//! Spawning one configured MCP server, in isolation (#5428).
//!
//! Why: a task run may have several connectors configured and exactly one of
//! them broken — a typo'd command, a server that hangs on `initialize`, a
//! binary that exits immediately. None of that may abort the run or the other
//! servers, so every connection attempt is independently bounded and every
//! failure is a value, never an error that propagates.
//! What: [`connect`] spawns the child through the SHARED
//! `trusty_common::stdio_mcp_client::StdioMcpClient` (never a bespoke
//! transport), runs the handshake and `tools/list` under one
//! [`HANDSHAKE_TIMEOUT`], and returns either the live client plus its tools or
//! a one-line reason. A remote transport is refused here with the same shape,
//! because "slice 1 does not speak HTTP" is a status, not a fault.
//! Test: `super::tests::spawn_tests`, `mcp_loader_e2e::spawn_failure_isolates_the_healthy_server`.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Mutex;
use tokio::time::timeout;
use trusty_common::stdio_mcp_client::{McpTool, StdioMcpClient};
use trusty_mcp::config::{McpServerConfig, McpTransport};

/// Wall-clock bound on one server's whole startup: handshake plus `tools/list`.
///
/// Why: `StdioMcpClient` bounds each REQUEST at 30s, which is far too long to
/// wait at task startup and, with two requests, twice as long again. A server
/// that cannot introduce itself in ten seconds is not one this run should
/// block on.
/// Test: `super::tests::spawn_tests::handshake_timeout_is_ten_seconds`.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// `clientInfo.name` advertised during the MCP handshake.
const CLIENT_NAME: &str = "trusty-code";

/// A live server: its client handle and the tools it advertised.
///
/// What: the client is already behind the `Arc<Mutex<_>>` every
/// [`super::tool::McpBridgeTool`] for this server shares, because that shared
/// handle is also what keeps the child process alive.
pub struct Connected {
    /// The shared, live client.
    pub client: Arc<Mutex<StdioMcpClient>>,
    /// Everything `tools/list` advertised.
    pub tools: Vec<McpTool>,
}

/// Spawn one server and discover its tools, or say why not.
///
/// Why: the isolation boundary. Every failure mode — an unsupported transport,
/// a command that does not exist, a handshake that times out, a server that
/// will not list its tools — comes back as `Err(reason)` for the caller to
/// record as a per-server status, so one bad entry costs exactly itself.
/// What: stdio only. `env` is overlaid on the inherited environment through
/// `spawn_with_env`; its VALUES are never logged, here or in the returned
/// reason, because they are credentials.
/// Test: `super::tests::spawn_tests::a_remote_transport_is_refused_with_a_reason`,
/// `super::tests::spawn_tests::a_missing_binary_is_refused_with_a_reason`,
/// `mcp_loader_e2e::spawn_failure_isolates_the_healthy_server`.
pub async fn connect(server: &McpServerConfig) -> Result<Connected, String> {
    let McpTransport::Stdio { command, args, env } = &server.transport else {
        return Err(super::REMOTE_UNSUPPORTED.to_string());
    };

    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let envs: HashMap<String, String> = env.clone().into_iter().collect();
    let mut client = StdioMcpClient::spawn_with_env(command, &arg_refs, CLIENT_NAME, &envs)
        .await
        .map_err(|e| format!("could not start {command:?}: {e}"))?;

    let tools = match timeout(HANDSHAKE_TIMEOUT, handshake(&mut client)).await {
        Ok(Ok(tools)) => tools,
        Ok(Err(reason)) => return Err(reason),
        // Dropping `client` here kills the child (`kill_on_drop` plus its own
        // `Drop`), so a hung server does not outlive the attempt to use it.
        Err(_) => {
            return Err(format!(
                "did not finish the MCP handshake within {}s",
                HANDSHAKE_TIMEOUT.as_secs()
            ));
        }
    };

    tracing::info!(
        server = %server.name,
        command = %command,
        tools = tools.len(),
        "mcp: server connected",
    );
    Ok(Connected {
        client: Arc::new(Mutex::new(client)),
        tools,
    })
}

/// `initialize` then `tools/list`, as one bounded unit.
///
/// Why: both are startup, and a server that handshakes but will not list its
/// tools contributes nothing — so one timeout covers the pair rather than
/// letting a slow server spend the budget twice.
/// Test: covered through [`connect`].
async fn handshake(client: &mut StdioMcpClient) -> Result<Vec<McpTool>, String> {
    client
        .initialize()
        .await
        .map_err(|e| format!("the MCP handshake failed: {e}"))?;
    client
        .list_tools()
        .await
        .map_err(|e| format!("tools/list failed: {e}"))
}
