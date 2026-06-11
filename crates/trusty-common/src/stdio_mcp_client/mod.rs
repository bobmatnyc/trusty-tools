//! MCP (Model Context Protocol) client over child-process stdio.
//!
//! Why: MCP servers are commonly distributed as binaries that speak JSON-RPC
//! 2.0 over stdin/stdout (one JSON object per line). Sharing a single client
//! implementation across the workspace (trusty-agents, trusty-console, and any
//! future local service) ensures consistent framing, timeout handling, and
//! lifecycle semantics. The client originated in
//! `trusty-agents/src/plugins/stdio_mcp` and was promoted here as part of
//! epic #1104 Phase 0a so trusty-console can use it without a circular
//! dependency on trusty-agents.
//!
//! What: `StdioMcpClient` spawns a binary, performs the MCP `initialize`
//! handshake (protocol version `2024-11-05`), and exposes `list_tools`,
//! `call_tool`, and `ping` methods. JSON-RPC errors are surfaced as
//! `anyhow::Error` carrying the code and message. Server logs go to stderr;
//! stdout is reserved for clean newline-delimited JSON.
//!
//! Test: See `tests` below — JSON-RPC envelope construction, error code
//! mapping, ID monotonicity. A `#[ignore]`d integration test covers the full
//! spawn flow against an embedded mock written as a shell `cat`-loop, kept
//! out of the default suite to avoid platform/shell dependence.

// Module layout: types + helpers + Drop + JSON-RPC builders live here;
// the `impl StdioMcpClient` request methods live in `client.rs`.
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
use tracing::{debug, info, warn};

/// Tracks which plugin log paths we have already announced. This keeps the
/// "logs at ..." banner to one log entry per plugin per process so a single
/// invocation that spawns/respawns the same plugin doesn't spam the user.
///
/// Why: Without dedup, every respawn would emit a fresh announcement, flooding
/// operators with redundant messages on noisy reconnect cycles.
/// What: A process-global set of announced `PathBuf`s, lazily initialised via
/// `OnceLock`. Thread-safe via `Mutex`.
/// Test: Covered implicitly by `plugin_stderr_stdio` tests; dedup behaviour
/// is exercised by spawning the same binary twice.
fn announced_plugins() -> &'static std::sync::Mutex<std::collections::HashSet<PathBuf>> {
    static ANNOUNCED: OnceLock<std::sync::Mutex<std::collections::HashSet<PathBuf>>> =
        OnceLock::new();
    ANNOUNCED.get_or_init(|| std::sync::Mutex::new(std::collections::HashSet::new()))
}

/// Resolve `binary` to a short, filesystem-safe slug used to name the plugin's
/// stderr log file.
///
/// Why: Plugin binaries can emit human-readable ERROR lines on every invocation
/// when stderr is inherited. We redirect that stream to a per-plugin log file
/// so the parent's terminal stays clean.
/// What: Returns the file_stem of the binary path, or "plugin" if absent.
/// Test: `plugin_slug_extracts_stem` in unit tests below.
fn plugin_slug(binary: &str) -> String {
    std::path::Path::new(binary)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("plugin")
        .to_string()
}

/// Compute the path to a plugin's stderr log file.
///
/// Why: Centralises the path policy so `spawn()` and `respawn()` agree on
/// where to write logs — otherwise debugging would require checking two
/// different locations.
/// What: Uses `$HOME/.trusty-agents/logs/<slug>-stderr.log` when HOME is set,
/// or `/tmp/trusty-agents-<slug>.log` as a fallback. Parent directories are
/// created best-effort. The `.trusty-agents` prefix is retained from the
/// original trusty-agents home to avoid breaking existing log paths.
/// Test: Covered by path-policy assertions in `plugin_log_path_uses_home`.
fn plugin_log_path(slug: &str) -> PathBuf {
    let base = std::env::var_os("HOME")
        .map(|h| PathBuf::from(h).join(".trusty-agents").join("logs"))
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    let _ = std::fs::create_dir_all(&base);
    if std::env::var_os("HOME").is_some() {
        base.join(format!("{slug}-stderr.log"))
    } else {
        base.join(format!("trusty-agents-{slug}.log"))
    }
}

