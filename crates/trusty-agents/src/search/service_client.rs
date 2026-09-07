//! Socket client for the search-as-a-service daemon (#374, UDS since #6433).
//!
//! Why: tools and the REPL need a typed way to ask the daemon for results
//! without each caller reimplementing the JSON-RPC envelope, the timeout and
//! the reply unwrapping. Encapsulating those concerns here lets the rest of the
//! codebase treat the daemon as just another injectable backend behind the
//! existing `SearchCodeTool` abstraction.
//! What: [`SearchDaemonClient`] derives the daemon's socket from the project
//! root, and calls the five `search.*` methods over it.
//! [`SearchDaemonClient::connect_if_running`] is a non-failing constructor — it
//! returns `None` when no daemon answers, so callers can transparently fall
//! back to a local indexer.
//!
//! #6433 replaced the HTTP half. This module used to read
//! `.trusty-agents/state/search.pid` for a port and build a `reqwest::Client`
//! per instance. There is no port and no discovery file now: caller and daemon
//! derive the same path from the same project root
//! ([`crate::search::service::search_socket_path`]), so there is nothing for a
//! stale file to disagree with, and the client holds a path rather than a
//! connection pool.
//!
//! Test: see `tests` in this module — the round trip drives every one of the
//! five methods against a real socket served by `service::rpc::build_router`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Result, anyhow};
use serde::Serialize;
use serde::de::DeserializeOwned;
use trusty_common::uds::server::RpcResponse;

use crate::search::indexer::CodeChunk;
use crate::search::service::rpc::{
    HealthResponse, IndexFileResponse, PathRequest, QueryRequest, ReindexResponse,
};
use crate::search::service::search_socket_path;

/// Per-call budget for daemon requests.
///
/// Why: search must feel synchronous in a tool call; a 5-second budget is
/// generous enough for a cold query against a large index but short enough that
/// a hung daemon doesn't stall the whole agent loop.
///
/// The budget is applied ONCE, by the shared client:
/// `trusty_common::uds::send_framed_request` wraps the whole dial-write-read
/// exchange in `tokio::time::timeout` itself. An outer timeout of the same
/// duration here could only lose that race.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// Shorter budget for the two liveness probes.
///
/// Why: `connect_if_running` runs on paths that fall back to a local indexer,
/// so the cost of a missing daemon is paid before any real work starts.
const PROBE_TIMEOUT: Duration = Duration::from_millis(500);

/// Client for talking to a running search daemon over its Unix socket.
///
/// Why: holding the derived socket path avoids recomputing it per call, and
/// keeps `is_running` answerable without re-passing the project root.
/// What: two paths — the daemon's socket and the project root it was derived
/// from.
/// Test: `connect_if_running_returns_none_when_no_daemon`,
/// `round_trip_reaches_every_method`.
#[derive(Clone)]
pub struct SearchDaemonClient {
    socket: PathBuf,
    project_root: PathBuf,
}

impl SearchDaemonClient {
    /// Construct a client only when a daemon is observably answering.
    ///
    /// Why: callers (the auto-detecting `SearchCodeTool::new_auto`) want a
    /// "connect or fall back" semantic. Returning `Option` rather than `Result`
    /// makes that pattern one line.
    /// What: derives the socket under `project_root`, calls `search.health`
    /// with a [`PROBE_TIMEOUT`] budget, and returns `Some` only when the daemon
    /// answered.
    /// Test: `connect_if_running_returns_none_when_no_daemon`,
    /// `round_trip_reaches_every_method`.
    pub async fn connect_if_running(project_root: &Path) -> Option<Self> {
        let client = Self {
            socket: search_socket_path(project_root),
            project_root: project_root.to_path_buf(),
        };
        client.is_running().await.then_some(client)
    }

    /// Build a client for `socket` without probing it.
    ///
    /// Why: a test serves a router on a temporary path rather than under the
    /// real home directory, and `connect_if_running` can only reach the derived
    /// one. Not a general-purpose constructor — a caller that has a project root
    /// wants `connect_if_running`, which also tells it whether anyone is there.
    /// Test: `round_trip_reaches_every_method`.
    #[cfg(test)]
    pub(crate) fn for_socket(socket: PathBuf, project_root: PathBuf) -> Self {
        Self {
            socket,
            project_root,
        }
    }

