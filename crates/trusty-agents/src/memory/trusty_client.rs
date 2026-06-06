//! HTTP client for an external `trusty-memory` daemon, plus a
//! `MemoryBackend` enum that picks between Trusty and the local
//! `RedbUsearchStore` at runtime.
//!
//! Why: trusty-agents ships an embedded memory store (redb + usearch) so it works
//! out of the box with no external services. When operators run the
//! `trusty-memory` daemon locally, however, they get a richer Palace/Room/
//! Drawer model that's worth using transparently. Auto-detection at startup
//! avoids forcing a config flag — if the daemon is up, we use it; otherwise
//! we fall back to the embedded store with no behavior change for the user.
//! What: `TrustyMemoryClient` implements `MemoryStore` against the
//! `/v1/memories` and `/v1/search` REST endpoints. `MemoryBackend` is an
//! enum delegating `MemoryStore` calls to whichever variant was chosen by
//! `auto_detect`. Health is probed via `GET /v1/health` with a short
//! connect timeout so startup never stalls.
//! Test: See `tests` below — `health_check` returns false against an
//! unreachable port; `auto_detect` falls back to local in that case.

#![allow(dead_code)]

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::memory::redb_usearch::RedbUsearchStore;
use crate::memory::store::{MemoryResult, MemoryStore, Segment};

/// Default address of the trusty-memory daemon for local-dev installs.
pub const DEFAULT_TRUSTY_URL: &str = "http://127.0.0.1:7775";

/// Connect timeout for the auto-detect health probe. Kept short so startup
/// never noticeably stalls when the daemon is down.
const HEALTH_TIMEOUT: Duration = Duration::from_millis(500);

/// Per-call timeout for normal CRUD requests (longer than the health probe
/// because actual operations may be slower than a TCP handshake).
const CALL_TIMEOUT: Duration = Duration::from_secs(5);

/// HTTP client wrapping the trusty-memory REST API.
///
/// Why: Threading raw `reqwest` calls through the codebase would couple
/// callers to URL construction and JSON shapes. Wrapping them in a typed
/// client keeps the `MemoryStore` impl self-contained.
/// What: Holds a base URL and a shared `reqwest::Client` (connection pooled
/// across requests). The `MemoryStore` impl maps our segment-scoped ids to
/// `mem:{segment}:{id}` keys so different segments don't collide in the
/// daemon's flat namespace.
/// Test: See `health_check_false_when_daemon_absent` and
/// `auto_detect_falls_back_to_local`.
pub struct TrustyMemoryClient {
    base_url: String,
    client: reqwest::Client,
}

impl TrustyMemoryClient {
    /// Construct a client against `base_url` (e.g. `http://127.0.0.1:7775`).
    ///
    /// Why: Operators may run the daemon on a custom host/port; centralising
    /// the URL here keeps the rest of the code parameterless.
    pub fn new(base_url: impl Into<String>) -> Self {
        let client = reqwest::Client::builder()
            .timeout(CALL_TIMEOUT)
            .connect_timeout(HEALTH_TIMEOUT)
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            base_url: base_url.into(),
            client,
        }
    }

    /// Probe `GET /v1/health`. Returns true on a 2xx response.
    ///
    /// Why: Used by `MemoryBackend::auto_detect` to decide whether the
    /// daemon is reachable. Any error (timeout, refused, DNS) is mapped to
    /// false so callers don't have to handle network errors at startup.
    pub async fn health_check(&self) -> bool {
        let url = format!("{}/v1/health", self.base_url.trim_end_matches('/'));
        match self.client.get(&url).send().await {
            Ok(resp) => resp.status().is_success(),
            Err(_) => false,
        }
    }

    /// Build the namespaced id used to address a record on the daemon.
    fn ns_id(segment: Segment, id: &str) -> String {
        format!("mem:{}:{}", segment.prefix(), id)
    }
}

#[derive(Serialize)]
struct InsertBody<'a> {
    id: String,
    content: String,
    vector: &'a [f32],
    metadata: Value,
}

#[derive(Serialize)]
struct SearchBody<'a> {
    vector: &'a [f32],
    limit: usize,
    namespace: String,
}

#[derive(Deserialize)]
struct SearchHit {
    id: String,
    score: f32,
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    metadata: Option<Value>,
}

