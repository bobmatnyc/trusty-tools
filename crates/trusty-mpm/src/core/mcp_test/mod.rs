//! Verify user-scope MCP servers with a real MCP stdio handshake.
//!
//! Why: `tm mcp add|list|get` register and inspect servers, but registration
//! only proves the *config* is well-formed — not that the server actually
//! starts and speaks MCP. Operators (and CI) need a single command that spawns
//! each configured server, drives the real `initialize` → `initialized` →
//! `tools/list` exchange, and reports pass/fail. `tm mcp test` is that command;
//! this module is its testable core.
//!
//! What: splits cleanly into a **pure** layer (result types, transport
//! classification, target selection, JSON-RPC message build/parse, exit-code
//! aggregation — all unit-testable with no live server) and an **I/O** layer
//! (`probe_*`, which spawn a subprocess or open an HTTP connection). The stdio
//! probe matches the framing every trusty-* MCP server uses:
//! newline-delimited JSON-RPC 2.0 (one compact JSON object per line, `\n`
//! terminated), NOT LSP `Content-Length` — confirmed against
//! [`trusty_common::mcp::run_stdio_loop`], the loop each `serve --stdio` server
//! runs, which reads with `BufReader::lines()` and writes `to_string(resp) +
//! "\n"`. The `initialize` payload uses `protocolVersion` `"2024-11-05"`,
//! matching [`trusty_common::mcp::initialize_response`]. Unlike
//! `run_stdio_loop`'s plain `BufReader::lines()`, the probe's line reader is
//! byte-capped (see [`MAX_LINE_BYTES`]) — a misbehaving server that writes
//! non-newline-terminated noise to stdout cannot grow the read buffer
//! unbounded within the timeout window.
//!
//! Transport scope: `http`/`sse` servers get an HTTP *reachability* check (any
//! HTTP response — even a 4xx — means the endpoint answered, so it passes); a
//! full SSE/streamable-HTTP MCP handshake is intentionally out of scope. stdio
//! servers get the full handshake and a real tool count.
//!
//! Test: `tests` submodule covers selection, classification, message
//! (de)serialization against fixtures, and aggregation. The subprocess spawn is
//! integration-level and is not unit-tested here (the message layer it drives
//! is fixture-tested instead).

use std::time::Duration;

use serde_json::{Value, json};

use crate::core::mcp_config::{
    builtin_server_entry, managed_mcp_server_names, strip_arg_separator,
};

#[cfg(test)]
mod tests;

/// Per-server handshake budget.
///
/// Why: one hung or slow server must not stall the whole sweep; a few seconds is
/// ample for a local subprocess to answer `initialize` + `tools/list` yet short
/// enough to keep `tm mcp test` snappy.
/// What: 5 seconds, applied per server (the sweep tests servers sequentially, so
/// each is independently bounded — a hung server times out and the sweep moves
/// on).
/// Test: value asserted by `default_timeout_is_bounded`.
pub const DEFAULT_PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// Maximum bytes buffered while scanning for one stdio line terminator.
///
/// Why: `tokio::io::AsyncBufReadExt::lines()`/`read_until` has no built-in cap —
/// a server that writes to stdout without a newline (stray `println!` debug
/// output, a crash dump, …) would otherwise grow the line buffer for the entire
/// [`DEFAULT_PROBE_TIMEOUT`] window. Capping the scan (via `AsyncReadExt::take`,
/// see [`read_bounded_line`]) turns that into a bounded, reported
/// `McpTestOutcome::Protocol("line too long")` instead of unbounded memory
/// growth.
/// What: 1 MiB — generous for any legitimate JSON-RPC response, tight enough to
/// bound worst-case memory.
/// Test: `read_bounded_line_rejects_oversized_line`.
pub const MAX_LINE_BYTES: usize = 1024 * 1024;

/// The transport a server entry uses, as understood by the probe.
///
/// Why: the probe dispatches to a subprocess handshake (stdio) or an HTTP
/// reachability check (http/sse); a typed classification keeps that dispatch
/// total and drives the display column.
/// What: `Stdio`, `Http`, or `Sse`, derived from an entry's `type` field (or
/// inferred from the presence of `url`). Derived from
/// [`crate::core::mcp_config::McpTransport`]'s wire discriminants.
/// Test: `classify_transport_*`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestTransport {
    /// Local subprocess speaking MCP over stdio.
    Stdio,
    /// Remote streamable-HTTP endpoint.
    Http,
    /// Remote SSE endpoint.
    Sse,
}