/// Open the plugin's stderr log file in append mode and return a Stdio handle
/// suitable for `Command::stderr`. On failure, falls back to `Stdio::null`
/// (better to silently drop logs than crash the spawn).
///
/// Why: Redirecting stderr prevents plugin noise from polluting the parent
/// process's terminal — especially critical when the parent is an MCP server
/// whose stdout carries JSON-RPC framing.
/// What: Opens `plugin_log_path(slug)` in append mode; announces the path at
/// `tracing::info!` on first use (once per process per plugin). Falls back to
/// `Stdio::null` on open failure, logging the error at `warn` level.
/// Test: `plugin_stderr_stdio_falls_back_on_bad_path` verifies null fallback.
pub fn plugin_stderr_stdio(binary: &str) -> std::process::Stdio {
    let slug = plugin_slug(binary);
    let path = plugin_log_path(&slug);
    let was_new = std::fs::metadata(&path).is_err();
    // Announce once per (process, plugin) via a let-chain so the lock and the
    // set membership check are combined in one conditional (edition 2024).
    if let Ok(mut set) = announced_plugins().lock()
        && !set.contains(&path)
    {
        set.insert(path.clone());
        if was_new {
            info!(
                plugin = %slug,
                log_path = %path.display(),
                "(created) plugin stderr redirected to log file"
            );
        } else {
            debug!(
                plugin = %slug,
                log_path = %path.display(),
                "plugin stderr redirected to log file"
            );
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
                plugin = %slug,
                log_path = %path.display(),
                error = %e,
                "StdioMcpClient: failed to open plugin log; suppressing stderr"
            );
            std::process::Stdio::null()
        }
    }
}

/// MCP protocol version this client speaks during the `initialize` handshake.
///
/// Why: The MCP spec is versioned; advertising the wrong version can cause
/// servers to reject the handshake. This constant centralises the version so
/// all callers are automatically aligned.
/// What: The 2024-11-05 version string as defined by the MCP specification.
/// Test: `initialize_envelope_is_well_formed` asserts this value appears in
/// the envelope's `protocolVersion` field.
pub const MCP_PROTOCOL_VERSION: &str = "2024-11-05";

/// Per-call request timeout.
///
/// Why: Servers that take longer than this are treated as failed; callers can
/// retry or surface the error. 30s is generous enough for cold-start scenarios
/// (e.g. a search daemon loading its HNSW index) without blocking indefinitely.
/// What: Used by `StdioMcpClient::request` as the `tokio::time::timeout` bound.
/// Test: `call_tool_errors_when_respawn_unavailable` verifies the client fails
/// fast (< 5s) rather than blocking the full timeout when respawn fails.
pub(super) const CALL_TIMEOUT: Duration = Duration::from_secs(30);

/// Server identification returned from a successful `initialize` response.
///
/// Why: Callers want to log which server they connected to and verify the
/// negotiated protocol version matches expectations.
/// What: Plain owned strings parsed from the `serverInfo` and
/// `protocolVersion` fields of the initialize result.
/// Test: Indirectly by any test that calls `initialize` end-to-end.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerInfo {
    pub name: String,
    pub version: String,
    pub protocol_version: String,
}

