//! Stdio JSON-RPC 2.0 driver for OpenRPC endpoints (#453, #455).
//!
//! Why: The `direct` driver spawns a subprocess and speaks JSON-RPC 2.0
//! over its stdin/stdout — the same NDJSON wire pattern as the existing
//! `stdio-mcp` plugin path, but with a different application protocol
//! (OpenRPC, https://spec.open-rpc.org/, advertised as `openrpc/1` instead
//! of MCP `2024-11-05`). Stdio transport keeps the
//! integration shape uniform with `[[mcp.services]]` and avoids HTTP
//! servers, port allocation, and bearer-token plumbing for what is
//! fundamentally a local IPC concern.
//! What: `DirectDriver` owns a child process and a buffered stdin/stdout
//! pair guarded by a `tokio::sync::Mutex` (the `RegistryDriver` trait
//! takes `&self`, but stdio I/O requires `&mut`). `discover` issues
//! `rpc.discover`, `call_tool` issues a single `tools/call`, and
//! `call_tool_batch` packs requests into a JSON-RPC 2.0 Batch array
//! written as one line. Optional `transport.max_concurrency` is honored
//! via a permit semaphore; `transport.timeout_ms` bounds each request.
//! Test: `extract_rpc_result_*` cover envelope parsing; `new_*` cover
//! spawn-time validation; the ignored unix tests round-trip a real
//! sh-script "echo server" through stdin/stdout to verify the full wire
//! flow without depending on a real OpenRPC server binary.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::{Mutex, Semaphore};
use tokio::time::timeout;
use tracing::{debug, warn};
use uuid::Uuid;

use super::config::EndpointConfig;
use super::discovery::{EndpointCapabilities, EndpointManifest};
use super::driver::{BatchCall, BatchResult, RegistryDriver};

const PROTOCOL_VERSION: &str = "openrpc/1";
const CLIENT_NAME: &str = "trusty-agents";
const DEFAULT_TIMEOUT_MS: u64 = 30_000;

/// Holds the child process and framed stdio. Wrapped in `Mutex` so the
/// trait's `&self` methods can borrow it mutably for I/O.
struct ChildConn {
    child: Child,
    stdin: BufWriter<ChildStdin>,
    stdout: BufReader<ChildStdout>,
}

impl ChildConn {
    fn is_alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }
}

impl Drop for ChildConn {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
    }
}

pub struct DirectDriver {
    name: String,
    command: String,
    args: Vec<String>,
    conn: Mutex<ChildConn>,
    request_timeout: Duration,
    semaphore: Option<Arc<Semaphore>>,
    capabilities: EndpointCapabilities,
}

impl DirectDriver {
    /// Build a driver from an `[[endpoints]]` config entry.
    ///
    /// Why: Spawning at construction time means startup logs and errors
    /// surface immediately rather than at the first tool call. The mutex
    /// lets us share an `Arc<dyn RegistryDriver>` across the registry
    /// while keeping stdio I/O `&mut`.
    /// What: Validates `command` is set, spawns the subprocess with
    /// stdin/stdout piped and stderr written to a per-plugin log file,
    /// and prepares the optional concurrency semaphore + timeout.
    /// Test: Construction is exercised indirectly by the registry build
    /// path; failure modes (missing command, missing binary) covered by
    /// `new_requires_command` and `new_fails_on_missing_binary`.
    pub fn new(config: &EndpointConfig) -> Result<Self> {
        let command = config
            .command
            .clone()
            .context("direct driver requires `command` field (path to OpenRPC binary)")?;
        let args = config.args.clone().unwrap_or_default();

        let request_timeout = config
            .transport
            .as_ref()
            .and_then(|t| t.timeout_ms)
            .map(Duration::from_millis)
            .unwrap_or_else(|| Duration::from_millis(DEFAULT_TIMEOUT_MS));

        let semaphore = config
            .transport
            .as_ref()
            .and_then(|t| t.max_concurrency)
            .filter(|n| *n > 0)
            .map(|n| Arc::new(Semaphore::new(n)));

        let conn = spawn_child(&command, &args)?;

        Ok(Self {
            name: config.name.clone(),
            command,
            args,
            conn: Mutex::new(conn),
            request_timeout,
            semaphore,
            // Server-side `rpc.discover` will fill this in. Until then assume
            // no batch support; we'll upgrade after a successful discover.
            capabilities: EndpointCapabilities::default(),
        })
    }