#[derive(Deserialize)]
struct SearchResponse {
    #[serde(default)]
    results: Vec<SearchHit>,
}

#[derive(Deserialize)]
struct GetResponse {
    #[serde(default)]
    metadata: Option<Value>,
}

#[async_trait]
impl MemoryStore for TrustyMemoryClient {
    async fn insert(
        &self,
        segment: Segment,
        id: &str,
        vector: &[f32],
        payload: Value,
    ) -> Result<()> {
        let url = format!("{}/v1/memories", self.base_url.trim_end_matches('/'));
        // Encode the original payload alongside segment metadata so we can
        // reconstruct it on `get`. `content` carries a textual summary if
        // the payload happens to contain one; otherwise the JSON itself.
        let content = payload
            .get("text")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| payload.to_string());

        let body = InsertBody {
            id: Self::ns_id(segment, id),
            content,
            vector,
            metadata: serde_json::json!({
                "segment": segment.prefix(),
                "payload": payload,
            }),
        };

        let resp = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .with_context(|| format!("POST {url}"))?;
        if !resp.status().is_success() {
            return Err(anyhow!("trusty insert failed: HTTP {}", resp.status()));
        }
        Ok(())
    }

    async fn search(
        &self,
        segment: Segment,
        query_vec: &[f32],
        top_k: usize,
    ) -> Result<Vec<MemoryResult>> {
        let url = format!("{}/v1/search", self.base_url.trim_end_matches('/'));
        let body = SearchBody {
            vector: query_vec,
            limit: top_k,
            namespace: format!("mem:{}", segment.prefix()),
        };
        let resp = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .with_context(|| format!("POST {url}"))?;
        if !resp.status().is_success() {
            return Err(anyhow!("trusty search failed: HTTP {}", resp.status()));
        }
        let parsed: SearchResponse = resp.json().await.context("parsing search response")?;
        let prefix = format!("mem:{}:", segment.prefix());
        Ok(parsed
            .results
            .into_iter()
            .map(|h| {
                let id =
                    h.id.strip_prefix(&prefix)
                        .map(|s| s.to_string())
                        .unwrap_or(h.id);
                let payload = h
                    .metadata
                    .as_ref()
                    .and_then(|m| m.get("payload").cloned())
                    .or_else(|| h.content.clone().map(Value::String))
                    .unwrap_or(Value::Null);
                MemoryResult {
                    id,
                    score: h.score,
                    payload,
                    segment: segment.prefix().to_string(),
                }
            })
            .collect())
    }

    async fn get(&self, segment: Segment, id: &str) -> Result<Option<Value>> {
        let url = format!(
            "{}/v1/memories/{}",
            self.base_url.trim_end_matches('/'),
            Self::ns_id(segment, id)
        );
        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .with_context(|| format!("GET {url}"))?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !resp.status().is_success() {
            return Err(anyhow!("trusty get failed: HTTP {}", resp.status()));
        }
        let parsed: GetResponse = resp.json().await.context("parsing get response")?;
        Ok(parsed
            .metadata
            .as_ref()
            .and_then(|m| m.get("payload").cloned()))
    }

    async fn delete(&self, segment: Segment, id: &str) -> Result<()> {
        let url = format!(
            "{}/v1/memories/{}",
            self.base_url.trim_end_matches('/'),
            Self::ns_id(segment, id)
        );
        let resp = self
            .client
            .delete(&url)
            .send()
            .await
            .with_context(|| format!("DELETE {url}"))?;
        // Treat 404 as success — caller wanted the record gone, it's gone.
        if !resp.status().is_success() && resp.status() != reqwest::StatusCode::NOT_FOUND {
            return Err(anyhow!("trusty delete failed: HTTP {}", resp.status()));
        }
        Ok(())
    }
}

/// Runtime-selected memory backend.
///
/// Why: We want `auto_detect` to pick the best available backend without
/// every caller branching on configuration. Wrapping both options in an
/// enum lets us implement `MemoryStore` once and have callers stay
/// transport-agnostic.
/// What: `Local` holds an `Arc<RedbUsearchStore>` (shareable across tasks);
/// `Trusty` holds a stateless HTTP client.
/// Test: `auto_detect_falls_back_to_local` exercises the selection path.
pub enum MemoryBackend {
    Local(Arc<RedbUsearchStore>),
    Trusty(TrustyMemoryClient),
}

