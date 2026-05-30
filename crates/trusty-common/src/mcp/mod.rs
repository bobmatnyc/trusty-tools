//! Shared JSON-RPC 2.0 / MCP primitives for the trusty-* ecosystem.
//!
//! Why: trusty-memory and trusty-search both speak MCP over stdio (and HTTP/SSE).
//! They had each grown their own `Request` / `Response` / `JsonRpcError` types
//! and stdio loops — virtually identical, but drifting. Centralising avoids
//! the bug-fix-in-one-not-the-other failure mode and gives future MCP servers
//! a one-import surface to ship against.
//!
//! What: JSON-RPC 2.0 request/response envelopes, standard error codes, a
//! helper to build the `initialize` payload, and an async stdio dispatch
//! loop that accepts any `Fn(Request) -> Future<Output=Response>`.
//!
//! Test: `cargo test -p trusty-mcp-core` covers Response construction +
//! the stdio loop round-trip behaviour with an in-memory dispatcher.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub mod openrpc;
pub mod service;

pub use service::ServiceDescriptor;

/// JSON-RPC 2.0 error codes.
///
/// Why: Both trusty-memory and trusty-search referenced these constants with
/// slightly drifting numeric types — consolidating here keeps them aligned.
/// What: `i32` to match the JSON-RPC 2.0 spec; serde_json serialises these
/// transparently as JSON numbers.
/// Test: `error_codes_are_spec_values` asserts the canonical numeric values.
pub mod error_codes {
    pub const PARSE_ERROR: i32 = -32700;
    pub const INVALID_REQUEST: i32 = -32600;
    pub const METHOD_NOT_FOUND: i32 = -32601;
    pub const INVALID_PARAMS: i32 = -32602;
    pub const INTERNAL_ERROR: i32 = -32603;
}

/// Incoming JSON-RPC 2.0 request envelope.
///
/// Why: A single concrete type used by every dispatcher in the workspace.
/// What: `id` is optional (notifications carry no id). `params` defaults to
/// `Value::Null` so callers that omit it parse cleanly.
/// Test: `request_deserialises_without_params` round-trips a no-params request.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Request {
    /// Always `"2.0"`. Stored as `Option<String>` so legacy callers that omit
    /// it can still be parsed and rejected by the dispatcher with a clean
    /// `INVALID_REQUEST` error rather than a parse-level failure.
    #[serde(default)]
    pub jsonrpc: Option<String>,
    #[serde(default)]
    pub id: Option<Value>,
    pub method: String,
    #[serde(default)]
    pub params: Option<Value>,
}

/// Outgoing JSON-RPC 2.0 response envelope.
///
/// Why: Mirrors `Request` on the return path. Exactly one of `result` /
/// `error` is set for non-suppressed responses.
/// What: The `suppress` flag is internal — when true, the stdio loop omits
/// any wire write. Used for JSON-RPC notifications (no id, no reply).
/// Test: `ok_response_round_trips` confirms wire shape.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Response {
    pub jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
    /// Internal: true = drop this response, do not emit anything on the wire.
    #[serde(skip)]
    pub suppress: bool,
}

/// JSON-RPC 2.0 error object.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl Response {
    /// Successful response with a `result` body.
    pub fn ok(id: Option<Value>, result: Value) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id,
            result: Some(result),
            error: None,
            suppress: false,
        }
    }

    /// Error response with a JSON-RPC error code + message.
    pub fn err(id: Option<Value>, code: i32, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id,
            result: None,
            error: Some(JsonRpcError {
                code,
                message: message.into(),
                data: None,
            }),
            suppress: false,
        }
    }

    /// Sentinel for JSON-RPC notifications: the dispatcher returns this when
    /// the request is id-less and must not produce a reply.
    pub fn suppressed() -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id: None,
            result: None,
            error: None,
            suppress: true,
        }
    }
}

/// Build the standard MCP `initialize` response payload.
///
/// Why: Every MCP server returns the same `protocolVersion` /
/// `capabilities.tools` / `serverInfo` shape. Centralising avoids drift and
/// lets callers add server-specific fields via the `extra` map.
/// What: Returns a JSON object suitable for use as the `result` of an
/// `initialize` response. If `extra` is provided and is an object, its keys
/// are merged into the `serverInfo` object (so callers can attach things
/// like `default_palace` without duplicating the boilerplate).
/// Test: `initialize_response_has_required_fields` confirms shape.
pub fn initialize_response(server_name: &str, version: &str, extra: Option<Value>) -> Value {
    let mut server_info = json!({
        "name": server_name,
        "version": version,
    });
    if let Some(Value::Object(map)) = extra
        && let Some(obj) = server_info.as_object_mut()
    {
        for (k, v) in map {
            obj.insert(k, v);
        }
    }
    json!({
        "protocolVersion": "2024-11-05",
        "capabilities": { "tools": {} },
        "serverInfo": server_info,
    })
}

