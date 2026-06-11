//! The `impl StdioMcpClient` request methods (spawn + JSON-RPC round-trips).
//!
//! Why: The spawn flow and per-method JSON-RPC request/response handling are
//! the bulk of the client; isolating them from the struct/types/Drop in
//! `mod.rs` keeps both files under the 500-line cap.
//! What: `StdioMcpClient::spawn`, `initialize`, `list_tools`, `call_tool`,
//! `ping`, `is_alive`, `respawn`, `ensure_alive` — and the low-level
//! `send`/`recv` helpers — dispatching through `build_initialize_request` /
//! `extract_result` defined in `mod.rs`.
//! Test: JSON-RPC framing is unit-tested in `stdio_mcp_client::tests`; the
//! full spawn flow has an `#[ignore]`d integration test.

use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter};
use tokio::process::Command;
use tokio::time::timeout;
use tracing::{debug, warn};

use super::{
    CALL_TIMEOUT, MCP_PROTOCOL_VERSION, McpTool, ServerInfo, StdioMcpClient,
    build_initialize_request, extract_result, plugin_stderr_stdio,
};

impl StdioMcpClient {
    /// Spawn `binary` with `args`, piping stdin/stdout (stderr is redirected
    /// to a per-plugin log file so server logs don't pollute the parent's
    /// terminal or MCP stdout stream).
    ///
    /// Why: The MCP transport requires clean JSON on stdout, so plugin logs
    /// MUST go to stderr. Redirecting stderr to a named file keeps the parent
    /// console clean while still preserving logs for debugging. The
    /// `client_name` parameter is caller-supplied so each consumer
    /// (trusty-agents, trusty-console, etc.) advertises its own identity in
    /// `clientInfo.name` during the `initialize` handshake — hard-coding the
    /// library name here would mislead MCP server logs and any server-side
    /// logic keyed on that field.
    /// What: Returns an unconnected client with the handshake NOT yet sent.
    /// Call `initialize` next to complete the MCP handshake.
    /// Test: Indirectly via `#[ignore]`d e2e test; unit-test failure is
    /// covered by `spawn_missing_binary_errors` in `mod.rs`. The
    /// `initialize_envelope_is_well_formed` test verifies the supplied name
    /// propagates to `clientInfo.name`.
    pub async fn spawn(
        binary: &str,
        args: &[&str],
        client_name: impl Into<String>,
    ) -> Result<Self> {
        let mut child = Command::new(binary)
            .args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(plugin_stderr_stdio(binary))
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("failed to spawn MCP binary {binary}"))?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("child stdin not captured"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("child stdout not captured"))?;