impl MemoryBackend {
    /// Auto-detect at the default daemon URL.
    pub async fn auto_detect(local_store: Arc<RedbUsearchStore>) -> Self {
        Self::auto_detect_with_url(DEFAULT_TRUSTY_URL, local_store).await
    }

    /// Auto-detect at `base_url`. Falls back to `local_store` if the daemon
    /// is unreachable within the health timeout.
    pub async fn auto_detect_with_url(base_url: &str, local_store: Arc<RedbUsearchStore>) -> Self {
        let client = TrustyMemoryClient::new(base_url);
        if client.health_check().await {
            tracing::info!(url = %base_url, "trusty-memory daemon reachable; using HTTP backend");
            MemoryBackend::Trusty(client)
        } else {
            tracing::debug!(
                url = %base_url,
                "trusty-memory daemon not reachable; using embedded local backend"
            );
            MemoryBackend::Local(local_store)
        }
    }
}

#[async_trait]
impl MemoryStore for MemoryBackend {
    async fn insert(
        &self,
        segment: Segment,
        id: &str,
        vector: &[f32],
        payload: Value,
    ) -> Result<()> {
        match self {
            MemoryBackend::Local(s) => s.insert(segment, id, vector, payload).await,
            MemoryBackend::Trusty(c) => c.insert(segment, id, vector, payload).await,
        }
    }

    async fn search(
        &self,
        segment: Segment,
        query_vec: &[f32],
        top_k: usize,
    ) -> Result<Vec<MemoryResult>> {
        match self {
            MemoryBackend::Local(s) => s.search(segment, query_vec, top_k).await,
            MemoryBackend::Trusty(c) => c.search(segment, query_vec, top_k).await,
        }
    }

    async fn get(&self, segment: Segment, id: &str) -> Result<Option<Value>> {
        match self {
            MemoryBackend::Local(s) => s.get(segment, id).await,
            MemoryBackend::Trusty(c) => c.get(segment, id).await,
        }
    }

    async fn delete(&self, segment: Segment, id: &str) -> Result<()> {
        match self {
            MemoryBackend::Local(s) => s.delete(segment, id).await,
            MemoryBackend::Trusty(c) => c.delete(segment, id).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Why: If the daemon is not running we must report unreachable rather
    /// than block startup. Pointing at a port nothing listens on is the
    /// simplest way to verify that without spinning up real infra.
    /// What: Build a client at `127.0.0.1:19999` (assumed unused) and call
    /// `health_check`; assert false.
    /// Test: This test.
    #[tokio::test]
    async fn health_check_false_when_daemon_absent() {
        let client = TrustyMemoryClient::new("http://127.0.0.1:19999");
        assert!(!client.health_check().await);
    }

    /// Why: `auto_detect` is the single entry point callers use; if it
    /// silently chose Trusty when the daemon is down, every subsequent
    /// memory call would fail. Verify the fallback path picks Local.
    /// What: Open a temp `RedbUsearchStore`, call `auto_detect_with_url`
    /// pointing at the same dead port, and assert the resulting backend is
    /// `Local`.
    /// Test: This test.
    #[tokio::test]
    async fn auto_detect_falls_back_to_local() {
        let dir = TempDir::new().unwrap();
        let store = Arc::new(RedbUsearchStore::open(dir.path(), 4).unwrap());
        let backend = MemoryBackend::auto_detect_with_url("http://127.0.0.1:19999", store).await;
        match backend {
            MemoryBackend::Local(_) => {}
            MemoryBackend::Trusty(_) => panic!("expected Local fallback"),
        }
    }

    /// Why: `ns_id` is what keeps separate segments from colliding in the
    /// daemon's flat key namespace. Lock the format with a test so refactors
    /// don't silently break cross-segment isolation.
    /// What: Build ns ids for two different segments + same logical id;
    /// assert they differ and contain the segment prefix.
    /// Test: This test.
    #[test]
    fn ns_id_namespaces_by_segment() {
        let a = TrustyMemoryClient::ns_id(Segment::Brief, "abc");
        let b = TrustyMemoryClient::ns_id(Segment::History, "abc");
        assert_ne!(a, b);
        assert!(a.contains(Segment::Brief.prefix()));
        assert!(b.contains(Segment::History.prefix()));
    }
}