impl TestTransport {
    /// The lowercase wire discriminant for display / JSON output.
    pub fn as_str(self) -> &'static str {
        match self {
            TestTransport::Stdio => "stdio",
            TestTransport::Http => "http",
            TestTransport::Sse => "sse",
        }
    }
}

/// Outcome of testing a single server.
///
/// Why: the CLI needs to distinguish a pass (and how many tools) from each
/// distinct failure mode so the table and `--json` output are actionable and
/// the exit code can aggregate failures.
/// What: `Ok` carries the stdio tool count (`Some`) or marks an http/sse
/// endpoint reachable (`None`). Every other variant is a failure with a
/// human-readable detail where relevant.
/// Test: `passed_only_true_for_ok`, `render_labels_*`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpTestOutcome {
    /// Handshake succeeded (`tools = Some(n)` for stdio) or endpoint reachable
    /// (`tools = None` for http/sse).
    Ok {
        /// Tool count from `tools/list` (stdio); `None` for an http/sse
        /// reachability pass.
        tools: Option<usize>,
    },
    /// Entry present but structurally invalid (missing `command` or `url`).
    Malformed(String),
    /// The stdio subprocess could not be spawned (binary missing, no exec bit …).
    Spawn(String),
    /// The handshake exceeded [`DEFAULT_PROBE_TIMEOUT`].
    Timeout,
    /// A protocol-level failure: non-JSON line, missing fields, a server error
    /// response, or the server closing stdout before replying.
    Protocol(String),
    /// An http/sse endpoint could not be reached (connection refused, timeout …).
    Unreachable(String),
}

/// A server chosen for testing: its name, raw config entry, and transport.
///
/// Why: selection (which servers to test) is decided up front and separately
/// from probing so the selection logic is pure and unit-testable.
/// What: pairs the resolved `entry` with its classified `transport`.
/// Test: built by [`select_targets`]; see `select_*` tests.
#[derive(Debug, Clone)]
pub struct TestTarget {
    /// The `mcpServers` key.
    pub name: String,
    /// The raw entry JSON (stdio command/args/env or http/sse url/headers).
    pub entry: Value,
    /// The classified transport.
    pub transport: TestTransport,
}

/// The full result of testing one server.
#[derive(Debug, Clone)]
pub struct McpTestResult {
    /// The server name.
    pub name: String,
    /// The transport that was probed.
    pub transport: TestTransport,
    /// The outcome.
    pub outcome: McpTestOutcome,
}

impl McpTestResult {
    /// Whether this server passed (the only success variant is `Ok`).
    ///
    /// Test: `passed_only_true_for_ok`.
    pub fn passed(&self) -> bool {
        matches!(self.outcome, McpTestOutcome::Ok { .. })
    }

    /// A `(result_label, detail)` pair for the human table.
    ///
    /// Why: the table's RESULT and DETAIL columns share one source of truth so a
    /// new outcome variant can't render inconsistently between them.
    /// What: `("PASS", "12 tools" | "reachable")` on success; `("FAIL", <cause>)`
    /// otherwise.
    /// Test: `render_labels_pass_and_fail`.
    pub fn render(&self) -> (&'static str, String) {
        match &self.outcome {
            McpTestOutcome::Ok { tools: Some(n) } => ("PASS", format!("{n} tools")),
            McpTestOutcome::Ok { tools: None } => ("PASS", "reachable".to_string()),
            McpTestOutcome::Malformed(m) => ("FAIL", format!("malformed: {m}")),
            McpTestOutcome::Spawn(m) => ("FAIL", format!("spawn error: {m}")),
            McpTestOutcome::Timeout => ("FAIL", "timeout".to_string()),
            McpTestOutcome::Protocol(m) => ("FAIL", format!("protocol error: {m}")),
            McpTestOutcome::Unreachable(m) => ("FAIL", format!("unreachable: {m}")),
        }
    }

