//! Minimal JSON-RPC 2.0 stdio loop for the `tickets-mcp` binary.
//!
//! Why: `trusty-mcp` owns the shared stdio loop every other trusty-* MCP
//! server uses, and it must depend on `trusty-common` (feature `uds`) to
//! forward over a socket. `trusty-common` borrowing that loop back closed a
//! dependency cycle the moment the `tickets` feature was on, which blocked
//! [#6316](https://github.com/bobmatnyc/trusty-tools/issues/6316)'s shared
//! `daemon_bridge_json_rpc`. This module is the base crate's own copy so the
//! edge can be cut without moving the `tickets-mcp` binary out of this crate
//! (that move is #6316 slice 4, `trusty-mcp <service>`).
//! What: The four primitives `tickets::server` actually used — `error_codes`,
//! `Request`, `Response`, `initialize_response` — plus a line-delimited
//! read-dispatch-write loop over stdin/stdout. Deliberately narrower than
//! `trusty_mcp`'s: no `data` field on errors, no `extra` server-info merge, no
//! `INVALID_PARAMS`/`INVALID_REQUEST` constants, because nothing here uses
//! them. Adds no dependency — `serde`, `serde_json`, `anyhow` and `tokio`
//! (`io-std`, `io-util`) are already unconditional in this crate.
//! Test: `initialize_response_has_required_fields`,
//! `error_codes_are_spec_values`, `request_deserialises_without_params`,
//! `ok_response_round_trips`, `err_response_carries_code_and_message`,
//! `stdio_loop_dispatches_and_suppresses_notifications`,
//! `stdio_loop_reports_parse_errors`, `stdio_loop_exits_on_eof`.
//!
//! Not the shared loop. A second MCP server in this crate is a signal to move
//! the binary out to `trusty-mcp` rather than to widen this module.

// #6316: trusty-common must not depend on trusty-mcp (cycle)

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};

/// The JSON-RPC 2.0 error codes this server returns.
///
/// Why: The spec's reserved range, kept numerically identical to
/// `trusty_mcp::error_codes` so a client cannot tell the two loops apart.
/// What: `i32` to match the spec; serde_json emits them as JSON numbers.
/// Test: `error_codes_are_spec_values`.
pub(crate) mod error_codes {
    pub(crate) const PARSE_ERROR: i32 = -32700;
    pub(crate) const METHOD_NOT_FOUND: i32 = -32601;
    pub(crate) const INTERNAL_ERROR: i32 = -32603;
}

/// Incoming JSON-RPC 2.0 request envelope.
///
/// Why: The dispatcher re-serialises this to a `Value` before matching on
/// `method`, so it must round-trip through serde in both directions.
/// What: `id` is absent for notifications; `params` defaults to `None` so a
/// caller that omits it still parses.
/// Test: `request_deserialises_without_params`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub(crate) struct Request {
    /// Always `"2.0"` in practice. Optional so a legacy caller that omits it
    /// reaches the dispatcher instead of failing at the parse layer.
    #[serde(default)]
    pub(crate) jsonrpc: Option<String>,
    #[serde(default)]
    pub(crate) id: Option<Value>,
    pub(crate) method: String,
    #[serde(default)]
    pub(crate) params: Option<Value>,
}

/// Outgoing JSON-RPC 2.0 response envelope.
///
/// Why: Mirrors `Request` on the return path; exactly one of `result` /
/// `error` is set on any response that reaches the wire.
/// What: `suppress` never serialises — it tells the loop to write nothing at
/// all, which is how notifications are answered.
/// Test: `ok_response_round_trips`, `err_response_carries_code_and_message`.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct Response {
    pub(crate) jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) error: Option<JsonRpcError>,
    /// Internal: true = drop this response, emit nothing on the wire.
    #[serde(skip)]
    pub(crate) suppress: bool,
}

/// JSON-RPC 2.0 error object.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct JsonRpcError {
    pub(crate) code: i32,
    pub(crate) message: String,
}

impl Response {
    /// Successful response carrying a `result` body.
    pub(crate) fn ok(id: Option<Value>, result: Value) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id,
            result: Some(result),
            error: None,
            suppress: false,
        }
    }

    /// Error response carrying a JSON-RPC code and message.
    pub(crate) fn err(id: Option<Value>, code: i32, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id,
            result: None,
            error: Some(JsonRpcError {
                code,
                message: message.into(),
            }),
            suppress: false,
        }
    }

    /// A response the loop must not write — the reply to a notification.
    pub(crate) fn suppressed() -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id: None,
            result: None,
            error: None,
            suppress: true,
        }
    }
}

/// Build the `initialize` result payload.
///
/// Why: MCP hosts read `protocolVersion` and `serverInfo` from the handshake
/// and refuse the session without them.
/// What: Pins the same `2024-11-05` protocol revision and `tools`-only
/// capability set the rest of the trusty-* family advertises.
/// Test: `initialize_response_has_required_fields`.
pub(crate) fn initialize_response(server_name: &str, version: &str) -> Value {
    json!({
        "protocolVersion": "2024-11-05",
        "capabilities": { "tools": {} },
        "serverInfo": { "name": server_name, "version": version },
    })
}

