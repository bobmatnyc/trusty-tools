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
//! What: [`AnalyzerMcpServer::call`] — one framed exchange over the socket,
//! with transport failures and daemon-side JSON-RPC errors both mapped to
//! [`DispatchError::Transport`].
//!
//! Test: exercised by every per-tool handler test in `tests.rs` (which asserts
//! the method name surfaces in the transport error when the daemon is down).

use super::{AnalyzerMcpServer, DispatchError};
use serde_json::Value;
use trusty_common::uds::server::RpcResponse;

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
    /// Call one daemon method and return its `result` value.
    ///
    /// Why: the single point where an MCP tool becomes a wire request, so the
    /// envelope, the budget and the two failure shapes are stated once.
    /// What: builds a JSON-RPC 2.0 request frame, sends it over the socket
    /// under [`crate::core::mcp_client_timeout`], and unwraps the response.
    ///
    /// Both failure shapes report as [`DispatchError::Transport`], and the
    /// method name is in both messages — that is what the tool tests assert on,
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
    /// not an empty answer, and reports as such rather than decoding to `null`.
    pub(super) async fn call(&self, method: &str, params: Value) -> Result<Value, DispatchError> {
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": method,
            "params": params,
        });
        let response: RpcResponse = trusty_common::uds::send_framed_request_capped(
            &self.socket,
            &request,
            crate::core::mcp_client_timeout(),
            MAX_RESPONSE_FRAME_BYTES,
        )
        .await
        .map_err(|e| {
            DispatchError::Transport(format!("{method} over {}: {e}", self.socket.display()))
        })?;

        if let Some(error) = response.error {
            return Err(DispatchError::Transport(format!(
                "{method} returned error {}: {}",
                error.code, error.message
            )));
        }
        response.result.ok_or_else(|| {
            DispatchError::Transport(format!(
                "{method} answered with neither a result nor an error"
            ))
        })
    }
}
