//! Line-delimited JSON-RPC 2.0 over stdin/stdout.
//!
//! Why: MCP clients (Claude Code, Inspector) launch the server as a subprocess
//! and exchange one JSON object per line. Notification responses are silently
//! dropped. Parse errors are reported with id=null per the JSON-RPC spec.

use anyhow::Result;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use super::{error_codes, AnalyzerMcpServer, JsonRpcError, Request, Response};

/// Default maximum response size in bytes before the size guard activates.
///
/// Why: MCP hosts (e.g. Claude Code) terminate the session with a `-32000`
/// error when a single stdio line exceeds their internal buffer limit (observed
/// experimentally at ~2–4 MB). 2 MB is a conservative, safe ceiling.
/// What: read by `response_size_ceiling()` and used in `guard_response_size`.
/// Test: `stdio_size_guard_truncates_oversized_response` below.
const DEFAULT_MAX_RESPONSE_BYTES: usize = 2_000_000;

/// Return the effective maximum response byte count.
///
/// Why: allows operators to tune the ceiling via `TRUSTY_MCP_MAX_RESPONSE_BYTES`;
/// set `TRUSTY_MCP_MAX_RESPONSE_BYTES` before launching the process — it is read
/// once at startup. Absent or unparseable values fall back to the default.
/// What: reads `TRUSTY_MCP_MAX_RESPONSE_BYTES` from the process environment;
/// returns a parsed `usize` on success, `DEFAULT_MAX_RESPONSE_BYTES` otherwise.
/// Test: override the env var in a test process and call this function.
fn response_size_ceiling() -> usize {
    std::env::var("TRUSTY_MCP_MAX_RESPONSE_BYTES")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(DEFAULT_MAX_RESPONSE_BYTES)
}

/// Replace an oversized response bytes vec with a truncation-notice payload.
///
/// Why: a single stdio line larger than the MCP host's buffer limit causes a
/// `-32000` session-killing disconnect. Replacing the payload with a small,
/// in-band error notice keeps the session alive and tells the caller to paginate
/// instead. Per JSON-RPC 2.0 §5 the response must echo the request `id`, so we
/// parse it back out of the already-serialised bytes rather than hardcoding null.
/// What: if `bytes.len() > ceiling`, parses `id` from the bytes (falls back to
/// `null` if the bytes are not valid JSON or carry no `id`), serialises a
/// `{ isError: true, content: [{ type: "text", text: "Response truncated: …" }] }`
/// object with the echoed `id`, and returns it as the new bytes vec (with a
/// trailing newline). If the payload is within the limit, returns `bytes`
/// unchanged.
/// Test: `stdio_size_guard_truncates_oversized_response` and
/// `stdio_size_guard_echoes_request_id` below.
pub fn guard_response_size(bytes: Vec<u8>, ceiling: usize) -> Vec<u8> {
    if bytes.len() <= ceiling {
        return bytes;
    }
    let n = bytes.len();
    tracing::warn!(
        response_bytes = n,
        ceiling,
        "MCP response exceeds stdio size ceiling — replacing with truncation notice"
    );
    // Parse the id back out so the truncation notice is a valid JSON-RPC response.
    let id = serde_json::from_slice::<serde_json::Value>(&bytes)
        .ok()
        .and_then(|v| v.get("id").cloned())
        .unwrap_or(serde_json::Value::Null);
    let notice = serde_json::json!({
        "result": {
            "isError": true,
            "content": [{
                "type": "text",
                "text": format!(
                    "Response truncated: {n} bytes exceeded limit {ceiling}. \
                     Use limit/offset pagination to retrieve results in smaller pages."
                ),
            }]
        },
        "jsonrpc": "2.0",
        "id": id,
    });
    let mut out = serde_json::to_vec(&notice).unwrap_or_else(|_| b"{}".to_vec());
    out.push(b'\n');
    out
}

