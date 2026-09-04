//! The one call the MCP dispatcher makes against the analyzer daemon.
//!
//! Why: every MCP tool ultimately performs one request against the analyzer
//! daemon; the transport plumbing is identical error-handling boilerplate
//! lifted out of the dispatcher so `mcp/mod.rs` stays under the 500-SLOC
//! production cap (see #1195).
//!
//! #6287 replaced five verbs with one. This module used to expose
//! `get`/`post`/`post_text`/`post_bytes`/`delete`, because the daemon spoke
//! HTTP and the verb plus a URL path was the whole address of an operation.
//! ADR-0032 put the daemon on a Unix socket speaking JSON-RPC, where the
//! address is a method name and the arguments are one `params` object — so the
//! verb has nothing left to distinguish and the URL has nothing left to encode.
//! Every caller now names its method directly, which also removes the
//! percent-encoding the query-string builders needed.
//!
//! #6316 took the last of the transport out. [`AnalyzerMcpServer::call`] used
//! to dial [`trusty_common::uds::send_framed_request_capped`] itself and unpack
//! an `RpcResponse` by hand — a second copy of what trusty-memory's stdio
//! bridge was also doing. Both now run [`trusty_mcp::DaemonBridgeJsonRpc`], so
//! the framed exchange, the `jsonrpc` normalisation and the reply mapping have
//! exactly one implementation. This crate's stdio loop is unaffected: the
//! analyzer's MCP surface is a tool TRANSLATOR (its own `tools/list`, its own
//! per-tool argument validation), not an envelope forwarder, so
//! `mcp::stdio::run` keeps its dispatcher and its #917 response-size guard.
//!
//! What: [`AnalyzerMcpServer::call`] — one bridged exchange over the socket,
//! with transport failures and daemon-side JSON-RPC errors both mapped to
//! [`DispatchError::Transport`].
//!
//! Test: `a_dead_daemon_answers_the_request_that_caused_it` below; every
//! per-tool handler test in `tests.rs` asserts the method name surfaces in the
//! transport error when the daemon is down.

use super::{AnalyzerMcpServer, DispatchError};
use serde_json::Value;
use trusty_mcp::{DaemonBridgeJsonRpc, UdsBridgeConfig};

/// Largest RESPONSE frame the MCP client will buffer, in bytes.
///
/// Why: [`trusty_common::uds::MAX_FRAME_BYTES`] is 8 MiB, sized for
/// control-plane frames. `extract_graph` returns a whole-repository `KgGraph`
/// merged with any SCIP overlay, which is the mirror image of the
/// `analyze.scip_ingest` payload the daemon accepts — so the response budget
/// has to match the request budget, or the daemon would accept an index this
/// client could never read back.
/// What: 32 MiB, the same figure as `service::rpc::MAX_FRAME_BYTES`. The two
/// constants are deliberately equal and each documents the other; raising one
/// alone only moves which end refuses.
/// Test: `rpc_accepts_a_request_larger_than_the_shared_default` covers the
/// server half; this half is exercised by every live-socket tool test.
pub(super) const MAX_RESPONSE_FRAME_BYTES: u64 = 32 * 1024 * 1024;

impl AnalyzerMcpServer {
    /// The shared forwarder this dispatcher dials the daemon through (#6316).
    ///
    /// Why: built per call rather than stored, because `AnalyzerMcpServer` is
    /// `Clone` and a bridge is not — and the construction is one struct of
    /// scalars, cheaper than the connect it precedes.
    /// What: names the daemon for the error text, and carries the two budgets
    /// this crate owns: `core::mcp_client_timeout()` (which must clear both the
    /// OpenRouter ceiling `deep_analysis` can spend and the diagnostics
    /// deadline) and [`MAX_RESPONSE_FRAME_BYTES`]. No streaming list and no
    /// rewriter: every `analyze.*` method answers in one frame, and the
    /// dispatcher has already built the exact params the daemon expects.
    /// Test: `a_dead_daemon_answers_the_request_that_caused_it`.
    fn bridge(&self) -> DaemonBridgeJsonRpc {
        DaemonBridgeJsonRpc::new(
            UdsBridgeConfig::new(self.socket.clone(), "trusty-analyze")
                .with_request_timeout(crate::core::mcp_client_timeout())
                .with_max_frame_bytes(MAX_RESPONSE_FRAME_BYTES),
        )
    }