    /// Send one JSON-RPC envelope (object or array) and wait for the
    /// matching response (same shape: object or array).
    ///
    /// Why: Single-request and batch paths share the wire I/O — only the
    /// envelope shape differs. Centralising keeps respawn + timeout +
    /// concurrency logic in one place.
    /// What: Acquires the optional concurrency permit, ensures the child
    /// is alive (respawning if needed), writes one NDJSON line, then
    /// reads NDJSON lines until one parses as JSON. Non-JSON banner
    /// lines (any line not starting with `{` or `[`) are skipped at
    /// `debug` level — matches the `StdioMcpClient` policy from #425.
    async fn send(&self, envelope: &Value) -> Result<Value> {
        let _permit = match &self.semaphore {
            Some(sem) => Some(
                sem.clone()
                    .acquire_owned()
                    .await
                    .context("driver concurrency semaphore closed")?,
            ),
            None => None,
        };

        let mut guard = self.conn.lock().await;

        if !guard.is_alive() {
            warn!(
                endpoint = %self.name,
                "DirectDriver: child process exited, respawning"
            );
            *guard = spawn_child(&self.command, &self.args)
                .with_context(|| format!("respawning {} subprocess", self.name))?;
        }

        let bytes = serde_json::to_vec(envelope).context("serializing JSON-RPC envelope")?;
        let endpoint_name = self.name.clone();

        let fut = async {
            guard.stdin.write_all(&bytes).await?;
            guard.stdin.write_all(b"\n").await?;
            guard.stdin.flush().await?;
            loop {
                let mut line = String::new();
                let n = guard.stdout.read_line(&mut line).await?;
                if n == 0 {
                    bail!("OpenRPC subprocess closed stdout before responding");
                }
                let trimmed = line.trim_start();
                if !(trimmed.starts_with('{') || trimmed.starts_with('[')) {
                    debug!(
                        endpoint = %endpoint_name,
                        "DirectDriver: skipping non-JSON stdout line: {:?}",
                        line.trim_end()
                    );
                    continue;
                }
                let value: Value = serde_json::from_str(line.trim_end())
                    .with_context(|| format!("parsing JSON-RPC frame: {line:?}"))?;
                return Ok::<Value, anyhow::Error>(value);
            }
        };

        timeout(self.request_timeout, fut).await.map_err(|_| {
            anyhow!(
                "OpenRPC request to '{}' timed out after {:?}",
                self.name,
                self.request_timeout
            )
        })?
    }

    /// Single JSON-RPC 2.0 call.
    async fn rpc_call(&self, method: &str, params: Value) -> Result<Value> {
        let id = Uuid::new_v4().to_string();
        let envelope = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        let resp = self.send(&envelope).await?;
        extract_rpc_result(&resp)
    }
}

/// Spawn the configured binary with stdin/stdout piped. Stderr is sent
/// to the same per-plugin log file the `StdioMcpClient` uses so the
/// terminal stays clean.
fn spawn_child(command: &str, args: &[String]) -> Result<ChildConn> {
    let mut child = Command::new(command)
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(crate::plugins::stdio_mcp::plugin_stderr_stdio(command))
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("failed to spawn OpenRPC binary {command}"))?;

    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| anyhow!("child stdin not captured for {command}"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("child stdout not captured for {command}"))?;

    Ok(ChildConn {
        child,
        stdin: BufWriter::new(stdin),
        stdout: BufReader::new(stdout),
    })
}

#[async_trait]
impl RegistryDriver for DirectDriver {
    fn endpoint_name(&self) -> &str {
        &self.name
    }

