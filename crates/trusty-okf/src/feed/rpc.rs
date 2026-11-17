//! The trusty-search socket transport for the feed (SPEC-OKF-01 §5.1).
//!
//! Why: the daemon speaks JSON-RPC 2.0 over a Unix socket (#9214); the one
//! client is `trusty_common::search_rpc::call_at`. The feed never calls an
//! HTTP route (§5.3).
//! What: [`RpcIndexFeed`] dials a socket path the caller passes (B3: no
//! path is derived here) and maps a failure to [`SearchError`], keeping the
//! daemon's code when it sent one. Unlike trusty-agents' `RpcIndexFeed`, it
//! returns every reply body, so the pass can check `indexed`,
//! `content_sha256` and `identity` (F4, D1). #10746 S7.
//! Test: `rpc_feed_round_trips_over_a_real_socket`,
//! `c43_daemon_down_is_not_fed_and_the_journal_is_unchanged`.

use std::path::PathBuf;
use std::time::Duration;

use serde_json::Value;

use super::{IndexFeed, SearchError};

/// Per-call timeout. A push chunks, embeds and commits inline, so it is
/// generous, but bounded: a wedged call fails its task and is retried by the
/// next pass.
pub const PUSH_TIMEOUT: Duration = Duration::from_secs(60);

/// The live feed over trusty-search's Unix socket.
#[derive(Debug, Clone)]
pub struct RpcIndexFeed {
    socket: PathBuf,
    timeout: Duration,
}

impl RpcIndexFeed {
    /// A feed that dials `socket`, with [`PUSH_TIMEOUT`] per call.
    pub fn new(socket: PathBuf) -> Self {
        Self {
            socket,
            timeout: PUSH_TIMEOUT,
        }
    }

    /// The socket this feed dials.
    pub fn socket(&self) -> &std::path::Path {
        &self.socket
    }
}

#[async_trait::async_trait]
impl IndexFeed for RpcIndexFeed {
    async fn call(&self, method: &str, params: Value) -> Result<Value, SearchError> {
        trusty_common::search_rpc::call_at(&self.socket, method, params, self.timeout)
            .await
            .map_err(|e| {
                let rpc = e.downcast_ref::<trusty_common::search_rpc::SearchRpcError>();
                SearchError {
                    method: method.to_string(),
                    code: rpc.map(|r| r.code),
                    message: rpc.map_or_else(|| format!("{e:#}"), |r| r.message.clone()),
                }
            })
    }
}
