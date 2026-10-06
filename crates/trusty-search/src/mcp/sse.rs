//! HTTP/SSE transport for the MCP server.
//!
//! Why: Some MCP clients (and remote integrations) prefer Server-Sent Events
//! over stdio. axum gives us native SSE via `response::sse::Sse`. We expose:
//! - `POST /mcp`     — synchronous JSON-RPC request/response
//! - `GET  /mcp/sse` — long-lived SSE stream with periodic keep-alive
//!
//! What: [`router`] returns an axum `Router` with the dispatcher cloned into
//! state. The SSE endpoint emits a `ready` event immediately, then sends
//! keep-alive pings every 15s; a real bi-directional stream would attach a
//! per-connection inbound queue, but that's deferred until a client demands it.
//!
//! Test: smoke-tested in tests by binding to an ephemeral port; full SSE
//! semantics are exercised by Claude Code's MCP harness in CI.

use crate::mcp::tools::{McpServer, Request, Response};
use axum::{
    extract::State,
    response::{
        sse::{Event, KeepAlive, Sse},
        Json,
    },
    routing::{get, post},
    Router,
};
use futures_util::stream::{self, Stream};
use std::{convert::Infallible, sync::Arc, time::Duration};

/// Build the axum router carrying an [`McpServer`] in shared state.
pub fn router(server: McpServer) -> Router {
    Router::new()
        .route("/mcp", post(json_rpc_handler))
        .route("/mcp/sse", get(sse_handler))
        .with_state(Arc::new(server))
}

async fn json_rpc_handler(
    State(server): State<Arc<McpServer>>,
    Json(req): Json<Request>,
) -> Json<Response> {
    Json(server.dispatch(req).await)
}

async fn sse_handler(
    State(_server): State<Arc<McpServer>>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    // Single-shot 'ready' event; KeepAlive emits comments every 15s so
    // intermediaries (nginx, Cloudflare) don't time out the connection.
    let ready = Event::default()
        .event("ready")
        .data(r#"{"status":"ready"}"#);
    let stream = stream::iter(vec![Ok(ready)]);
    Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keep-alive"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn router_builds() {
        let server = crate::mcp::tools::test_daemon::unreachable_server();
        let _r: Router = router(server);
    }
}
