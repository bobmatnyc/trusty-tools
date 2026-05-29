//! MCP (Model Context Protocol) client over child-process stdio.
//!
//! Why: MCP servers are commonly distributed as binaries that speak JSON-RPC
//! 2.0 over stdin/stdout (one JSON object per line). To call those tools from
//! open-mpm agents we need a small async client that handles the handshake,
//! request/response correlation, and graceful child-process lifecycle.
//! What: `StdioMcpClient` spawns a binary, performs the MCP `initialize`
//! handshake (protocol version `2024-11-05`), and exposes `list_tools`,
//! `call_tool`, and `ping` methods. JSON-RPC errors are surfaced as
//! `anyhow::Error` carrying the code and message. Server logs go to stderr;
//! stdout is reserved for clean newline-delimited JSON.
//! Test: See `tests` below — JSON-RPC envelope construction, error code
//! mapping, ID monotonicity. A `#[ignore]`d integration test covers the full
//! spawn flow against an embedded mock written as a shell `cat`-loop, kept
//! out of the default suite to avoid platform/shell dependence.

// Module layout (see #366 split): types + helpers + Drop + JSON-RPC builders
// live here; the `impl StdioMcpClient` request methods live in `client.rs`.
mod client;

use std::path::PathBuf;
use std::sync::OnceLock;
use std::sync::atomic::AtomicU64;
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{BufReader, BufWriter};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tracing::{debug, warn};

/// Tracks which plugin log paths we have already announced to stdout. This
/// keeps the "logs at ..." banner to one print per plugin per process so a
/// single `om` invocation that spawns/respawns the same plugin doesn't spam
/// the user. (See issue #442.)
fn announced_plugins() -> &'static std::sync::Mutex<std::collections::HashSet<PathBuf>> {
    static ANNOUNCED: OnceLock<std::sync::Mutex<std::collections::HashSet<PathBuf>>> =
        OnceLock::new();
    ANNOUNCED.get_or_init(|| std::sync::Mutex::new(std::collections::HashSet::new()))
}

/// Resolve `binary` to a short, filesystem-safe slug used to name the plugin's
/// stderr log file.
///
/// Why: Plugin binaries spam human-readable ERROR lines (e.g. trusty-memory's
/// kg.db open errors) on every `om` invocation when stderr is inherited. We
/// redirect that stream to a per-plugin log file so the user's terminal stays
/// clean. (See issue #442 / #424.)
/// What: Returns the file_stem of the binary path, or "plugin" if absent.
fn plugin_slug(binary: &str) -> String {
    std::path::Path::new(binary)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("plugin")
        .to_string()
}

/// Compute the path to a plugin's stderr log file.
///
/// Why: Centralises the path policy so spawn() and respawn() agree on where
/// to write logs (otherwise debugging would require checking two locations).
/// What: Uses `$HOME/.open-mpm/logs/<slug>-stderr.log` when HOME is set, or
/// `/tmp/open-mpm-<slug>.log` as a fallback. Parent directories are created
/// best-effort.
fn plugin_log_path(slug: &str) -> PathBuf {
    let base = std::env::var_os("HOME")
        .map(|h| PathBuf::from(h).join(".open-mpm").join("logs"))
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    let _ = std::fs::create_dir_all(&base);
    if std::env::var_os("HOME").is_some() {
        base.join(format!("{slug}-stderr.log"))
    } else {
        base.join(format!("open-mpm-{slug}.log"))
    }
}

/// Open the plugin's stderr log file in append mode and return a Stdio handle
/// suitable for `Command::stderr`. On failure, falls back to `Stdio::null`
/// (better to silently drop logs than crash the spawn).
pub(crate) fn plugin_stderr_stdio(binary: &str) -> std::process::Stdio {
    let slug = plugin_slug(binary);
    let path = plugin_log_path(&slug);
    // Announce once per (process, plugin) so users discover the log location
    // without seeing the banner on every respawn.
    let was_new = std::fs::metadata(&path).is_err();
    {
        if let Ok(mut set) = announced_plugins().lock()
            && !set.contains(&path)
        {
            set.insert(path.clone());
            if was_new {
                println!(
                    "open-mpm: plugin '{slug}' stderr -> {} (created)",
                    path.display()
                );
            } else {
                debug!("open-mpm: plugin '{slug}' stderr -> {}", path.display());
            }
        }
    }
    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        Ok(file) => std::process::Stdio::from(file),
        Err(e) => {
            warn!(
                "StdioMcpClient: failed to open plugin log {}: {e}; suppressing stderr",
                path.display()
            );
            std::process::Stdio::null()
        }
    }
}

/// MCP protocol version this client speaks during the `initialize` handshake.
pub const MCP_PROTOCOL_VERSION: &str = "2024-11-05";

/// Per-call request timeout. Servers that take longer than this are treated
/// as failed (caller can retry or surface the error).
pub(super) const CALL_TIMEOUT: Duration = Duration::from_secs(30);