/// Run the stdio loop until stdin closes. Each line is parsed as a JSON-RPC
/// request, dispatched, and the (optional) response written back.
///
/// Why: MCP clients (Claude Code, Inspector) launch the server as a subprocess
/// and exchange one JSON object per line. Notification responses are silently
/// dropped. Parse errors are reported with id=null per the JSON-RPC spec.
/// What: reads newline-delimited JSON from stdin, dispatches each request, and
/// writes the response to stdout. Applies `guard_response_size` before each
/// write to prevent session-killing oversized payloads (#917).
/// Test: the size guard is unit-tested via `guard_response_size` directly.
pub async fn run(server: AnalyzerMcpServer) -> Result<()> {
    let stdin = tokio::io::stdin();
    let mut reader = BufReader::new(stdin).lines();
    let mut stdout = tokio::io::stdout();
    let ceiling = response_size_ceiling();

    while let Some(line) = reader.next_line().await? {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let resp: Response = match serde_json::from_str::<Request>(trimmed) {
            Ok(req) => server.dispatch(req).await,
            Err(e) => Response {
                jsonrpc: "2.0".into(),
                id: serde_json::Value::Null,
                result: None,
                error: Some(JsonRpcError {
                    code: error_codes::INVALID_REQUEST,
                    message: format!("parse error: {e}"),
                    data: None,
                }),
                suppress: false,
            },
        };
        if resp.suppress {
            continue;
        }
        let mut bytes = serde_json::to_vec(&resp)?;
        bytes.push(b'\n');
        let bytes = guard_response_size(bytes, ceiling);
        stdout.write_all(&bytes).await?;
        stdout.flush().await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: a payload at or below the ceiling must pass through unchanged so
    /// normal (small) responses are not affected by the guard.
    /// What: calls `guard_response_size` with `bytes.len() == ceiling`; asserts
    /// the returned vec is identical to the input.
    /// Test: this test.
    #[test]
    fn stdio_size_guard_passes_within_limit() {
        let payload = b"hello world\n".to_vec();
        let ceiling = payload.len(); // exactly at ceiling
        let out = guard_response_size(payload.clone(), ceiling);
        assert_eq!(out, payload, "payload within limit must be unchanged");
    }

    /// Why: a payload exceeding the ceiling must be replaced with a truncation
    /// notice so the MCP session survives instead of dying with `-32000`.
    /// What: constructs a synthetic large payload, calls `guard_response_size`
    /// with a smaller ceiling, and asserts the result contains the truncation
    /// message rather than the original content.
    /// Test: this test (pure function — no I/O).
    #[test]
    fn stdio_size_guard_truncates_oversized_response() {
        let large = vec![b'x'; 3_000_000]; // 3 MB
        let ceiling = 2_000_000usize;
        let out = guard_response_size(large, ceiling);
        let text = std::str::from_utf8(&out).expect("utf8");
        assert!(
            text.contains("Response truncated"),
            "truncation notice expected, got: {text:.200}"
        );
        assert!(
            text.contains("3000000"),
            "original size expected in notice, got: {text:.200}"
        );
        assert!(out.len() < ceiling, "notice must be smaller than ceiling");
    }

    /// Why: JSON-RPC 2.0 §5 requires a response to echo the request `id`. A
    /// truncation notice with `"id": null` breaks clients that match responses to
    /// requests by id (e.g. multiplexed MCP sessions).
    /// What: constructs a synthetic large-payload JSON object with `"id": 42`,
    /// calls `guard_response_size`, and asserts the truncation notice carries
    /// `"id": 42` rather than null.
    /// Test: this test.
    #[test]
    fn stdio_size_guard_echoes_request_id() {
        // Build a large-enough response JSON that carries a non-null id.
        let large_response = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 42,
            "result": { "data": "x".repeat(3_000_000) },
        });
        let bytes = serde_json::to_vec(&large_response).unwrap();
        assert!(
            bytes.len() > 2_000_000,
            "pre-condition: bytes must exceed ceiling"
        );
        let out = guard_response_size(bytes, 2_000_000);
        let trimmed = out.trim_ascii_end();
        let v: serde_json::Value =
            serde_json::from_slice(trimmed).expect("truncation notice must be valid JSON");
        assert_eq!(
            v["id"],
            serde_json::Value::from(42i64),
            "truncation notice must echo the request id"
        );
        assert!(v["result"]["isError"].as_bool().unwrap_or(false));
    }

    /// Why: the truncation notice must itself be valid JSON so the MCP host can
    /// parse it without error.
    /// What: triggers the guard and deserialises the output as `serde_json::Value`.
    /// Test: this test.
    #[test]
    fn stdio_size_guard_notice_is_valid_json() {
        let large = vec![b'x'; 3_000_000];
        let out = guard_response_size(large, 2_000_000);
        // Strip trailing newline before parsing.
        let trimmed = out.trim_ascii_end();
        let v: serde_json::Value =
            serde_json::from_slice(trimmed).expect("truncation notice must be valid JSON");
        assert_eq!(v["jsonrpc"], "2.0");
        assert_eq!(v["result"]["isError"], true);
    }
}
