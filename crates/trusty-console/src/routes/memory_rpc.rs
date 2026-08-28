//! One `tools/call` exchange with trusty-memory, for every route that needs one.
//!
//! Why: #6360 dialled `palace_delete` over the daemon's Unix socket and #6371
//! dials `palace_compact` the same way. The envelope, the framing, the timeout,
//! and the two ways the exchange fails before a tool ever runs are identical —
//! a second copy of them is how one route starts reading a JSON-RPC `error` as
//! a success while the other does not. This module owns the exchange; a caller
//! owns only what the tool's own payload has to say to count as confirmation.
//!
//! What: [`call_tool`] runs the exchange and returns the tool's OWN payload,
//! unwrapped from MCP's `content[0].text` block. A transport failure becomes
//! [`ActionVerdict::Unreachable`] and a JSON-RPC `error` becomes
//! [`ActionVerdict::Refused`], both carrying the daemon's words; neither is a
//! value the caller has to recognise for itself.
//!
//! `tools/call` is the envelope rather than a bare method name because that is
//! how trusty-memory's dispatcher routes these tools — `palace_delete` is in
//! neither its folded method table nor `TOOL_METHODS`, so a bare
//! `"method": "palace_delete"` answers `method_not_found`.
//!
//! Test: `call_tool_unwraps_the_tool_payload`,
//! `call_tool_reports_a_jsonrpc_error_as_a_refusal`,
//! `call_tool_reports_a_dead_socket_as_unreachable`.

use std::path::Path;

use serde_json::{Value, json};

use crate::routes::verdict::ActionVerdict;
use crate::routes::{ACTION_TIMEOUT, MEMORY_SERVICE};

/// Call one trusty-memory tool and return its own payload.
///
/// Why: see the module docs. `id` is carried only so a failure verdict names
/// the resource the caller was acting on.
/// What: one framed JSON-RPC `tools/call` on `socket`. `Ok` carries the tool's
/// payload — `Value::Null` when the answer had no readable payload, which every
/// caller must then treat as an unconfirmed answer rather than as a success.
/// Test: the `call_tool_*` tests below.
pub(crate) async fn call_tool(
    socket: &Path,
    tool: &str,
    arguments: Value,
    id: &str,
) -> Result<Value, ActionVerdict> {
    let request = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": { "name": tool, "arguments": arguments },
    });

    let sent =
        trusty_common::uds::send_framed_request::<_, trusty_common::uds::server::RpcResponse>(
            socket,
            &request,
            ACTION_TIMEOUT,
        )
        .await;

    let response = match sent {
        Ok(r) => r,
        Err(e) => {
            return Err(ActionVerdict::Unreachable {
                id: id.to_string(),
                reason: format!("{MEMORY_SERVICE} did not answer {tool}: {e}"),
            });
        }
    };

    if let Some(error) = response.error {
        return Err(ActionVerdict::Refused {
            id: id.to_string(),
            reason: format!(
                "{MEMORY_SERVICE} refused {tool} (code {}): {}",
                error.code, error.message
            ),
            detail: json!({ "code": error.code }),
        });
    }

    Ok(tool_payload(&response.result.unwrap_or(Value::Null)).unwrap_or(Value::Null))
}

/// Read the tool's own payload out of a `tools/call` result.
///
/// Why: `tools/call` wraps every tool result in MCP's `content[0].text` block,
/// so a tool's `{"deleted": "<id>"}` arrives as a JSON string inside a JSON
/// envelope. Unwrapping it is what turns "the daemon replied" into "the daemon
/// said this"; without it a successful `ping`-shaped reply would read as a
/// confirmed action.
/// What: `result.content[0].text`, parsed as JSON. `None` whenever any step is
/// absent or the wrong shape.
/// Test: `call_tool_unwraps_the_tool_payload`.
fn tool_payload(result: &Value) -> Option<Value> {
    let text = result
        .get("content")?
        .as_array()?
        .first()?
        .get("text")?
        .as_str()?;
    serde_json::from_str::<Value>(text).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Bind a socket that answers exactly one framed request with `reply`.
    fn stub_memory_daemon(dir: &Path, reply: impl Into<String>) -> PathBuf {
        let socket = dir.join("sockets").join("memory.sock");
        let reply = reply.into();
        let listener = trusty_common::uds::bind_hardened(&socket).expect("bind");
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
            let Ok((mut conn, _)) = listener.accept().await else {
                return;
            };
            let mut sink = Vec::new();
            let _ = conn.read_to_end(&mut sink).await;
            let _ = conn.write_all(reply.as_bytes()).await;
            let _ = conn.write_all(b"\n").await;
            let _ = conn.flush().await;
        });
        socket
    }

    /// Why: the caller decides what counts as confirmation, so it must be handed
    /// the tool's OWN object rather than the MCP envelope around it.
    /// Test: this is the test.
    #[tokio::test(flavor = "multi_thread")]
    async fn call_tool_unwraps_the_tool_payload() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let reply = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": { "content": [{ "type": "text", "text": r#"{"palace":"scratch","orphans_removed":4}"# }] },
        })
        .to_string();
        let socket = stub_memory_daemon(tmp.path(), reply);

        let payload = call_tool(&socket, "palace_compact", json!({}), "scratch")
            .await
            .expect("the exchange succeeds");
        assert_eq!(payload["palace"], json!("scratch"));
        assert_eq!(payload["orphans_removed"], json!(4));
    }

    /// Why: a JSON-RPC `error` is the daemon declining, and it must reach the
    /// operator as a refusal carrying the daemon's own message.
    /// Test: this is the test.
    #[tokio::test(flavor = "multi_thread")]
    async fn call_tool_reports_a_jsonrpc_error_as_a_refusal() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let socket = stub_memory_daemon(
            tmp.path(),
            r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32000,"message":"unknown palace 'scratch'"}}"#,
        );

        let verdict = call_tool(&socket, "palace_compact", json!({}), "scratch")
            .await
            .expect_err("a JSON-RPC error is not a success");
        assert!(
            matches!(&verdict, ActionVerdict::Refused { reason, .. } if reason.contains("unknown palace")),
            "the refusal must carry the daemon's words: {verdict:?}"
        );
    }

    /// Why: nothing listening is a different problem from a refusal, and must
    /// not be reported as one.
    /// Test: this is the test.
    #[tokio::test(flavor = "multi_thread")]
    async fn call_tool_reports_a_dead_socket_as_unreachable() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let verdict = call_tool(
            &tmp.path().join("absent.sock"),
            "palace_compact",
            json!({}),
            "scratch",
        )
        .await
        .expect_err("a dead socket is not a success");
        assert!(
            matches!(verdict, ActionVerdict::Unreachable { .. }),
            "a dead socket must read as unreachable: {verdict:?}"
        );
    }

    /// Why: an answer carrying no readable payload is an UNCONFIRMED answer, not
    /// a transport failure — the caller has to see `Null` and refuse it rather
    /// than get an `Err` that would read as an outage.
    /// Test: this is the test.
    #[tokio::test(flavor = "multi_thread")]
    async fn call_tool_returns_null_for_an_answer_with_no_payload() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let socket = stub_memory_daemon(tmp.path(), r#"{"jsonrpc":"2.0","id":1,"result":{}}"#);

        let payload = call_tool(&socket, "palace_compact", json!({}), "scratch")
            .await
            .expect("a payload-less answer is still an answer");
        assert_eq!(payload, Value::Null);
    }
}