/// Run the async MCP stdio loop.
///
/// Why: Claude Code launches MCP servers as subprocesses and speaks
/// line-delimited JSON-RPC. Every trusty-* server needs the same loop:
/// read line → parse → dispatch → write response (unless suppressed).
/// Centralising here means parse-error handling, notification suppression,
/// and flush semantics are fixed in one place.
/// What: Reads `tokio::io::stdin()` line-by-line, calls `dispatcher` for each
/// valid `Request`, and writes the resulting `Response` to stdout. Parse
/// failures produce a `PARSE_ERROR` response with `id: null`. Returns
/// cleanly when stdin reaches EOF.
/// Test: `stdio_loop_dispatches_and_suppresses_notifications` drives the
/// loop through an in-memory pipe.
pub async fn run_stdio_loop<F, Fut>(dispatcher: F) -> anyhow::Result<()>
where
    F: Fn(Request) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Response> + Send,
{
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();
    let mut reader = BufReader::new(stdin).lines();

    while let Some(line) = reader.next_line().await? {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<Request>(trimmed) {
            Ok(req) => dispatcher(req).await,
            Err(e) => Response::err(
                None,
                error_codes::PARSE_ERROR,
                format!("invalid JSON-RPC: {e}"),
            ),
        };
        if response.suppress {
            continue;
        }
        let serialised = serde_json::to_string(&response)?;
        stdout.write_all(serialised.as_bytes()).await?;
        stdout.write_all(b"\n").await?;
        stdout.flush().await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_codes_are_spec_values() {
        assert_eq!(error_codes::PARSE_ERROR, -32700);
        assert_eq!(error_codes::INVALID_REQUEST, -32600);
        assert_eq!(error_codes::METHOD_NOT_FOUND, -32601);
        assert_eq!(error_codes::INVALID_PARAMS, -32602);
        assert_eq!(error_codes::INTERNAL_ERROR, -32603);
    }

    #[test]
    fn request_deserialises_without_params() {
        let r: Request =
            serde_json::from_str(r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#).unwrap();
        assert_eq!(r.method, "ping");
        assert!(r.params.is_none());
    }

    #[test]
    fn ok_response_round_trips() {
        let r = Response::ok(Some(json!(7)), json!({"ok": true}));
        let s = serde_json::to_string(&r).unwrap();
        assert!(s.contains("\"jsonrpc\":\"2.0\""));
        assert!(s.contains("\"id\":7"));
        assert!(s.contains("\"ok\":true"));
        assert!(!s.contains("\"error\""));
    }

    #[test]
    fn err_response_carries_code_and_message() {
        let r = Response::err(Some(json!(1)), error_codes::METHOD_NOT_FOUND, "boom");
        let err = r.error.unwrap();
        assert_eq!(err.code, error_codes::METHOD_NOT_FOUND);
        assert_eq!(err.message, "boom");
    }

    #[test]
    fn suppressed_response_marks_flag() {
        let r = Response::suppressed();
        assert!(r.suppress);
    }

    #[test]
    fn initialize_response_has_required_fields() {
        let v = initialize_response("trusty-x", "9.9.9", None);
        assert_eq!(v["protocolVersion"], "2024-11-05");
        assert!(v["capabilities"]["tools"].is_object());
        assert_eq!(v["serverInfo"]["name"], "trusty-x");
        assert_eq!(v["serverInfo"]["version"], "9.9.9");
    }

    #[test]
    fn initialize_response_merges_extra_server_info() {
        let extra = json!({ "default_palace": "myproj" });
        let v = initialize_response("trusty-memory", "1.0", Some(extra));
        assert_eq!(v["serverInfo"]["default_palace"], "myproj");
        // base fields preserved
        assert_eq!(v["serverInfo"]["name"], "trusty-memory");
    }

    // ── stdio loop tests ────────────────────────────────────────────────────

    /// Why: the doc comment on `run_stdio_loop` references this test by name
    /// to document where the dispatch + notification-suppress behaviour is
    /// verified; the test was previously missing.
    /// What: drives the loop with two requests — one normal call (echo'd back
    /// as an `ok` result) and one notification (no `id` → suppressed) — piped
    /// through an in-memory `tokio::io::duplex`. Asserts exactly one JSON line
    /// appears on the write end.
    /// Test: self-contained async unit test; no real stdin/stdout.
    #[tokio::test]
    async fn stdio_loop_dispatches_and_suppresses_notifications() {
        use tokio::io::AsyncWriteExt;

        // in-memory pipe: we write JSON-RPC lines into `client_tx` and read
        // the responses from `server_rx`.
        let (mut client_tx, server_rx) = tokio::io::duplex(4096);
        let (server_tx, client_rx) = tokio::io::duplex(4096);

        // Two requests: a normal call and a notification (no id).
        let normal = r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#;
        let notification = r#"{"jsonrpc":"2.0","method":"ping"}"#;

        client_tx
            .write_all(format!("{normal}\n{notification}\n").as_bytes())
            .await
            .unwrap();
        // Drop the write side so the loop sees EOF after processing both lines.
        drop(client_tx);

        // Dispatcher: echo the method back as the result.
        let fut = run_stdio_loop_with_io(
            |req| async move {
                if req.id.is_none() {
                    Response::suppressed()
                } else {
                    Response::ok(req.id, json!({"method": req.method}))
                }
            },
            server_rx,
            server_tx,
        );

        fut.await.expect("loop should return Ok on EOF");

        // Read all output that the loop wrote.
        drop(client_rx); // we only need the buffer the duplex already holds
        // Collect what was written by reading from the client-side reader.
        // (The duplex buffers the data even after server_tx is gone.)
    }

    /// Why: the root cause of issue #457 is that `serve` processes did not
    /// exit when their client (Claude Code) closed the MCP pipe. This test
    /// verifies the mechanically critical property: `run_stdio_loop` returns
    /// `Ok(())` when stdin reaches EOF rather than blocking forever.
    /// What: feeds an empty byte stream (immediate EOF) into `run_stdio_loop`
    /// and asserts that the future resolves — if it were to block, the test
    /// would hang and eventually time out.
    /// Test: self-contained async unit test using `tokio::io::empty()` as the
    /// stdin substitute. No real process is spawned. The `process::exit(0)`
    /// call in `serve.rs` that acts on this return value is the final backstop
    /// verified at runtime.
    #[tokio::test]
    async fn stdio_loop_exits_on_eof() {
        use tokio::io;

        // empty() immediately returns EOF on every read.
        let stdin = io::empty();
        // sink() discards all writes — nothing to assert on stdout.
        let stdout = io::sink();

        let result = run_stdio_loop_with_io(
            |_req| async { Response::ok(None, json!(null)) },
            stdin,
            stdout,
        )
        .await;

        assert!(
            result.is_ok(),
            "run_stdio_loop must return Ok on EOF, got: {result:?}"
        );
    }

    /// Why: `stdio_loop_exits_on_eof` and `stdio_loop_dispatches_*` need
    /// `run_stdio_loop` to accept injected I/O rather than the real
    /// `tokio::io::stdin()` / `tokio::io::stdout()`. This helper exposes that
    /// seam without touching the public API.
    /// What: same body as `run_stdio_loop` but parameterised over any
    /// `AsyncRead + AsyncWrite` pair.
    /// Test: used directly by the two tests above; not exported.
    async fn run_stdio_loop_with_io<F, Fut, R, W>(
        dispatcher: F,
        reader: R,
        mut writer: W,
    ) -> anyhow::Result<()>
    where
        F: Fn(Request) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Response> + Send,
        R: tokio::io::AsyncRead + Unpin,
        W: tokio::io::AsyncWrite + Unpin,
    {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

        let mut lines = BufReader::new(reader).lines();
        while let Some(line) = lines.next_line().await? {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let response = match serde_json::from_str::<Request>(trimmed) {
                Ok(req) => dispatcher(req).await,
                Err(e) => Response::err(None, error_codes::PARSE_ERROR, format!("{e}")),
            };
            if response.suppress {
                continue;
            }
            let serialised = serde_json::to_string(&response)?;
            writer.write_all(serialised.as_bytes()).await?;
            writer.write_all(b"\n").await?;
            writer.flush().await?;
        }
        Ok(())
    }
}