    /// A stable machine-readable JSON object for `--json` output.
    ///
    /// Why: scripts and CI consume `--json`; explicit field names decouple the
    /// wire shape from the internal enum layout.
    /// What: `{name, transport, result, tools, error, passed}` where `result` is
    /// a short kind string, `tools` is the count (or `null`), and `error` is the
    /// failure detail (or `null`).
    /// Test: `to_json_shapes_ok_and_failure`.
    pub fn to_json(&self) -> Value {
        let (result, error, tools): (&str, Option<String>, Option<usize>) = match &self.outcome {
            McpTestOutcome::Ok { tools } => ("ok", None, *tools),
            McpTestOutcome::Malformed(m) => ("malformed", Some(m.clone()), None),
            McpTestOutcome::Spawn(m) => ("spawn_error", Some(m.clone()), None),
            McpTestOutcome::Timeout => ("timeout", None, None),
            McpTestOutcome::Protocol(m) => ("protocol_error", Some(m.clone()), None),
            McpTestOutcome::Unreachable(m) => ("unreachable", Some(m.clone()), None),
        };
        json!({
            "name": self.name,
            "transport": self.transport.as_str(),
            "result": result,
            "tools": tools,
            "error": error,
            "passed": self.passed(),
        })
    }
}

/// Whether any server in a result set failed (drives the process exit code).
///
/// Test: `any_failed_flags_a_single_failure`.
pub fn any_failed(results: &[McpTestResult]) -> bool {
    results.iter().any(|r| !r.passed())
}

/// Classify a server entry's transport.
///
/// Why: the probe must route each entry to the right handshake; the default
/// (no explicit `type`) mirrors `commands::mcp::entry_type` — an entry with a
/// `url` is remote, otherwise stdio.
/// What: `type: "http"|"sse"|"stdio"` maps directly; a missing/unknown `type`
/// infers `Http` when a `url` key is present, else `Stdio`.
/// Test: `classify_transport_explicit`, `classify_transport_inferred`.
pub fn classify_transport(entry: &Value) -> TestTransport {
    match entry.get("type").and_then(Value::as_str) {
        Some("http") => TestTransport::Http,
        Some("sse") => TestTransport::Sse,
        Some("stdio") => TestTransport::Stdio,
        _ if entry.get("url").is_some() => TestTransport::Http,
        _ => TestTransport::Stdio,
    }
}

/// Select which servers to test.
///
/// Why: `tm mcp test` (bare) must sweep every user-scope server unioned with the
/// three framework built-ins (deduped), while `tm mcp test <name>` targets one
/// server and errors if it is unknown. Deciding this from the parsed config —
/// with no I/O — keeps it unit-testable.
/// What: with `name = None`, unions `servers`' keys with
/// [`crate::core::mcp_config::BUILTIN_MANAGED_MCP_SERVERS`] via the shared
/// [`managed_mcp_server_names`] (sorted + deduped) and resolves each name's
/// entry from `servers`, falling back to [`builtin_server_entry`] for a built-in
/// not present in the user config. With `name = Some(n)`, returns just that
/// server (config entry, else built-in fallback), erroring if neither exists.
/// Test: `select_bare_unions_builtins`, `select_named_uses_config`,
/// `select_named_falls_back_to_builtin`, `select_named_unknown_errors`.
pub fn select_targets(
    servers: &serde_json::Map<String, Value>,
    name: Option<&str>,
) -> anyhow::Result<Vec<TestTarget>> {
    match name {
        Some(n) => {
            let entry = servers
                .get(n)
                .cloned()
                .or_else(|| builtin_server_entry(n))
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "MCP server '{n}' not found among configured servers or framework built-ins"
                    )
                })?;
            Ok(vec![make_target(n.to_string(), entry)])
        }
        None => {
            // Reuse the exact union semantics `managed_mcp_server_names` applies
            // when seeding trust, so the sweep and the trust list never diverge.
            let config = json!({ "mcpServers": Value::Object(servers.clone()) });
            let names = managed_mcp_server_names(&config);
            let targets = names
                .into_iter()
                .map(|n| {
                    let entry = servers
                        .get(&n)
                        .cloned()
                        .or_else(|| builtin_server_entry(&n))
                        .unwrap_or(Value::Null);
                    make_target(n, entry)
                })
                .collect();
            Ok(targets)
        }
    }
}

/// Build a [`TestTarget`], classifying the transport from the entry.
fn make_target(name: String, entry: Value) -> TestTarget {
    let transport = classify_transport(&entry);
    TestTarget {
        name,
        entry,
        transport,
    }
}

// ─── JSON-RPC message layer (pure) ─────────────────────────────────────────

/// Build the MCP `initialize` request.
///
/// What: `{jsonrpc, id, method:"initialize", params:{protocolVersion,
/// capabilities, clientInfo}}` with `protocolVersion` `"2024-11-05"` — the
/// version [`trusty_common::mcp::initialize_response`] advertises.
/// Test: `initialize_request_has_protocol_version`.
pub fn initialize_request(id: i64) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": { "name": "tm-mcp-test", "version": env!("CARGO_PKG_VERSION") }
        }
    })
}