    /// The socket this client dials.
    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// The project root this client's socket was derived from.
    pub fn project_root(&self) -> &Path {
        &self.project_root
    }

    /// Re-probe `search.health`.
    ///
    /// Test: `round_trip_reaches_every_method`.
    pub async fn is_running(&self) -> bool {
        self.call::<_, HealthResponse>("search.health", serde_json::Value::Null, PROBE_TIMEOUT)
            .await
            .is_ok_and(|h| h.status == "ok")
    }

    /// Submit a hybrid search query and return the daemon's chunks.
    ///
    /// Why: this is the hot path — the call `SearchCodeTool` makes from inside
    /// an agent loop. Returns raw `CodeChunk` so the tool layer formats hits
    /// identically to the local-indexer code path.
    /// What: calls `search.query` asking for KG expansion (#376 B1) and compact
    /// snippets (#376 C1), the same two flags the HTTP client sent.
    ///
    /// # Errors
    ///
    /// When the exchange failed or the daemon answered a JSON-RPC error.
    ///
    /// Test: `round_trip_reaches_every_method`.
    pub async fn search(&self, query: &str, top_k: usize) -> Result<Vec<CodeChunk>> {
        self.call(
            "search.query",
            QueryRequest {
                query: query.to_string(),
                top_k,
                expand_graph: true,
                compact: true,
            },
            REQUEST_TIMEOUT,
        )
        .await
    }

    /// Ask the daemon to (re)index a single file. Returns the chunk count.
    ///
    /// # Errors
    ///
    /// As [`SearchDaemonClient::search`].
    ///
    /// Test: `round_trip_reaches_every_method`.
    pub async fn index_file(&self, path: &Path) -> Result<usize> {
        let resp: IndexFileResponse = self
            .call(
                "search.index_file",
                PathRequest {
                    path: path.display().to_string(),
                },
                REQUEST_TIMEOUT,
            )
            .await?;
        Ok(resp.chunks)
    }

    /// Ask the daemon to drop all chunks for a path. Returns how many went.
    ///
    /// # Errors
    ///
    /// As [`SearchDaemonClient::search`].
    ///
    /// Test: `round_trip_reaches_every_method`.
    pub async fn remove_file(&self, path: &Path) -> Result<usize> {
        let resp: IndexFileResponse = self
            .call(
                "search.remove_file",
                PathRequest {
                    path: path.display().to_string(),
                },
                REQUEST_TIMEOUT,
            )
            .await?;
        Ok(resp.chunks)
    }

    /// Ask the daemon to start a full reindex.
    ///
    /// Why it answers immediately: the walk runs for minutes and the daemon
    /// keeps serving queries off the old index while it does. The returned
    /// status distinguishes a walk this call started from one already in
    /// flight.
    ///
    /// # Errors
    ///
    /// As [`SearchDaemonClient::search`].
    ///
    /// Test: `round_trip_reaches_every_method`.
    pub async fn reindex(&self) -> Result<String> {
        let resp: ReindexResponse = self
            .call("search.reindex", serde_json::Value::Null, REQUEST_TIMEOUT)
            .await?;
        Ok(resp.status)
    }

