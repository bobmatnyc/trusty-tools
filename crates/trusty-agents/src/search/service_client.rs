//! HTTP client for the search-as-a-service daemon (#374).
//!
//! Why: Tools and the REPL need a typed, ergonomic way to ask the daemon
//! for results without each caller reimplementing pid-file parsing,
//! retries, and JSON shaping. Encapsulating those concerns in one place
//! lets the rest of the codebase treat the daemon as just another
//! injectable backend behind the existing `SearchCodeTool` abstraction.
//! What: [`SearchDaemonClient`] reads `<project_root>/.trusty-agents/state/
//! search.pid`, talks to `http://127.0.0.1:<port>/search/...` over
//! `reqwest`, and exposes [`SearchDaemonClient::search`] +
//! [`SearchDaemonClient::is_running`]. [`SearchDaemonClient::
//! connect_if_running`] is a non-failing constructor — it returns
//! `None` when no daemon is up so callers can transparently fall back
//! to a local indexer.
//! Test: See `tests` in this module — pid-file discovery and a happy
//! path round-trip (`router_round_trip`) using `build_router` from
//! `service.rs`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use serde::Serialize;

use crate::search::indexer::CodeChunk;
use crate::search::service::{SearchDaemonState, read_pid_file};

/// Per-call timeout for daemon HTTP requests.
///
/// Why: Search must feel synchronous in a tool call; a 5-second budget
/// is generous enough for a cold query against a large index but short
/// enough that a hung daemon doesn't stall the whole agent loop.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// Client for talking to a running search daemon over HTTP.
///
/// Why: Holding a pre-built `reqwest::Client` plus the cached daemon
/// port avoids re-reading the pid file on every call and lets us reuse
/// the connection pool.
/// What: Three pieces of state — the underlying HTTP client, the
/// daemon's port, and the project root (kept so callers can ask
/// `is_running` without re-passing it).
/// Test: `connect_if_running_returns_none_when_no_pid_file`,
/// `router_round_trip`.
#[derive(Clone)]
pub struct SearchDaemonClient {
    client: reqwest::Client,
    port: u16,
    project_root: PathBuf,
}

impl SearchDaemonClient {
    /// Construct a client only when a daemon is observably running.
    ///
    /// Why: Callers (the auto-detecting `SearchCodeTool::new_auto`)
    /// want a "connect or fall back" semantic. Returning `Option`
    /// rather than `Result` makes that pattern one-liner clean.
    /// What: Reads the pid file under `project_root`, probes
    /// `/search/health` over a 500ms budget, returns `Some(client)`
    /// on success or `None` for any failure (missing pid file, dead
    /// pid, unhealthy probe).
    /// Test: `connect_if_running_returns_none_when_no_pid_file`.
    pub async fn connect_if_running(project_root: &Path) -> Option<Self> {
        let state = read_pid_file(project_root)?;
        let client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .ok()?;
        let probe = client
            .get(format!("http://127.0.0.1:{}/search/health", state.port))
            .timeout(Duration::from_millis(500))
            .send()
            .await
            .ok()?;
        if !probe.status().is_success() {
            return None;
        }
        Some(Self {
            client,
            port: state.port,
            project_root: project_root.to_path_buf(),
        })
    }

    /// Return the daemon's record (port + pid + started_at) if present.
    ///
    /// Why: Some callers want the metadata, not just a yes/no liveness
    /// answer (e.g. `/service status`-style display).
    pub fn daemon_state(&self) -> Option<SearchDaemonState> {
        read_pid_file(&self.project_root)
    }

    /// Re-probe `/search/health` with a 500ms budget.
    pub async fn is_running(&self) -> bool {
        let url = format!("http://127.0.0.1:{}/search/health", self.port);
        match self
            .client
            .get(&url)
            .timeout(Duration::from_millis(500))
            .send()
            .await
        {
            Ok(r) => r.status().is_success(),
            Err(_) => false,
        }
    }

    /// Submit a hybrid search query and return the daemon's chunks.
    ///
    /// Why: This is the hot path — the call `SearchCodeTool` makes from
    /// inside an agent loop. Returns raw `CodeChunk` so the tool layer
    /// can format hits identically to the local-indexer code path.
    /// What: POSTs `{query, top_k}` JSON to `/search/query` and
    /// deserializes the response array into `Vec<CodeChunk>`.
    pub async fn search(&self, query: &str, top_k: usize) -> Result<Vec<CodeChunk>> {
        #[derive(Serialize)]
        struct Body<'a> {
            query: &'a str,
            top_k: usize,
            /// Ask the daemon to run KG expansion (#376 B1).
            expand_graph: bool,
            /// Ask the daemon to return compact snippets (#376 C1).
            compact: bool,
        }
        let url = format!("http://127.0.0.1:{}/search/query", self.port);
        let resp = self
            .client
            .post(&url)
            .json(&Body {
                query,
                top_k,
                expand_graph: true,
                compact: true,
            })
            .send()
            .await
            .with_context(|| format!("POST {url}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!("search daemon returned {status}: {body}"));
        }
        let chunks: Vec<CodeChunk> = resp
            .json()
            .await
            .context("decoding /search/query response")?;
        Ok(chunks)
    }