/// One tool advertised by a `tools/list` response.
///
/// Why: Agents and console pollers need the tool name and input schema to call
/// it correctly; the description is used to render selection prompts.
/// What: Mirrors the MCP tool descriptor with snake_case field renaming so
/// our internal types stay idiomatic Rust.
/// Test: `list_tools` integration tests in trusty-agents.
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
    pub(super) child: Child,
    pub(super) stdin: BufWriter<ChildStdin>,
    pub(super) stdout: BufReader<ChildStdout>,
    pub(super) next_id: AtomicU64,
    /// Original binary path used at spawn time, retained for respawn.
    pub(super) binary: String,
    /// Original args used at spawn time, retained for respawn.
    pub(super) args: Vec<String>,
    /// The `clientInfo.name` field advertised during the MCP `initialize`
    /// handshake. Caller-supplied so each consumer (trusty-agents, console,
    /// etc.) can identify itself accurately to the MCP server.
    pub(super) client_name: String,
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
/// (the network of fields is easy to typo). The `client_name` parameter is
/// caller-supplied so each consumer (trusty-agents, trusty-console, etc.)
/// can advertise its own identity in the MCP handshake instead of the
/// generic "trusty-common" library name, which would confuse MCP server
/// logs and any server-side allowlists keyed on `clientInfo.name`.
/// What: Builds a JSON-RPC 2.0 request with `method: "initialize"`,
/// `protocolVersion`, `capabilities: {}`, and `clientInfo` with the
/// supplied `client_name` and the crate's version.
/// Test: `initialize_envelope_is_well_formed` asserts all required fields
/// and verifies the supplied name propagates to `clientInfo.name`.
pub(super) fn build_initialize_request(id: u64, client_name: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "initialize",
        "params": {
            "protocolVersion": MCP_PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": {
                "name": client_name,
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
/// What: If the response contains an `error` key, returns `Err` with the
/// JSON-RPC error code and message. Otherwise returns `Ok(result)`. Returns
/// `Err` if neither `result` nor `error` is present (malformed frame).
/// Test: `extract_result_maps_error_object`, `extract_result_returns_inner_result`,
/// `extract_result_errors_when_missing_result`.
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
    /// required fields are present and that the caller-supplied name
    /// propagates to `clientInfo.name` exactly.
    /// What: Builds initialize requests with two different caller names and
    /// asserts protocolVersion, id, and clientInfo.name match the contract.
    /// Test: This test.
    #[test]
    fn initialize_envelope_is_well_formed() {
        let req = build_initialize_request(7, "trusty-agents");
        assert_eq!(req["jsonrpc"], "2.0");
        assert_eq!(req["id"], 7);
        assert_eq!(req["method"], "initialize");
        assert_eq!(req["params"]["protocolVersion"], MCP_PROTOCOL_VERSION);
        assert_eq!(req["params"]["clientInfo"]["name"], "trusty-agents");
        assert!(req["params"]["capabilities"].is_object());

        // Verify a different caller name also propagates correctly.
        let req2 = build_initialize_request(42, "trusty-console");
        assert_eq!(req2["params"]["clientInfo"]["name"], "trusty-console");
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
        let r = StdioMcpClient::spawn("/nonexistent/mcp/binary/xyzzy", &[], "test-client").await;
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
        let mut client = StdioMcpClient::spawn("sh", &["-c", "exit 0"], "test-client")
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
        let mut client = StdioMcpClient::spawn("sh", &["-c", "exit 0"], "test-client")
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
    /// with id=1 result={"ok":true}, then read directly and assert the banner
    /// was skipped and the JSON object returned.
    /// Test: This test.
    #[tokio::test]
    #[cfg(unix)]
    async fn read_line_skips_non_json_prefix_lines() {
        let script = r#"printf 'trusty-memory v0.1.14 — HTTP admin panel: http://127.0.0.1:9999\n{"jsonrpc":"2.0","id":1,"result":{"ok":true}}\n'; sleep 1"#;
        let mut client = StdioMcpClient::spawn("sh", &["-c", script], "test-client")
            .await
            .unwrap();
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
        let client = StdioMcpClient::spawn("cat", &[], "test-client")
            .await
            .unwrap();
        let a = client.alloc_id();
        let b = client.alloc_id();
        let c = client.alloc_id();
        assert!(a < b && b < c);
    }

    /// Why: The plugin slug must be filesystem-safe and human-readable for log
    /// file naming. Verify it extracts the stem of a full binary path.
    /// What: Call `plugin_slug` with a path like `/usr/bin/trusty-search` and
    /// assert it returns `"trusty-search"`.
    /// Test: This test.
    #[test]
    fn plugin_slug_extracts_stem() {
        assert_eq!(plugin_slug("/usr/bin/trusty-search"), "trusty-search");
        assert_eq!(plugin_slug("trusty-memory"), "trusty-memory");
        assert_eq!(plugin_slug(""), "plugin"); // empty → fallback
    }
}