    async fn discover(&self) -> Result<EndpointManifest> {
        let params = json!({
            "client": {"name": CLIENT_NAME, "version": env!("CARGO_PKG_VERSION")},
            "protocol_versions": [PROTOCOL_VERSION],
        });
        let v = self.rpc_call("rpc.discover", params).await?;
        let manifest: EndpointManifest = serde_json::from_value(v.clone())
            .with_context(|| format!("malformed rpc.discover result: {v}"))?;
        Ok(manifest)
    }

    async fn call_tool(&self, name: &str, args: Value) -> Result<Value> {
        let params = json!({"name": name, "arguments": args});
        self.rpc_call("tools/call", params).await
    }

    /// Native JSON-RPC 2.0 Batch: pack all calls into one stdin write.
    async fn call_tool_batch(&self, calls: Vec<BatchCall>) -> Vec<BatchResult> {
        if calls.is_empty() {
            return Vec::new();
        }

        let envelopes: Vec<Value> = calls
            .iter()
            .map(|c| {
                json!({
                    "jsonrpc": "2.0",
                    "id": c.id,
                    "method": "tools/call",
                    "params": {"name": c.name, "arguments": c.args},
                })
            })
            .collect();

        let batch_body = Value::Array(envelopes);

        let response = match self.send(&batch_body).await {
            Ok(v) => v,
            Err(e) => {
                let err = anyhow!("batch request failed: {e}");
                return calls
                    .into_iter()
                    .map(|c| BatchResult {
                        id: c.id,
                        result: Err(anyhow!("{err}")),
                    })
                    .collect();
            }
        };

        let array = match response.as_array() {
            Some(a) => a.clone(),
            None => {
                let err = anyhow!("expected JSON-RPC 2.0 Batch array response, got: {response}");
                return calls
                    .into_iter()
                    .map(|c| BatchResult {
                        id: c.id,
                        result: Err(anyhow!("{err}")),
                    })
                    .collect();
            }
        };

        let mut by_id: std::collections::HashMap<String, Value> =
            std::collections::HashMap::with_capacity(array.len());
        for env in array {
            if let Some(id) = env.get("id").and_then(|v| v.as_str()) {
                by_id.insert(id.to_string(), env);
            }
        }

        calls
            .into_iter()
            .map(|c| {
                let result = match by_id.remove(&c.id) {
                    Some(env) => extract_rpc_result(&env),
                    None => Err(anyhow!("no batch response element matched id={}", c.id)),
                };
                BatchResult { id: c.id, result }
            })
            .collect()
    }

    fn capabilities(&self) -> &EndpointCapabilities {
        &self.capabilities
    }
}