    /// Call one daemon method and return its `result` value.
    ///
    /// Why: the single point where an MCP tool becomes a wire request, so the
    /// envelope, the budget and the two failure shapes are stated once.
    /// What: hands the shared bridge a JSON-RPC 2.0 request and unwraps its
    /// answer.
    ///
    /// # Errors
    ///
    /// Both failure shapes report as [`DispatchError::Transport`], and the
    /// method name leads both messages — that is what the tool tests assert on,
    /// and what tells an operator which call failed:
    ///
    /// - the exchange itself failed (nothing is listening, the frame was too
    ///   large, the daemon hung up), or
    /// - the daemon answered with a JSON-RPC error frame.
    ///
    /// The second is not `InvalidParams` even when the code is `-32602`: the
    /// MCP surface validates its own arguments before it gets here, so a
    /// daemon-side `invalid_params` means the two ends disagree about a
    /// method's shape, which is a deployment skew rather than something the MCP
    /// caller typed wrongly.
    ///
    /// A response carrying neither `result` nor `error` is a malformed frame,
    /// not an empty answer: the bridge turns it into an error naming the
    /// daemon, so it arrives here as `Transport` rather than decoding to
    /// `null`.
    ///
    /// Test: `a_dead_daemon_answers_the_request_that_caused_it`.
    pub(super) async fn call(&self, method: &str, params: Value) -> Result<Value, DispatchError> {
        // #6316: the framed exchange now lives in trusty-mcp; this maps the
        // bridge's JSON-RPC answer onto the dispatcher's own error type.
        let response = self
            .bridge()
            .answer(trusty_mcp::Request {
                jsonrpc: Some("2.0".to_string()),
                id: Some(Value::from(1)),
                method: method.to_string(),
                params: Some(params),
            })
            .await;

        if let Some(error) = response.error {
            return Err(DispatchError::Transport(format!(
                "{method} over {}: {} ({})",
                self.socket.display(),
                error.message,
                error.code
            )));
        }

        response.result.ok_or_else(|| {
            DispatchError::Transport(format!(
                "{method} answered with neither a result nor an error"
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::{error_codes, Request, Response};

    /// Why (#6316 fail-open check): `ensure_daemon_running` can report a live
    /// daemon that is gone by the time a tool call reaches the wire — an
    /// upgrade, a `stop`, a crash. The dispatcher must answer that request with
    /// a JSON-RPC error carrying the request's own id, because a client matches
    /// a response to its call by id and an unmatched frame reads as a hang. An
    /// empty `result` would be worse still: the caller would treat "the daemon
    /// is gone" as "there is nothing to report".
    /// What: points the dispatcher at a socket nothing is serving and drives
    /// one direct-method request through `dispatch`; asserts an error carrying
    /// the id, no result, and the failing method named in the message.
    /// Test: itself.
    #[tokio::test]
    async fn a_dead_daemon_answers_the_request_that_caused_it() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let server = AnalyzerMcpServer::new(tmp.path().join("vanished.sock"));

        let resp: Response = server
            .dispatch(Request {
                jsonrpc: "2.0".to_string(),
                id: Some(Value::from(9)),
                method: "analyzer_health".to_string(),
                params: Value::Null,
            })
            .await;

        assert!(
            resp.result.is_none(),
            "a dead daemon is never an empty result"
        );
        let error = resp.error.expect("a dead daemon is an error");
        assert_eq!(error.code, error_codes::INTERNAL_ERROR);
        assert!(
            error.message.contains("analyze.health"),
            "the error must name the call that failed, got: {}",
            error.message
        );
        assert_eq!(
            resp.id,
            Value::from(9),
            "the error must carry the id of the request it answers"
        );
    }

    /// Why: the `tools/call` envelope reports a failure in-band as an MCP tool
    /// error rather than as a JSON-RPC error — that is the MCP contract — but
    /// it must still be a failure the caller can see, not an empty payload it
    /// would read as "nothing found".
    /// What: drives the same dead socket through `tools/call`; asserts the
    /// result is an `isError` envelope naming the method.
    /// Test: itself.
    #[tokio::test]
    async fn a_dead_daemon_reports_is_error_through_tools_call() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let server = AnalyzerMcpServer::new(tmp.path().join("vanished.sock"));

        let resp = server
            .dispatch(Request {
                jsonrpc: "2.0".to_string(),
                id: Some(Value::from(4)),
                method: "tools/call".to_string(),
                params: serde_json::json!({"name": "analyzer_health", "arguments": {}}),
            })
            .await;

        let result = resp.result.expect("tools/call reports errors in-band");
        assert_eq!(result["isError"], true);
        assert!(
            result["content"][0]["text"]
                .as_str()
                .unwrap_or_default()
                .contains("analyze.health"),
            "the tool error must name the call that failed, got: {result}"
        );
    }
}