        Ok(Self {
            child,
            stdin: BufWriter::new(stdin),
            stdout: BufReader::new(stdout),
            next_id: AtomicU64::new(1),
            binary: binary.to_string(),
            args: args.iter().map(|s| s.to_string()).collect(),
            client_name: client_name.into(),
        })
    }

    /// Check whether the child process is still running.
    ///
    /// Why: When the MCP child dies (crash, OOM, killed externally) and the
    /// caller writes to its stdin, the write blocks until the 30s timeout —
    /// causing 15-60s query latency. Probing `try_wait()` lets callers detect
    /// the dead child cheaply and respawn before writing. (See issue #421.)
    /// What: Returns `true` if `try_wait()` reports `Ok(None)` (still
    /// running); `false` if the process has exited or `try_wait()` errored.
    /// Test: `is_alive_returns_false_after_child_exits` verifies the false
    /// path against a child that exits immediately; `ids_are_monotonic`
    /// exercises the true path implicitly.
    pub fn is_alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// Replace the dead child with a freshly spawned one and rerun the MCP
    /// handshake. Used internally by `call_tool`/`list_tools`/`ping` when
    /// `is_alive()` returns false.
    ///
    /// Why: Avoids the 30s write-to-dead-stdin timeout (issue #421) by
    /// transparently recovering the connection. Without this, every query
    /// after a server crash stalls until timeout.
    /// What: Spawns the same `binary` + `args` used at construction, swaps
    /// in the new stdio handles, resets the request id counter, and runs
    /// `initialize`. Returns Err if respawn or handshake fails.
    /// Test: `call_tool_errors_when_respawn_unavailable` exercises the
    /// failure path (binary no longer present).
    async fn respawn(&mut self) -> Result<()> {
        let args_ref: Vec<&str> = self.args.iter().map(String::as_str).collect();
        let mut new_child = Command::new(&self.binary)
            .args(&args_ref)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(plugin_stderr_stdio(&self.binary))
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("failed to respawn MCP binary {}", self.binary))?;

        let stdin = new_child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("child stdin not captured on respawn"))?;
        let stdout = new_child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("child stdout not captured on respawn"))?;

        // Best-effort reap of the old child before replacing it.
        let _ = self.child.start_kill();

        self.child = new_child;
        self.stdin = BufWriter::new(stdin);
        self.stdout = BufReader::new(stdout);
        // client_name is retained from the original spawn — no update needed.
        self.next_id.store(1, Ordering::Relaxed);

        self.initialize()
            .await
            .context("MCP initialize failed after respawn")?;
        Ok(())
    }

    /// Ensure the child is alive, respawning it if necessary.
    ///
    /// Why: Centralised pre-flight for every write path so callers don't have
    /// to remember to probe before each request.
    /// What: If `is_alive()` returns false, logs a warning and calls
    /// `respawn()`. Returns Err if respawn fails.
    /// Test: Covered indirectly by `call_tool_errors_when_respawn_unavailable`.
    async fn ensure_alive(&mut self) -> Result<()> {
        if !self.is_alive() {
            warn!("StdioMcpClient: child process exited, attempting respawn");
            self.respawn().await?;
        }
        Ok(())
    }

    /// Allocate the next JSON-RPC request id. Monotonic, starts at 1.
    ///
    /// Why: JSON-RPC 2.0 requires each request to carry a unique id so
    /// responses can be correlated. The atomic counter provides this without
    /// locks.
    /// What: Atomically increments and returns the previous value using
    /// `Relaxed` ordering (ordering across threads is not required for id
    /// uniqueness within a single connection).
    /// Test: `ids_are_monotonic` verifies the strict ordering property.
    pub(super) fn alloc_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Send the MCP `initialize` request and follow up with the `initialized`
    /// notification per the protocol spec.
    ///
    /// Why: MCP requires this two-step handshake before any other method may
    /// be called. Doing it here means callers get a ready-to-use client.
    /// What: Sends `initialize` with our protocol version + minimal client
    /// info, parses `serverInfo`/`protocolVersion`, then sends the
    /// `initialized` notification (no response expected).
    /// Test: Envelope construction covered in `build_initialize_request`;
    /// e2e in the ignored integration test.
    pub async fn initialize(&mut self) -> Result<ServerInfo> {
        let id = self.alloc_id();
        let req = build_initialize_request(id, &self.client_name);
        let resp = self.request(&req).await?;
        let result = extract_result(resp)?;

        let server_info = result
            .get("serverInfo")
            .ok_or_else(|| anyhow!("initialize response missing serverInfo"))?;
        let name = server_info
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let version = server_info
            .get("version")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let protocol_version = result
            .get("protocolVersion")
            .and_then(|v| v.as_str())
            .unwrap_or(MCP_PROTOCOL_VERSION)
            .to_string();

        // Send `initialized` notification (no id, no response).
        let notif = json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
        });
        self.write_line(&notif).await?;

        Ok(ServerInfo {
            name,
            version,
            protocol_version,
        })
    }

    /// Call `tools/list` and return the advertised tool descriptors.
    ///
    /// Why: The console poller must enumerate available tools to verify the
    /// service exposes the expected metrics tool before polling.
    /// What: Sends `tools/list`, parses the `tools` array, and returns a
    /// `Vec<McpTool>`. Calls `ensure_alive` first to auto-respawn dead children.
    /// Test: End-to-end in trusty-agents integration tests.
    pub async fn list_tools(&mut self) -> Result<Vec<McpTool>> {
        self.ensure_alive().await?;
        let id = self.alloc_id();
        let req = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/list",
        });
        let resp = self.request(&req).await?;
        let result = extract_result(resp)?;
        let tools = result
            .get("tools")
            .and_then(|v| v.as_array())
            .ok_or_else(|| anyhow!("tools/list response missing tools array"))?;

        let mut out = Vec::with_capacity(tools.len());
        for t in tools {
            let name = t
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("tool entry missing name"))?
                .to_string();
            let description = t
                .get("description")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let input_schema = t.get("inputSchema").cloned().unwrap_or_else(|| json!({}));
            out.push(McpTool {
                name,
                description,
                input_schema,
            });
        }
        Ok(out)
    }

    /// Invoke `tools/call` with `name` and the given `params` as `arguments`.
    ///
    /// Why: The primary method for a console or agent to invoke a service's
    /// tools (e.g., `console_metrics` for health data).
    /// What: Sends `tools/call`, parses the result, and returns the raw JSON
    /// value for the caller to interpret. Calls `ensure_alive` first.
    /// Test: End-to-end in trusty-agents integration tests.
    pub async fn call_tool(&mut self, name: &str, params: Value) -> Result<Value> {
        self.ensure_alive().await?;
        let id = self.alloc_id();
        let req = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": { "name": name, "arguments": params },
        });
        let resp = self.request(&req).await?;
        extract_result(resp)
    }

    /// Send a `ping` request — useful for liveness checks.
    ///
    /// Why: Operators and supervisors need a cheap way to verify the child is
    /// alive and responsive without triggering side effects.
    /// What: Sends a `ping` request and discards the response. Calls
    /// `ensure_alive` first to respawn if needed.
    /// Test: Liveness check in integration tests.
    pub async fn ping(&mut self) -> Result<()> {
        self.ensure_alive().await?;
        let id = self.alloc_id();
        let req = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "ping",
        });
        let _ = self.request(&req).await?;
        Ok(())
    }

    /// Write one JSON value followed by a newline, then flush.
    ///
    /// Why: MCP uses newline-delimited JSON framing; every write must end with
    /// `\n` so the server's line reader sees a complete frame. Flushing
    /// ensures the bytes leave the buffer immediately.
    /// What: Serialises `value` to bytes, writes them and a `\n`, then flushes
    /// the buffered writer.
    /// Test: Covered indirectly by all request tests.
    async fn write_line(&mut self, value: &Value) -> Result<()> {
        let bytes = serde_json::to_vec(value).context("serializing JSON-RPC frame")?;
        self.stdin.write_all(&bytes).await?;
        self.stdin.write_all(b"\n").await?;
        self.stdin.flush().await?;
        Ok(())
    }

    /// Read one newline-terminated JSON object from the child.
    ///
    /// Why: Some MCP servers (e.g., trusty-memory) print a human-readable
    /// status banner to stdout before the JSON-RPC stream begins. Treating
    /// such lines as JSON-RPC frames would break the handshake. We skip any
    /// line that does not start with `{` and log it at debug level — these
    /// are expected, not warnings. (See issue #425.)
    /// What: Loops reading lines, discarding non-JSON prefix lines, until a
    /// line starting with `{` is found, then parses it as JSON.
    /// Test: `read_line_skips_non_json_prefix_lines` feeds banner-then-JSON
    /// through the codec and asserts the JSON object is returned.
    pub(super) async fn read_line(&mut self) -> Result<Value> {
        loop {
            let mut line = String::new();
            let n = self.stdout.read_line(&mut line).await?;
            if n == 0 {
                bail!("MCP server closed stdout before responding");
            }
            let trimmed = line.trim_start();
            if !trimmed.starts_with('{') {
                debug!(
                    "StdioMcpClient: skipping non-JSON line from child stdout: {:?}",
                    line.trim_end()
                );
                continue;
            }
            let value: Value = serde_json::from_str(line.trim_end())
                .with_context(|| format!("parsing JSON-RPC frame: {line:?}"))?;
            return Ok(value);
        }
    }

    /// Send `req` and read responses until one matches the expected id.
    /// Server-initiated notifications (no `id`) are ignored.
    ///
    /// Why: JSON-RPC allows servers to send notifications (no id) at any time.
    /// A simple read-one-frame approach would misinterpret a notification as
    /// a response. This loop discards notifications and out-of-order ids
    /// (rare) until the matching response arrives.
    /// What: Wraps the round-trip in a 30s timeout. Returns Err if the timeout
    /// fires or if the frame cannot be parsed.
    /// Test: Covered by every method that calls `request`.
    async fn request(&mut self, req: &Value) -> Result<Value> {
        let expected_id = req
            .get("id")
            .cloned()
            .ok_or_else(|| anyhow!("request must carry an id"))?;

        timeout(CALL_TIMEOUT, async {
            self.write_line(req).await?;
            loop {
                let frame = self.read_line().await?;
                // Skip server-initiated notifications.
                if frame.get("id").is_none() {
                    continue;
                }
                if frame.get("id") == Some(&expected_id) {
                    return Ok::<Value, anyhow::Error>(frame);
                }
                // Out-of-order id (rare). Continue reading.
            }
        })
        .await
        .map_err(|_| anyhow!("MCP request timed out after {:?}", CALL_TIMEOUT))?
    }
}
