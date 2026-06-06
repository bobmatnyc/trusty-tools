//! The `impl StdioMcpClient` request methods (spawn + JSON-RPC round-trips).
//!
//! Why: The spawn flow and per-method JSON-RPC request/response handling are
//! the bulk of the client; isolating them from the struct/types/Drop in
//! `mod.rs` keeps both files under the 500-line cap.
//! What: `StdioMcpClient::new`, `initialize`, `list_tools`, `call_tool`, and
//! the low-level send/recv helpers — dispatching through `build_initialize_request`
//! / `extract_result` defined in `mod.rs`.
//! Test: JSON-RPC framing is unit-tested in `stdio_mcp::tests`; the full spawn
//! flow has an `#[ignore]`d integration test.

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
    /// Spawn `binary` with `args`, piping stdin/stdout (stderr is inherited
    /// so server logs surface in the parent's terminal).
    ///
    /// Why: The MCP transport requires clean JSON on stdout, so logs MUST
    /// go to stderr. Inheriting stderr keeps debugging simple — server
    /// output appears in the same console as the harness.
    /// What: Returns an unconnected client with the handshake NOT yet sent.
    /// Call `initialize` next.
    /// Test: Indirectly via `#[ignore]`d e2e test; unit-test failure is
    /// covered by `spawn_missing_binary_errors` below.
    pub async fn spawn(binary: &str, args: &[&str]) -> Result<Self> {
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
        })
    }

    /// Check whether the child process is still running.
    ///
    /// Why: When the MCP child dies (crash, OOM, killed externally) and the
    /// caller writes to its stdin, the write blocks until the 30s timeout —
    /// causing 15-60s query latency. Probing `try_wait()` lets callers detect
    /// the dead child cheaply and respawn before writing. (See issue #421.)
    /// What: Returns `true` if `try_wait()` reports `Ok(None)` (still running);
    /// `false` if the process has exited or `try_wait()` errored.
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
        let req = build_initialize_request(id);
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