/// Server identification returned from a successful `initialize` response.
///
/// Why: Callers want to log which server they connected to and verify the
/// negotiated protocol version matches expectations.
/// What: Plain owned strings parsed from the `serverInfo` and
/// `protocolVersion` fields of the initialize result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerInfo {
    pub name: String,
    pub version: String,
    pub protocol_version: String,
}

/// One tool advertised by a `tools/list` response.
///
/// Why: Agents need the tool name and input schema to call it correctly;
/// the description is used to render selection prompts.
/// What: Mirrors the MCP tool descriptor with snake_case field renaming so
/// our internal types stay idiomatic Rust.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpTool {
    pub name: String,
    pub description: Option<String>,
    pub input_schema: Value,
}

/// JSON-RPC 2.0 client speaking newline-delimited JSON over a child's stdio.
///
/// Why: Encapsulates child lifetime, framed I/O, and request-id allocation
/// so callers issue method calls without thinking about transport.
/// What: Holds the spawned `Child`, buffered stdin/stdout halves, and an
/// atomic id counter for JSON-RPC `id` fields. Drop kills the child.
/// Test: Covered by the unit tests in this module (envelope building, id
/// monotonicity) and a `#[ignore]`d e2e test for the full spawn flow.
pub struct StdioMcpClient {
    child: Child,
    stdin: BufWriter<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    next_id: AtomicU64,
    /// Original binary path used at spawn time, retained for respawn.
    binary: String,
    /// Original args used at spawn time, retained for respawn.
    args: Vec<String>,
}

impl Drop for StdioMcpClient {
    /// Best-effort kill so we never leak child processes when a client is
    /// dropped without a clean shutdown. `kill_on_drop(true)` set at spawn
    /// also covers this; the explicit call is belt-and-suspenders.
    fn drop(&mut self) {
        let _ = self.child.start_kill();
    }
}

/// Construct the canonical `initialize` request envelope.
///
/// Why: Pulling this out makes it unit-testable without spawning a child
/// (the network of fields is easy to typo).
pub(super) fn build_initialize_request(id: u64) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "initialize",
        "params": {
            "protocolVersion": MCP_PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": {
                "name": "open-mpm",
                "version": env!("CARGO_PKG_VERSION"),
            }
        }
    })
}