/// Build the `notifications/initialized` notification (id-less).
///
/// Test: `initialized_notification_is_idless`.
pub fn initialized_notification() -> Value {
    json!({ "jsonrpc": "2.0", "method": "notifications/initialized" })
}

/// Build the `tools/list` request.
///
/// Test: `tools_list_request_has_method`.
pub fn tools_list_request(id: i64) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "method": "tools/list", "params": {} })
}

/// Validate an `initialize` response envelope.
///
/// Why: a server that returns an error, or omits `result.protocolVersion`, has
/// not completed the handshake — that must surface as a protocol failure.
/// What: `Err` if the envelope carries a JSON-RPC `error` or lacks
/// `result.protocolVersion`; `Ok(())` otherwise.
/// Test: `validate_initialize_accepts_valid`, `validate_initialize_rejects_*`.
pub fn validate_initialize(resp: &Value) -> Result<(), String> {
    if let Some(err) = resp.get("error") {
        return Err(rpc_error_message(err));
    }
    resp.get("result")
        .and_then(|r| r.get("protocolVersion"))
        .ok_or_else(|| "initialize response missing result.protocolVersion".to_string())?;
    Ok(())
}

/// Extract the tool count from a `tools/list` response envelope.
///
/// What: `Err` if the envelope carries a JSON-RPC `error` or lacks a
/// `result.tools` array; otherwise `Ok(len)`.
/// Test: `parse_tool_count_counts_array`, `parse_tool_count_rejects_*`.
pub fn parse_tool_count(resp: &Value) -> Result<usize, String> {
    if let Some(err) = resp.get("error") {
        return Err(rpc_error_message(err));
    }
    let tools = resp
        .get("result")
        .and_then(|r| r.get("tools"))
        .and_then(Value::as_array)
        .ok_or_else(|| "tools/list response missing result.tools array".to_string())?;
    Ok(tools.len())
}

/// Render a JSON-RPC `error` object into a one-line message.
fn rpc_error_message(err: &Value) -> String {
    let code = err.get("code").and_then(Value::as_i64).unwrap_or(0);
    let msg = err
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("unknown error");
    format!("server error {code}: {msg}")
}

/// Pull the stdio `args` out of an entry as owned strings.
///
/// Why: entries written before the `tm mcp add` fix (or hand-edited) may carry a
/// stray leading `--` separator as their first arg; spawned verbatim that breaks
/// npx/uv-style servers. Stripping it here (via
/// [`crate::core::mcp_config::strip_arg_separator`]) makes the probe exercise the
/// SAME effective argv a fixed session would, so already-broken registries test
/// green without manual repair.
/// What: collects the entry's string `args`, then drops a single leading `--`.
/// Test: `entry_args_strips_legacy_leading_separator`,
/// `entry_args_preserves_deeper_separator`.
fn entry_args(entry: &Value) -> Vec<String> {
    let raw: Vec<String> = entry
        .get("args")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    strip_arg_separator(&raw).to_vec()
}

/// Pull the stdio `env` out of an entry as `(key, value)` pairs.
fn entry_env(entry: &Value) -> Vec<(String, String)> {
    entry
        .get("env")
        .and_then(Value::as_object)
        .map(|m| {
            m.iter()
                .filter_map(|(k, v)| v.as_str().map(|vs| (k.clone(), vs.to_string())))
                .collect()
        })
        .unwrap_or_default()
}

// ─── probe layer (I/O) ─────────────────────────────────────────────────────

/// Test every selected server sequentially, each bounded by `timeout`.
///
/// Why: sequential probing keeps output ordering deterministic and needs no
/// extra concurrency deps; because each server is independently timeout-bounded,
/// a single hung server delays but never blocks the sweep.
/// What: runs [`probe_target`] for each target and collects a [`McpTestResult`].
/// Test: covered end-to-end by `tm mcp test` (integration); pure inputs are
/// unit-tested via the message/selection layers.
pub async fn run_all(targets: Vec<TestTarget>, timeout: Duration) -> Vec<McpTestResult> {
    let mut results = Vec::with_capacity(targets.len());
    for target in targets {
        let outcome = probe_target(&target, timeout).await;
        results.push(McpTestResult {
            name: target.name,
            transport: target.transport,
            outcome,
        });
    }
    results
}

