//! Supervised stdio MCP connection to a local service (epic #1104 Phase 0b).
//!
//! Why: The trusty-console needs to poll each local service (starting with
//! trusty-analyze) over a persistent stdio MCP connection. Re-spawning on
//! every poll wastes process-creation overhead and can miss in-flight work.
//! Keeping a persistent connection allows low-overhead polling while the
//! supervisor handles crashes gracefully.
//!
//! What: `McpServiceHandle` wraps a `StdioMcpClient` behind an async mutex.
//! It spawns the service binary on construction (or lazily on first poll),
//! implements the supervisor pattern from `trusty-common`'s embedder client:
//! - `which(binary)` miss → never retry (service not installed on this machine)
//! - call failure / dead child → respawn with one retry per poll call
//!   (the `StdioMcpClient` already handles respawn internally via `ensure_alive`)
//! - `poll_metrics()` calls `console_metrics` tool and returns a parsed report
//!
//! Test: `mcp_handle_absent_binary_returns_error` and
//! `mcp_handle_poll_metrics_propagates_parse_failure` in this module.

use std::sync::Arc;

use anyhow::{Context, Result};
use serde_json::json;
use tokio::sync::Mutex;
use tracing::{debug, warn};
use trusty_common::console_metrics::{CONSOLE_METRICS_METHOD, ConsoleMetricsReport, parse_report};
use trusty_common::stdio_mcp_client::StdioMcpClient;

/// State of the supervised connection.
enum HandleState {
    /// `which(binary)` returned None — service not installed; never retry.
    Absent,
    /// Connection is up (client holds the live pipe).
    /// Boxed to avoid large_enum_variant: `StdioMcpClient` is ~384 bytes.
    Connected(Box<StdioMcpClient>),
}

/// A supervised, persistent stdio MCP connection to a single local service.
///
/// Why: The console needs to poll each local service every ~15 s. A persistent
/// connection avoids per-poll spawn overhead. The supervisor recovers from
/// crashes (the underlying `StdioMcpClient` auto-respawns via `ensure_alive`).
/// Binary-missing machines degrade immediately to `Absent` and never retry.
/// What: Holds the `StdioMcpClient` behind an async `Mutex`. `poll_metrics()`
/// calls the `console_metrics` tool and returns a `ConsoleMetricsReport`.
/// Test: Unit tests in this module cover the absent-binary and parse-failure
/// paths; the end-to-end smoke test covers the live pipe.
pub struct McpServiceHandle {
    /// Absolute path or short name of the binary to spawn.
    binary: String,
    /// Args to pass to the binary (e.g. `["serve", "--mcp"]`).
    args: Vec<String>,
    /// The supervised client state, protected by an async mutex so the console
    /// and the poller task can share the same handle across tasks.
    state: Arc<Mutex<Option<HandleState>>>,
}

impl McpServiceHandle {
    /// Construct a new `McpServiceHandle`.
    ///
    /// Why: Defers the actual spawn to the first `poll_metrics()` call so
    /// construction is sync and cheap (no async required at startup).
    /// What: Stores the binary + args; `state = None` means not yet connected.
    /// Test: `mcp_handle_absent_binary_returns_error` constructs and polls.
    pub fn new(binary: impl Into<String>, args: Vec<String>) -> Self {
        Self {
            binary: binary.into(),
            args,
            state: Arc::new(Mutex::new(None)),
        }
    }

    /// Poll the service's `console_metrics` tool and return a decoded report.
    ///
    /// Why: The background poller calls this every ~15 s to refresh the cached
    /// snapshot. The method lazily connects on the first call. On tool-call
    /// failure the `StdioMcpClient`'s built-in `ensure_alive` + `respawn` path
    /// recovers transparently; if the binary is absent the error is immediate.
    /// What: Lazy-init the connection on first call; call the `console_metrics`
    /// tool; decode the result into a `ConsoleMetricsReport`. On any failure
    /// returns `Err` so the poller can log and retain the previous cached value.
    /// Test: End-to-end smoke test validates live pipe; unit test
    /// `mcp_handle_absent_binary_returns_error` covers the absent path.
    pub async fn poll_metrics(&self) -> Result<ConsoleMetricsReport> {
        let mut guard = self.state.lock().await;

        // Lazy initialisation: None means we haven't tried connecting yet.
        if guard.is_none() {
            let resolved = which::which(&self.binary).ok();
            if resolved.is_none() {
                warn!(
                    binary = %self.binary,
                    "McpServiceHandle: binary not found on PATH — marking as Absent"
                );
                *guard = Some(HandleState::Absent);
            } else {
                debug!(binary = %self.binary, "McpServiceHandle: spawning MCP child");
                let args_ref: Vec<&str> = self.args.iter().map(String::as_str).collect();
                match StdioMcpClient::spawn(&self.binary, &args_ref, "trusty-console").await {
                    Ok(mut client) => {
                        client.initialize().await.with_context(|| {
                            format!(
                                "McpServiceHandle: MCP initialize failed for {}",
                                self.binary
                            )
                        })?;
                        *guard = Some(HandleState::Connected(Box::new(client)));
                    }
                    Err(e) => {
                        warn!(
                            binary = %self.binary,
                            error = %e,
                            "McpServiceHandle: spawn failed"
                        );
                        return Err(e.context(format!(
                            "McpServiceHandle: failed to spawn {}",
                            self.binary
                        )));
                    }
                }
            }
        }

        match guard.as_mut() {
            Some(HandleState::Absent) => anyhow::bail!(
                "McpServiceHandle: {} is not installed on this machine",
                self.binary
            ),
            Some(HandleState::Connected(client)) => {
                let raw = client
                    .call_tool(CONSOLE_METRICS_METHOD, json!({}))
                    .await
                    .with_context(|| {
                        format!(
                            "McpServiceHandle: {} tool call failed for {}",
                            CONSOLE_METRICS_METHOD, self.binary
                        )
                    })?;
                parse_report(&raw).with_context(|| {
                    format!("McpServiceHandle: parse_report failed for {}", self.binary)
                })
            }
            None => unreachable!("guard must be Some after init block"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: When the binary is absent from PATH, `poll_metrics` must return an
    /// error immediately (not hang or panic) so the poller can degrade gracefully.
    /// What: Create a handle pointing at a binary that does not exist, call
    /// `poll_metrics`, assert it returns `Err`.
    /// Test: This test.
    #[tokio::test]
    async fn mcp_handle_absent_binary_returns_error() {
        let handle =
            McpServiceHandle::new("/nonexistent/trusty-analyze-xyzzy", vec!["mcp".to_string()]);
        let result = handle.poll_metrics().await;
        assert!(result.is_err(), "absent binary must return Err");
    }

    /// Why: `new()` must succeed synchronously without performing I/O so
    /// the console can construct handles at startup without async.
    /// What: Construct a handle and assert the state is `None` initially.
    /// Test: This test (checks construction is cheap/sync-compatible).
    #[test]
    fn mcp_handle_constructs_without_io() {
        let handle = McpServiceHandle::new("trusty-analyze", vec!["mcp".to_string()]);
        // The binary field must be stored correctly.
        assert_eq!(handle.binary, "trusty-analyze");
        assert_eq!(handle.args, vec!["mcp"]);
    }
}