    /// One framed JSON-RPC exchange with the daemon.
    ///
    /// Why one place: the envelope, the budget and the two failure shapes —
    /// the exchange failed, or the daemon answered a refusal — are stated once
    /// rather than five times.
    /// What: sends `{jsonrpc, id, method, params}` and unwraps `result`. The
    /// response budget is `trusty_common::uds::MAX_FRAME_BYTES`, the shared
    /// 8 MiB control-plane default, which is also what the daemon's
    /// `RpcServeOptions::default` reads a request with. `search.query` is the
    /// only method whose answer is bulk, and compact mode caps each hit at
    /// seven lines.
    ///
    /// # Errors
    ///
    /// A transport failure, a daemon-side error frame, or a response carrying
    /// neither `result` nor `error`. All three name the method, so an operator
    /// reading the message knows which call failed.
    async fn call<Req: Serialize, Resp: DeserializeOwned>(
        &self,
        method: &str,
        params: Req,
        timeout: Duration,
    ) -> Result<Resp> {
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": method,
            "params": params,
        });
        let response: RpcResponse =
            trusty_common::uds::send_framed_request(&self.socket, &request, timeout)
                .await
                .map_err(|e| anyhow!("{method} over {}: {e}", self.socket.display()))?;
        if let Some(error) = response.error {
            return Err(anyhow!(
                "{method} over {}: {} ({})",
                self.socket.display(),
                error.message,
                error.code
            ));
        }
        let result = response.result.ok_or_else(|| {
            anyhow!(
                "{method} over {}: response carried neither result nor error",
                self.socket.display()
            )
        })?;
        serde_json::from_value(result)
            .map_err(|e| anyhow!("{method}: decoding response failed: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::service::SearchState;
    use crate::search::service::rpc;
    use std::sync::Arc;
    use tempfile::TempDir;
    use tokio::sync::Mutex;

    /// A daemon that was never started answers nothing, and the client says so
    /// rather than constructing itself against a dead path.
    #[tokio::test]
    async fn connect_if_running_returns_none_when_no_daemon() {
        let dir = TempDir::new().unwrap();
        assert!(
            SearchDaemonClient::connect_if_running(dir.path())
                .await
                .is_none()
        );
    }

    /// Why: proves the client reaches ALL FIVE methods over the socket, which
    /// is the contract #6433 moved. The HTTP round-trip this replaces exercised
    /// two of them and left `index_file`, `remove_file` and `reindex` covered
    /// only by the router test on the daemon's own side.
    /// What: serves `rpc::build_router` over a real hardened socket in a temp
    /// directory, then drives health, query, index_file, remove_file and
    /// reindex through the public client methods.
    #[tokio::test]
    async fn round_trip_reaches_every_method() {
        let dir = TempDir::new().unwrap();
        let socket = dir.path().join("search.sock");
        let state = SearchState {
            indexer: Arc::new(crate::search::service::tests::mock_indexer()),
            project_root: dir.path().to_path_buf(),
            reindex_in_flight: Arc::new(Mutex::new(false)),
        };

        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
        let serve_socket = socket.clone();
        let server = tokio::spawn(async move {
            rpc::serve_with_shutdown(state, &serve_socket, async {
                let _ = stop_rx.await;
            })
            .await
        });
        crate::search::service::tests::await_socket(&socket).await;

        let client = SearchDaemonClient::for_socket(socket.clone(), dir.path().to_path_buf());

        assert!(client.is_running().await, "search.health");

        let hits = client.search("anything", 5).await.expect("search.query");
        assert!(hits.is_empty(), "mock store returns no hits");

        // The mock store accepts writes, so indexing a real file reports the
        // chunks it produced and removing it reports them gone.
        let source = dir.path().join("sample.rs");
        std::fs::write(&source, "fn alpha() {\n    let x = 1;\n}\n").unwrap();
        let indexed = client.index_file(&source).await.expect("search.index_file");
        assert!(indexed > 0, "expected at least one chunk, got {indexed}");
        let removed = client
            .remove_file(&source)
            .await
            .expect("search.remove_file");
        assert_eq!(removed, indexed, "removal should drop what indexing wrote");

        assert_eq!(
            client.reindex().await.expect("search.reindex"),
            "started",
            "first reindex should start a walk"
        );

        let _ = stop_tx.send(());
        server.await.unwrap().expect("serve");
    }

    /// A method the daemon does not serve is a named refusal, not a hang — and
    /// the error text carries the method so an operator can see which call was
    /// wrong.
    #[tokio::test]
    async fn an_unknown_method_reports_the_method_that_failed() {
        let dir = TempDir::new().unwrap();
        let socket = dir.path().join("search.sock");
        let state = SearchState {
            indexer: Arc::new(crate::search::service::tests::mock_indexer()),
            project_root: dir.path().to_path_buf(),
            reindex_in_flight: Arc::new(Mutex::new(false)),
        };
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
        let serve_socket = socket.clone();
        let server = tokio::spawn(async move {
            rpc::serve_with_shutdown(state, &serve_socket, async {
                let _ = stop_rx.await;
            })
            .await
        });
        crate::search::service::tests::await_socket(&socket).await;

        let client = SearchDaemonClient::for_socket(socket.clone(), dir.path().to_path_buf());
        let err = client
            .call::<_, serde_json::Value>(
                "search.nonexistent",
                serde_json::Value::Null,
                PROBE_TIMEOUT,
            )
            .await
            .expect_err("unknown method must fail");
        let text = err.to_string();
        assert!(text.contains("search.nonexistent"), "got: {text}");

        let _ = stop_tx.send(());
        server.await.unwrap().expect("serve");
    }
}