/// Probe one target, dispatching on its transport.
pub async fn probe_target(target: &TestTarget, timeout: Duration) -> McpTestOutcome {
    match target.transport {
        TestTransport::Stdio => probe_stdio(&target.entry, timeout).await,
        TestTransport::Http | TestTransport::Sse => probe_http(&target.entry, timeout).await,
    }
}

/// Spawn a stdio server and run the full MCP handshake, timeout-bounded.
async fn probe_stdio(entry: &Value, timeout: Duration) -> McpTestOutcome {
    let Some(command) = entry.get("command").and_then(Value::as_str) else {
        return McpTestOutcome::Malformed("stdio entry missing 'command'".to_string());
    };
    let args = entry_args(entry);
    let env = entry_env(entry);
    match tokio::time::timeout(timeout, stdio_handshake(command, &args, &env)).await {
        Ok(Ok(count)) => McpTestOutcome::Ok { tools: Some(count) },
        Ok(Err(outcome)) => outcome,
        Err(_) => McpTestOutcome::Timeout,
    }
}

/// Drive `initialize` → `initialized` → `tools/list` over the child's stdio.
///
/// Framing: newline-delimited JSON-RPC 2.0 (one compact object per line, `\n`
/// terminated), matching `trusty_common::mcp::run_stdio_loop`, with each line
/// capped at [`MAX_LINE_BYTES`] (see [`read_bounded_line`]). Returns the tool
/// count on success or an [`McpTestOutcome`] failure. The child is spawned with
/// `kill_on_drop(true)`, so `child`'s `Drop` (run when this function returns,
/// on every path — success, error, or the caller's `tokio::time::timeout`
/// cancelling us mid-await) is what reaps the subprocess; no explicit kill is
/// needed here.
async fn stdio_handshake(
    command: &str,
    args: &[String],
    env: &[(String, String)],
) -> Result<usize, McpTestOutcome> {
    use std::process::Stdio;
    use tokio::io::BufReader;
    use tokio::process::Command;

    let mut cmd = Command::new(command);
    cmd.args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    for (k, v) in env {
        cmd.env(k, v);
    }

    let mut child = cmd
        .spawn()
        .map_err(|e| McpTestOutcome::Spawn(format!("{command}: {e}")))?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| McpTestOutcome::Protocol("child stdin unavailable".to_string()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| McpTestOutcome::Protocol("child stdout unavailable".to_string()))?;
    let mut reader = BufReader::new(stdout);

    // initialize → await response → validate.
    write_message(&mut stdin, &initialize_request(1)).await?;
    let init_resp = read_response(&mut reader, 1).await?;
    validate_initialize(&init_resp).map_err(McpTestOutcome::Protocol)?;

    // Per MCP: acknowledge with the `initialized` notification before tool use.
    write_message(&mut stdin, &initialized_notification()).await?;

    // tools/list → await response → count.
    write_message(&mut stdin, &tools_list_request(2)).await?;
    let list_resp = read_response(&mut reader, 2).await?;
    let count = parse_tool_count(&list_resp).map_err(McpTestOutcome::Protocol)?;

    // `child` drops here; `kill_on_drop(true)` (set above) reaps the
    // subprocess so no orphan survives a successful probe.
    Ok(count)
}

/// Serialize `msg` as one JSON line and write it (flushed) to the child stdin.
async fn write_message<W>(writer: &mut W, msg: &Value) -> Result<(), McpTestOutcome>
where
    W: tokio::io::AsyncWriteExt + Unpin,
{
    let mut line = serde_json::to_string(msg)
        .map_err(|e| McpTestOutcome::Protocol(format!("serialise request: {e}")))?;
    line.push('\n');
    writer
        .write_all(line.as_bytes())
        .await
        .map_err(|e| McpTestOutcome::Protocol(format!("write to server: {e}")))?;
    writer
        .flush()
        .await
        .map_err(|e| McpTestOutcome::Protocol(format!("flush to server: {e}")))?;
    Ok(())
}

/// Read lines until one is a JSON-RPC response with `id == want_id`.
///
/// Skips blank lines and any message whose `id` does not match (server-emitted
/// notifications or unrelated responses). A non-JSON line is a protocol error; a
/// closed stream before the wanted id is a protocol error; a line exceeding
/// [`MAX_LINE_BYTES`] is a protocol error (see [`read_bounded_line`]).
async fn read_response<R>(reader: &mut R, want_id: i64) -> Result<Value, McpTestOutcome>
where
    R: tokio::io::AsyncBufRead + Unpin,
{
    loop {
        match read_bounded_line(reader, MAX_LINE_BYTES).await? {
            Some(line) => {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let value: Value = serde_json::from_str(trimmed).map_err(|e| {
                    McpTestOutcome::Protocol(format!("non-JSON line from server: {e}"))
                })?;
                match value.get("id").and_then(Value::as_i64) {
                    Some(id) if id == want_id => return Ok(value),
                    _ => continue, // notification or unrelated response — keep reading
                }
            }
            None => {
                return Err(McpTestOutcome::Protocol(
                    "server closed stdout before responding".to_string(),
                ));
            }
        }
    }
}