/// Read line-delimited JSON-RPC from stdin, dispatch, write replies to stdout.
///
/// Why: The transport half of `tickets::server::run_stdio`; an MCP host
/// launches the binary and speaks newline-framed JSON-RPC over the pipe.
/// What: Blank lines are skipped, unparseable lines answer `PARSE_ERROR` with
/// a null id, suppressed responses write nothing, and EOF returns `Ok`.
/// Test: `stdio_loop_dispatches_and_suppresses_notifications`,
/// `stdio_loop_reports_parse_errors`, `stdio_loop_exits_on_eof`.
pub(crate) async fn run_stdio_loop<F, Fut>(dispatcher: F) -> anyhow::Result<()>
where
    F: Fn(Request) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Response> + Send,
{
    run_stdio_loop_with_io(dispatcher, tokio::io::stdin(), tokio::io::stdout()).await
}

/// `run_stdio_loop` with the streams injected, so tests drive it over a pipe.
///
/// Why: stdin/stdout are process-global; a test that used them would fight
/// every other test in the binary.
/// What: Identical logic to `run_stdio_loop`, generic over the two streams.
/// Test: the three `stdio_loop_*` tests call this directly.
async fn run_stdio_loop_with_io<F, Fut, R, W>(
    dispatcher: F,
    reader: R,
    mut writer: W,
) -> anyhow::Result<()>
where
    F: Fn(Request) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Response> + Send,
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut lines = BufReader::new(reader).lines();
    while let Some(line) = lines.next_line().await? {
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
        writer.write_all(serialised.as_bytes()).await?;
        writer.write_all(b"\n").await?;
        writer.flush().await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drive the loop over an in-memory pipe and return everything it wrote.
    async fn drive(input: &str) -> String {
        let (mut client_tx, server_rx) = tokio::io::duplex(8192);
        client_tx.write_all(input.as_bytes()).await.unwrap();
        drop(client_tx);

        let mut out: Vec<u8> = Vec::new();
        run_stdio_loop_with_io(
            |req: Request| async move {
                if req.id.is_none() {
                    Response::suppressed()
                } else {
                    Response::ok(req.id, json!({ "method": req.method }))
                }
            },
            server_rx,
            &mut out,
        )
        .await
        .expect("loop returns Ok on EOF");
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn error_codes_are_spec_values() {
        assert_eq!(error_codes::PARSE_ERROR, -32700);
        assert_eq!(error_codes::METHOD_NOT_FOUND, -32601);
        assert_eq!(error_codes::INTERNAL_ERROR, -32603);
    }

    #[test]
    fn request_deserialises_without_params() {
        let r: Request =
            serde_json::from_str(r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#).unwrap();
        assert_eq!(r.method, "ping");
        assert!(r.params.is_none());
        assert_eq!(r.jsonrpc.as_deref(), Some("2.0"));
    }

    #[test]
    fn ok_response_round_trips() {
        let s =
            serde_json::to_string(&Response::ok(Some(json!(7)), json!({ "ok": true }))).unwrap();
        assert!(s.contains(r#""jsonrpc":"2.0""#));
        assert!(s.contains(r#""id":7"#));
        assert!(s.contains(r#""ok":true"#));
        assert!(!s.contains("error"));
    }

    #[test]
    fn err_response_carries_code_and_message() {
        let r = Response::err(Some(json!(1)), error_codes::METHOD_NOT_FOUND, "boom");
        let err = r.error.as_ref().unwrap();
        assert_eq!(err.code, error_codes::METHOD_NOT_FOUND);
        assert_eq!(err.message, "boom");
        assert!(r.result.is_none());
    }

    #[test]
    fn initialize_response_has_required_fields() {
        let v = initialize_response("tickets-mcp", "9.9.9");
        assert_eq!(v["protocolVersion"], "2024-11-05");
        assert!(v["capabilities"]["tools"].is_object());
        assert_eq!(v["serverInfo"]["name"], "tickets-mcp");
        assert_eq!(v["serverInfo"]["version"], "9.9.9");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn stdio_loop_dispatches_and_suppresses_notifications() {
        let out = drive(concat!(
            r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#,
            "\n\n",
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            "\n",
        ))
        .await;
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 1, "notification must not be answered: {out:?}");
        let v: Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(v["id"], 1);
        assert_eq!(v["result"]["method"], "ping");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn stdio_loop_reports_parse_errors() {
        let out = drive("{not json}\n").await;
        let v: Value = serde_json::from_str(out.trim()).unwrap();
        assert_eq!(v["error"]["code"], error_codes::PARSE_ERROR);
        assert!(v.get("id").is_none(), "parse errors carry no id: {v}");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn stdio_loop_exits_on_eof() {
        assert_eq!(drive("").await, "");
    }
}