/// Pull the `result` out of a JSON-RPC response, mapping `error` objects to
/// `anyhow::Error` carrying the code and message.
///
/// Why: All call paths share this final step; centralising avoids the
/// possibility of one path silently ignoring an error response.
pub(super) fn extract_result(resp: Value) -> Result<Value> {
    if let Some(err) = resp.get("error") {
        let code = err.get("code").and_then(|v| v.as_i64()).unwrap_or(0);
        let message = err
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown error");
        bail!("JSON-RPC error {code}: {message}");
    }
    resp.get("result")
        .cloned()
        .ok_or_else(|| anyhow!("JSON-RPC response missing result"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: The initialize envelope is exact-shape-sensitive; verify all
    /// required fields are present so we don't break the handshake.
    /// What: Builds an initialize request and asserts protocolVersion, id,
    /// and clientInfo.name match the contract.
    /// Test: This test.
    #[test]
    fn initialize_envelope_is_well_formed() {
        let req = build_initialize_request(7);
        assert_eq!(req["jsonrpc"], "2.0");
        assert_eq!(req["id"], 7);
        assert_eq!(req["method"], "initialize");
        assert_eq!(req["params"]["protocolVersion"], MCP_PROTOCOL_VERSION);
        assert_eq!(req["params"]["clientInfo"]["name"], "open-mpm");
        assert!(req["params"]["capabilities"].is_object());
    }

    /// Why: An error response must surface as an Err with the code and
    /// message visible to operators; otherwise debugging MCP servers is
    /// impossible.
    /// What: Feed a synthetic error response through `extract_result` and
    /// assert the resulting Err contains both the code and message text.
    /// Test: This test.
    #[test]
    fn extract_result_maps_error_object() {
        let resp = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "error": { "code": -32601, "message": "method not found" }
        });
        let err = extract_result(resp).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("-32601"), "missing code: {msg}");
        assert!(msg.contains("method not found"), "missing message: {msg}");
    }

    /// Why: A success response must return the inner `result` value
    /// unmodified so callers can parse method-specific fields.
    /// What: Feed a synthetic success response, assert the returned value
    /// equals the embedded `result`.
    /// Test: This test.
    #[test]
    fn extract_result_returns_inner_result() {
        let resp = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": { "tools": [] }
        });
        let v = extract_result(resp).unwrap();
        assert_eq!(v, json!({ "tools": [] }));
    }

    /// Why: A response missing both `result` and `error` is malformed; we
    /// must error rather than silently returning null.
    /// What: Feed a response with only `id`, assert Err.
    /// Test: This test.
    #[test]
    fn extract_result_errors_when_missing_result() {
        let resp = json!({ "jsonrpc": "2.0", "id": 1 });
        assert!(extract_result(resp).is_err());
    }

    /// Why: Spawning a non-existent binary must fail fast with a useful
    /// error so operators can see misconfiguration immediately.
    /// What: Try to spawn `/nonexistent/mcp/binary` and assert Err.
    /// Test: This test.
    #[tokio::test]
    async fn spawn_missing_binary_errors() {
        let r = StdioMcpClient::spawn("/nonexistent/mcp/binary/xyzzy", &[]).await;
        assert!(r.is_err());
    }

    /// Why: Issue #421 — if the MCP child dies between calls, writing to its
    /// stdin blocks for the full 30s timeout. `is_alive()` must report false
    /// so callers can respawn before writing.
    /// What: Spawn `/bin/sh -c "exit 0"` so the child exits immediately. Wait
    /// briefly so the OS reaps the exit, then assert `is_alive()` is false.
    /// Test: This test.
    #[tokio::test]
    #[cfg(unix)]
    async fn is_alive_returns_false_after_child_exits() {
        let mut client = StdioMcpClient::spawn("sh", &["-c", "exit 0"])
            .await
            .unwrap();
        // Give the OS a moment to mark the child as exited.
        for _ in 0..50 {
            if !client.is_alive() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(
            !client.is_alive(),
            "child should be reported dead after `exit 0`"
        );
    }

    /// Why: Issue #421 — when the child is dead and respawn cannot succeed
    /// (e.g., binary missing), `call_tool` MUST return an error fast rather
    /// than blocking on a write to dead stdin until the 30s timeout fires.
    /// What: Spawn a short-lived child, swap its binary path to something
    /// non-existent so respawn fails, wait for it to exit, then assert
    /// `call_tool` returns Err quickly.
    /// Test: This test.
    #[tokio::test]
    #[cfg(unix)]
    async fn call_tool_errors_when_respawn_unavailable() {
        let mut client = StdioMcpClient::spawn("sh", &["-c", "exit 0"])
            .await
            .unwrap();
        // Point respawn at a binary that definitely won't exist.
        client.binary = "/nonexistent/mcp/binary/xyzzy-respawn".to_string();
        client.args.clear();
        // Wait for the child to be reaped.
        for _ in 0..50 {
            if !client.is_alive() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let start = std::time::Instant::now();
        let r = client.call_tool("anything", json!({})).await;
        let elapsed = start.elapsed();
        assert!(r.is_err(), "call_tool should error when respawn fails");
        // Must NOT have waited the full 30s timeout — the whole point of #421.
        assert!(
            elapsed < std::time::Duration::from_secs(5),
            "call_tool should fail fast (<5s), took {elapsed:?}"
        );
    }

    /// Why: Issue #425 — some MCP servers (trusty-memory) print a human-readable
    /// status banner to stdout before the JSON-RPC stream. `read_line` must skip
    /// such non-JSON prefix lines (any line not starting with `{`) so the
    /// handshake doesn't fail trying to parse them.
    /// What: Spawn `printf` to emit a banner line followed by a real JSON object
    /// with id=1 result={"ok":true}, then issue an `initialize`-shaped request
    /// (id=1) and assert the banner was skipped and the JSON object returned.
    /// We bypass `initialize` and call `request` directly so we control the id.
    /// Test: This test.
    #[tokio::test]
    #[cfg(unix)]
    async fn read_line_skips_non_json_prefix_lines() {
        // Emit a banner line, then a valid JSON-RPC response with id=1.
        // The shell script ignores its stdin (read from /dev/null isn't needed —
        // we just don't write to stdin in this test).
        let script = r#"printf 'trusty-memory v0.1.14 — HTTP admin panel: http://127.0.0.1:9999\n{"jsonrpc":"2.0","id":1,"result":{"ok":true}}\n'; sleep 1"#;
        let mut client = StdioMcpClient::spawn("sh", &["-c", script]).await.unwrap();
        // Bypass initialize: read directly. The first frame returned must be
        // the JSON object, with the banner silently skipped.
        let frame = client.read_line().await.unwrap();
        assert_eq!(frame["jsonrpc"], "2.0");
        assert_eq!(frame["id"], 1);
        assert_eq!(frame["result"]["ok"], true);
    }

    /// Why: `alloc_id` underpins request/response correlation; if it ever
    /// returns a duplicate id within a session, replies could be misrouted.
    /// What: Spawn a real `cat` (always present on unix) so we have a valid
    /// client, then call `alloc_id` repeatedly and assert strict monotonicity.
    /// Test: This test.
    #[tokio::test]
    #[cfg(unix)]
    async fn ids_are_monotonic() {
        let client = StdioMcpClient::spawn("cat", &[]).await.unwrap();
        let a = client.alloc_id();
        let b = client.alloc_id();
        let c = client.alloc_id();
        assert!(a < b && b < c);
    }
}