    /// Ask the daemon to (re)index a single file.
    pub async fn index_file(&self, path: &Path) -> Result<usize> {
        #[derive(Serialize)]
        struct Body<'a> {
            path: &'a str,
        }
        let url = format!("http://127.0.0.1:{}/search/index-file", self.port);
        let resp = self
            .client
            .post(&url)
            .json(&Body {
                path: &path.display().to_string(),
            })
            .send()
            .await
            .with_context(|| format!("POST {url}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!("daemon returned {status}: {body}"));
        }
        let v: serde_json::Value = resp.json().await.context("decoding response")?;
        Ok(v.get("chunks").and_then(|n| n.as_u64()).unwrap_or(0) as usize)
    }

    /// Ask the daemon to drop all chunks for a path.
    pub async fn remove_file(&self, path: &Path) -> Result<usize> {
        #[derive(Serialize)]
        struct Body<'a> {
            path: &'a str,
        }
        let url = format!("http://127.0.0.1:{}/search/remove-file", self.port);
        let resp = self
            .client
            .post(&url)
            .json(&Body {
                path: &path.display().to_string(),
            })
            .send()
            .await
            .with_context(|| format!("POST {url}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!("daemon returned {status}: {body}"));
        }
        let v: serde_json::Value = resp.json().await.context("decoding response")?;
        Ok(v.get("removed").and_then(|n| n.as_u64()).unwrap_or(0) as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tempfile::TempDir;

    #[tokio::test]
    async fn connect_if_running_returns_none_when_no_pid_file() {
        let dir = TempDir::new().unwrap();
        assert!(
            SearchDaemonClient::connect_if_running(dir.path())
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn router_round_trip() {
        // Why: Exercises the full client → daemon path without spinning
        // up the real on-disk store. Mounts the same `build_router` the
        // daemon uses, writes a synthetic pid file pointing at our
        // bound port, then drives `connect_if_running` + `search`.
        use crate::memory::{Embedder, MemoryResult, MemoryStore, Segment};
        use crate::search::indexer::CodeIndexer;
        use crate::search::service::{
            SearchDaemonState, SearchState, build_router, write_pid_file,
        };
        use async_trait::async_trait;
        use chrono::Utc;
        use serde_json::Value;
        use std::collections::HashMap;
        use std::sync::Mutex as StdMutex;
        use tokio::sync::Mutex;

        struct MockStore {
            inner: StdMutex<HashMap<String, (Vec<f32>, Value)>>,
        }
        #[async_trait]
        impl MemoryStore for MockStore {
            async fn insert(
                &self,
                _: Segment,
                id: &str,
                v: &[f32],
                p: Value,
            ) -> anyhow::Result<()> {
                self.inner
                    .lock()
                    .unwrap()
                    .insert(id.into(), (v.to_vec(), p));
                Ok(())
            }
            async fn search(
                &self,
                _: Segment,
                _: &[f32],
                _: usize,
            ) -> anyhow::Result<Vec<MemoryResult>> {
                Ok(vec![])
            }
            async fn get(&self, _: Segment, _: &str) -> anyhow::Result<Option<Value>> {
                Ok(None)
            }
            async fn delete(&self, _: Segment, _: &str) -> anyhow::Result<()> {
                Ok(())
            }
        }
        struct MockEmbedder;
        impl Embedder for MockEmbedder {
            fn embed(&self, t: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
                Ok(t.iter().map(|s| vec![s.len() as f32; 8]).collect())
            }
            fn embed_single(&self, t: &str) -> anyhow::Result<Vec<f32>> {
                Ok(vec![t.len() as f32; 8])
            }
            fn dimension(&self) -> usize {
                8
            }
        }

        let dir = TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join(".trusty-agents").join("state")).unwrap();

        let store: Arc<dyn MemoryStore> = Arc::new(MockStore {
            inner: StdMutex::new(HashMap::new()),
        });
        let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder);
        let indexer = Arc::new(CodeIndexer::new(store, embedder));
        let state = SearchState {
            indexer,
            project_root: dir.path().to_path_buf(),
            reindex_in_flight: Arc::new(Mutex::new(false)),
        };
        let app = build_router(state);

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        // Tell the client where to find us.
        let pid_state = SearchDaemonState {
            pid: std::process::id(),
            started_at: Utc::now(),
            port,
            socket_path: dir.path().join("search.sock"),
        };
        write_pid_file(dir.path(), &pid_state).unwrap();

        // Give the listener a beat to come up.
        tokio::time::sleep(Duration::from_millis(50)).await;

        let client = SearchDaemonClient::connect_if_running(dir.path())
            .await
            .expect("daemon should look running");
        assert!(client.is_running().await);
        let hits = client.search("anything", 5).await.expect("search");
        assert!(hits.is_empty(), "mock store returns no hits");

        handle.abort();
    }
}