/// Read one `\n`-terminated line, capping the scan at `max_bytes`.
///
/// Why: plain `AsyncBufReadExt::read_until`/`lines()` buffers without limit
/// while scanning for the delimiter — a server that emits non-newline-
/// terminated stdout noise would grow that buffer for the whole probe timeout.
/// Wrapping the reader in [`tokio::io::AsyncReadExt::take`] per call bounds a
/// single `read_until` to at most `max_bytes`, so a missing terminator within
/// that budget fails fast instead of growing unbounded.
/// What: returns `Ok(None)` at true EOF with no bytes read; `Ok(Some(line))`
/// with the `\n` (and a trailing `\r`, if present) stripped, on either finding
/// the delimiter or hitting EOF with a non-empty trailing partial line (mirrors
/// `tokio::io::Lines`' EOF behaviour); `Err(Protocol("line too long"))` when
/// `max_bytes` is exhausted without finding `\n`.
/// Test: `read_bounded_line_reads_terminated_line`,
/// `read_bounded_line_rejects_oversized_line`,
/// `read_bounded_line_returns_none_at_eof`.
async fn read_bounded_line<R>(
    reader: &mut R,
    max_bytes: usize,
) -> Result<Option<String>, McpTestOutcome>
where
    R: tokio::io::AsyncBufRead + Unpin,
{
    use tokio::io::{AsyncBufReadExt, AsyncReadExt};

    let mut buf: Vec<u8> = Vec::new();
    let n = {
        let mut limited = reader.take(max_bytes as u64);
        limited
            .read_until(b'\n', &mut buf)
            .await
            .map_err(|e| McpTestOutcome::Protocol(format!("read from server: {e}")))?
    };

    if n == 0 {
        return Ok(None); // true EOF, nothing buffered
    }
    if buf.last() == Some(&b'\n') {
        buf.pop();
        if buf.last() == Some(&b'\r') {
            buf.pop();
        }
        return Ok(Some(String::from_utf8_lossy(&buf).into_owned()));
    }
    if n >= max_bytes {
        // Filled the cap without ever finding a terminator — bail rather than
        // keep scanning an unbounded amount of server output.
        return Err(McpTestOutcome::Protocol("line too long".to_string()));
    }
    // Read fewer bytes than the cap with no terminator: the stream reached
    // real EOF mid-line. Return the trailing partial line, matching
    // `tokio::io::Lines`' behaviour for an unterminated final line.
    Ok(Some(String::from_utf8_lossy(&buf).into_owned()))
}

/// Reachability-check an http/sse endpoint, timeout-bounded.
///
/// Why: a full SSE/streamable-HTTP MCP handshake is out of scope (see the module
/// docs); confirming the configured URL answers at all is a useful, cheap signal
/// that mirrors `tm services`' `/health` probing.
/// What: issues a `GET` (applying any configured headers) and treats ANY HTTP
/// response — including 4xx/5xx — as reachable (the endpoint answered).
/// Connection/timeout errors are [`McpTestOutcome::Unreachable`].
async fn probe_http(entry: &Value, timeout: Duration) -> McpTestOutcome {
    let Some(url) = entry.get("url").and_then(Value::as_str) else {
        return McpTestOutcome::Malformed("http/sse entry missing 'url'".to_string());
    };
    let client = match reqwest::Client::builder().timeout(timeout).build() {
        Ok(c) => c,
        Err(e) => return McpTestOutcome::Unreachable(format!("HTTP client build failed: {e}")),
    };
    let mut req = client.get(url);
    if let Some(headers) = entry.get("headers").and_then(Value::as_object) {
        for (k, v) in headers {
            if let Some(vs) = v.as_str() {
                req = req.header(k, vs);
            }
        }
    }
    match req.send().await {
        Ok(_resp) => McpTestOutcome::Ok { tools: None },
        Err(e) => McpTestOutcome::Unreachable(format!("{e}")),
    }
}