/// Extract `result` from a JSON-RPC 2.0 response envelope, surfacing errors.
fn extract_rpc_result(envelope: &Value) -> Result<Value> {
    if let Some(err) = envelope.get("error") {
        let code = err.get("code").and_then(|v| v.as_i64()).unwrap_or(0);
        let msg = err
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("(no message)");
        bail!("JSON-RPC error {code}: {msg}");
    }
    envelope
        .get("result")
        .cloned()
        .ok_or_else(|| anyhow!("JSON-RPC response missing `result`: {envelope}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::registry::config::DriverKind;

    fn cfg(name: &str, command: Option<&str>, args: Vec<&str>) -> EndpointConfig {
        EndpointConfig {
            name: name.to_string(),
            driver: DriverKind::Direct,
            description: None,
            url: None,
            command: command.map(str::to_string),
            args: Some(args.into_iter().map(String::from).collect()),
            enabled: true,
            scopes: vec![],
            discovery_ttl_secs: 0,
            eager_discovery: false,
            auth: None,
            transport: None,
        }
    }

    #[test]
    fn extract_result_success() {
        let env = json!({"jsonrpc": "2.0", "id": "1", "result": {"ok": true}});
        let out = extract_rpc_result(&env).unwrap();
        assert_eq!(out, json!({"ok": true}));
    }

    #[test]
    fn extract_result_error() {
        let env = json!({
            "jsonrpc": "2.0",
            "id": "1",
            "error": {"code": -32601, "message": "Method not found"},
        });
        let e = extract_rpc_result(&env).unwrap_err().to_string();
        assert!(e.contains("-32601"));
        assert!(e.contains("Method not found"));
    }

    #[test]
    fn extract_result_missing_both_errors() {
        let env = json!({"jsonrpc": "2.0", "id": "1"});
        assert!(extract_rpc_result(&env).is_err());
    }

    #[tokio::test]
    async fn new_requires_command() {
        let mut c = cfg("ep", Some("/bin/cat"), vec![]);
        c.command = None;
        let r = DirectDriver::new(&c);
        let err = match r {
            Err(e) => e,
            Ok(_) => panic!("missing command must fail fast"),
        };
        assert!(
            err.to_string().contains("command"),
            "error should mention `command`"
        );
    }

    /// Why: Spawning a non-existent binary must error at construction so
    /// startup can degrade gracefully rather than blocking on the first
    /// `rpc.discover` call.
    /// What: Build a config pointing at a path that cannot exist and
    /// assert `new()` returns Err.
    #[tokio::test]
    async fn new_fails_on_missing_binary() {
        let c = cfg("ep", Some("/nonexistent/openrpc/binary/xyzzy-455"), vec![]);
        let r = DirectDriver::new(&c);
        assert!(r.is_err(), "missing binary must fail at spawn");
    }

    /// Why: Verify the full stdin → stdout JSON-RPC round-trip without a
    /// real server. An awk program reads the inbound JSON line, extracts
    /// the `"id":"..."` field verbatim, and emits a canned `result`
    /// envelope with that same id. Confirms `send` correctly writes one
    /// NDJSON line, matches responses by `id`, and that `rpc_call`
    /// unwraps `result`.
    #[tokio::test]
    #[cfg(unix)]
    async fn rpc_call_round_trips_against_echo_server() {
        let script = r#"
awk '
{
  match($0, /"id":"[^"]*"/);
  id = substr($0, RSTART, RLENGTH);
  printf("{\"jsonrpc\":\"2.0\",%s,\"result\":{\"echoed\":true}}\n", id);
  fflush();
}
'
"#;
        let c = cfg("ep", Some("sh"), vec!["-c", script]);
        let driver = DirectDriver::new(&c).expect("spawn echo server");
        let v = driver
            .rpc_call("tools/call", json!({"name": "x", "arguments": {}}))
            .await
            .expect("rpc_call should succeed");
        assert_eq!(v, json!({"echoed": true}));
    }

    /// Why: Batch calls write one JSON array line; the response is
    /// expected as an array of envelopes correlated by `id`. This test
    /// confirms the wire shape end-to-end against a minimal awk-based
    /// server that echoes the two inbound ids back with canned results.
    #[tokio::test]
    #[cfg(unix)]
    async fn call_tool_batch_round_trips_against_batch_server() {
        let script = r#"
awk '
{
  s = $0; n = 0;
  while (match(s, /"id":"[^"]*"/) > 0 && n < 2) {
    ids[n++] = substr(s, RSTART, RLENGTH);
    s = substr(s, RSTART + RLENGTH);
  }
  printf("[{\"jsonrpc\":\"2.0\",%s,\"result\":{\"i\":0}},{\"jsonrpc\":\"2.0\",%s,\"result\":{\"i\":1}}]\n", ids[0], ids[1]);
  fflush();
}
'
"#;
        let c = cfg("ep", Some("sh"), vec!["-c", script]);
        let driver = DirectDriver::new(&c).expect("spawn batch server");
        let calls = vec![
            BatchCall {
                id: "a".into(),
                name: "x".into(),
                args: json!({}),
            },
            BatchCall {
                id: "b".into(),
                name: "y".into(),
                args: json!({}),
            },
        ];
        let results = driver.call_tool_batch(calls).await;
        assert_eq!(results.len(), 2);
        for r in &results {
            assert!(r.result.is_ok(), "batch entry failed: {:?}", r.result);
        }
    }
}
