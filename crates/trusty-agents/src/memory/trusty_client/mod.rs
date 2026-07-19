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
//! What: `TrustyMemoryClient` implements `MemoryStore` against trusty-memory's
//! *real* HTTP surface (issue #3225 — the previous implementation targeted a
//! `/v1/*` route family that trusty-memory's router never mounted, so the
//! health probe always failed and every install silently ran on the embedded
//! store even with the daemon up). The real surface is:
//!   - `GET /health` — bare, unversioned liveness probe.
//!   - `POST /api/v1/palaces` — idempotent-ish palace creation (`force: true`
//!     bypasses the project-slug naming gate); one palace per `Segment`,
//!     named `trusty-agents-{segment-prefix}` (mirrors the naming convention
//!     already used by `TrustyBackedMemoryStore`).
//!   - `POST /api/v1/palaces/{id}/drawers` — creates a drawer. trusty-memory
//!     computes its own embedding server-side (ONNX, via the shared
//!     embedder) from `content`; there is no endpoint that accepts a
//!     caller-supplied vector, so `insert`'s `vector` argument is accepted
//!     for trait-compatibility but not transmitted. `content` is the
//!     JSON-serialized `payload` (not a text summary) so `get` can losslessly
//!     round-trip it back — trusty-memory's `CreateDrawerBody` has no
//!     side-channel metadata field, only `content`/`room`/`tags`/`importance`.
//!   - `GET /api/v1/palaces/{id}/drawers?tag=<ns_id>` — trusty-agents' string
//!     ids are tagged onto the drawer at insert time (`ns_id(segment, id)`)
//!     since drawers are keyed by server-generated UUID, not caller id;
//!     `get`/`delete` resolve the UUID via an exact tag-match lookup first.
//!   - `DELETE /api/v1/palaces/{id}/drawers/{uuid}` — delete by resolved UUID.
//!
//!   `search(segment, query_vec, top_k)` is NOT wired to
//!   `GET /api/v1/palaces/{id}/recall` — see that method's doc comment for
//!   why a raw vector cannot be bridged onto trusty-memory's text-embedding
//!   recall endpoint, and returns a descriptive error instead of silently
//!   hitting the wrong route or fabricating results.
//!
//! Test: See `tests` below — `health_check_false_when_daemon_absent`;
//! `auto_detect` falls back to local when unreachable; a mock HTTP server
//! (mirroring the real `/health`, `/api/v1/palaces`, and
//! `/api/v1/palaces/{id}/drawers` routes) proves `auto_detect` selects the
//! HTTP backend when the daemon *is* reachable, and exercises the
//! insert/get/delete round trip end to end.

#![allow(dead_code)]

use std::collections::HashSet;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

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
/// What: Holds a base URL, a shared `reqwest::Client` (connection pooled
/// across requests), and a small in-memory cache of palace ids already
/// confirmed to exist (`ensure_palace` skips the create round-trip once a
/// segment's palace is known-present for this client's lifetime). The
/// `MemoryStore` impl maps our segment-scoped ids to `mem:{segment}:{id}`
/// tags so different segments — and different records within a segment —
/// don't collide in trusty-memory's UUID-keyed drawer namespace.
/// Test: See `health_check_false_when_daemon_absent`,
/// `auto_detect_falls_back_to_local`, and the mock-server round-trip tests.
pub struct TrustyMemoryClient {
    base_url: String,
    client: reqwest::Client,
    /// Palace ids this client has already created/confirmed. Avoids a
    /// `POST /api/v1/palaces` round-trip on every `insert`.
    ensured_palaces: RwLock<HashSet<String>>,
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
            ensured_palaces: RwLock::new(HashSet::new()),
        }
    }

    /// Probe `GET /health`. Returns true on a 2xx response.
    ///
    /// Why: Used by `MemoryBackend::auto_detect` to decide whether the
    /// daemon is reachable. Any error (timeout, refused, DNS) is mapped to
    /// false so callers don't have to handle network errors at startup.
    /// Issue #3225: this used to probe `/v1/health`, a route trusty-memory
    /// never mounts (its router carries `/health` bare, unversioned — see
    /// `crates/trusty-memory/src/web/mod.rs`), so the probe always failed
    /// against a real daemon.
    pub async fn health_check(&self) -> bool {
        let url = format!("{}/health", self.base_url.trim_end_matches('/'));
        match self.client.get(&url).send().await {
            Ok(resp) => resp.status().is_success(),
            Err(_) => false,
        }
    }

    /// Build the namespaced id used to address a record on the daemon.
    fn ns_id(segment: Segment, id: &str) -> String {
        format!("mem:{}:{}", segment.prefix(), id)
    }

    /// Deterministic palace id for `segment` — one palace per segment.
    ///
    /// Why: mirrors the `trusty-agents-{prefix}` naming convention already
    /// established by `TrustyBackedMemoryStore::palace_id_for` for the
    /// in-process Palace adapter, so an operator browsing the daemon's admin
    /// UI sees a consistent naming scheme regardless of which trusty-agents
    /// adapter wrote the data.
    fn palace_id_for(segment: Segment) -> String {
        format!("trusty-agents-{}", segment.prefix())
    }

    /// Ensure `segment`'s palace exists on the daemon, creating it on first
    /// use and caching the result for the lifetime of this client.
    ///
    /// Why: `POST /api/v1/palaces/{id}/drawers` 404s against a palace that
    /// was never created. `force: true` bypasses trusty-memory's
    /// project-slug naming gate (`validate_palace_name`), which is meant for
    /// user-facing palaces tied to a project checkout, not this adapter's
    /// synthetic per-segment palaces.
    /// What: Checks the in-memory cache first; on a miss, POSTs a create
    /// request (idempotent in effect — trusty-memory's `create_palace`
    /// overwrites `palace.json` rather than erroring when the directory
    /// already exists) and caches the id on success.
    /// Test: `insert_creates_palace_once_then_reuses` (mock-server test).
    async fn ensure_palace(&self, segment: Segment) -> Result<()> {
        let palace_id = Self::palace_id_for(segment);
        {
            let cache = self
                .ensured_palaces
                .read()
                .unwrap_or_else(|e| e.into_inner());
            if cache.contains(&palace_id) {
                return Ok(());
            }
        }

        let url = format!("{}/api/v1/palaces", self.base_url.trim_end_matches('/'));
        let body = CreatePalaceReq {
            name: palace_id.clone(),
            description: "Auto-created by trusty-agents TrustyMemoryClient".to_string(),
            force: true,
        };
        let resp = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .with_context(|| format!("POST {url}"))?;
        if !resp.status().is_success() {
            return Err(anyhow!(
                "trusty create_palace({palace_id}) failed: HTTP {}",
                resp.status()
            ));
        }

        self.ensured_palaces
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(palace_id);
        Ok(())
    }

    /// Resolve the drawer(s) tagged with `tag` inside `palace_id`.
    ///
    /// Why: shared by `get`, `delete`, and `insert`'s upsert-cleanup step —
    /// all three need to translate our caller-supplied string id into the
    /// server-generated UUID that `DELETE .../drawers/{uuid}` requires.
    /// What: `GET /api/v1/palaces/{palace_id}/drawers?tag=<tag>&limit=1`. A
    /// 404 (unknown palace) is treated as "no matches" rather than an error
    /// so callers see a clean `None`/empty result instead of a spurious
    /// failure on a not-yet-created palace.
    /// Test: `get_returns_none_when_absent`, `delete_is_noop_when_absent`.
    async fn find_by_tag(&self, palace_id: &str, tag: &str) -> Result<Vec<DrawerRow>> {
        let url = format!(
            "{}/api/v1/palaces/{palace_id}/drawers",
            self.base_url.trim_end_matches('/')
        );
        let resp = self
            .client
            .get(&url)
            .query(&[("tag", tag), ("limit", "1")])
            .send()
            .await
            .with_context(|| format!("GET {url}"))?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(Vec::new());
        }
        if !resp.status().is_success() {
            return Err(anyhow!(
                "trusty list_drawers failed: HTTP {}",
                resp.status()
            ));
        }
        resp.json().await.context("parsing drawers response")
    }

    /// Delete a drawer by its resolved server UUID. 404 is treated as
    /// success (already gone).
    async fn delete_by_uuid(&self, palace_id: &str, drawer_id: Uuid) -> Result<()> {
        let url = format!(
            "{}/api/v1/palaces/{palace_id}/drawers/{drawer_id}",
            self.base_url.trim_end_matches('/')
        );
        let resp = self
            .client
            .delete(&url)
            .send()
            .await
            .with_context(|| format!("DELETE {url}"))?;
        if !resp.status().is_success() && resp.status() != reqwest::StatusCode::NOT_FOUND {
            return Err(anyhow!("trusty delete failed: HTTP {}", resp.status()));
        }
        Ok(())
    }
}

#[derive(Serialize)]
struct CreatePalaceReq {
    name: String,
    description: String,
    force: bool,
}

#[derive(Serialize)]
struct CreateDrawerReq {
    content: String,
    tags: Vec<String>,
}

/// Minimal shape we read back from `GET .../drawers` — trusty-memory's
/// `Drawer` struct (`trusty_common::memory_core::palace::Drawer`) has many
/// more fields; we only need `id` and `content` to resolve/round-trip a
/// record, plus `tags` for defensive filtering client-side.
#[derive(Deserialize)]
struct DrawerRow {
    id: Uuid,
    content: String,
    #[serde(default)]
    tags: Vec<String>,
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
        // trusty-memory computes its own embedding server-side (ONNX, via
        // the shared embedder) from `content` on `POST .../drawers`; there
        // is no endpoint that accepts a caller-precomputed vector, so it is
        // intentionally not transmitted. See the module doc comment.
        let _ = vector;

        self.ensure_palace(segment).await?;
        let palace_id = Self::palace_id_for(segment);
        let ns_id = Self::ns_id(segment, id);

        // Upsert: drawers are keyed by server-generated UUID, not our string
        // id, so a second `insert` for the same id would otherwise create a
        // duplicate drawer instead of overwriting the first (unlike the
        // redb-keyed local store). Best-effort delete any existing match
        // before creating the new one.
        if let Some(existing) = self
            .find_by_tag(&palace_id, &ns_id)
            .await?
            .into_iter()
            .next()
        {
            self.delete_by_uuid(&palace_id, existing.id).await?;
        }

        let url = format!(
            "{}/api/v1/palaces/{palace_id}/drawers",
            self.base_url.trim_end_matches('/')
        );
        // Serialize the full payload (rather than a text summary) as
        // `content` so `get` can losslessly round-trip it — trusty-memory's
        // `CreateDrawerBody` carries no separate metadata field.
        let content = serde_json::to_string(&payload).context("serialize payload as content")?;
        let body = CreateDrawerReq {
            content,
            tags: vec![ns_id],
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

    /// Not supported over HTTP against a real trusty-memory daemon.
    ///
    /// Why: `MemoryStore::search` takes a pre-computed `query_vec: &[f32]`
    /// (the contract the local `RedbUsearchStore`/`TrustyBackedMemoryStore`
    /// adapters satisfy by querying an in-process HNSW index directly).
    /// trusty-memory's only HTTP recall surface is
    /// `GET /api/v1/palaces/{id}/recall?q=<text>` — the daemon computes the
    /// query embedding itself, server-side, from text
    /// (`trusty_common::memory_core::retrieval::recall_with_default_embedder`).
    /// There is no route that accepts a raw vector, and embeddings are not
    /// invertible, so a caller-supplied vector cannot be turned into the `q`
    /// text parameter that route requires. Adding a vector-accepting recall
    /// route to trusty-memory was judged out of scope for issue #3225 (a
    /// route-correctness fix, not a new daemon capability) — see the PR body
    /// for the follow-up recommendation.
    /// What: Returns a descriptive `Err` rather than silently hitting the
    /// wrong endpoint or fabricating empty results.
    /// Test: `search_returns_descriptive_unsupported_error`.
    async fn search(
        &self,
        _segment: Segment,
        _query_vec: &[f32],
        _top_k: usize,
    ) -> Result<Vec<MemoryResult>> {
        Err(anyhow!(
            "TrustyMemoryClient::search is not supported: trusty-memory's HTTP API only \
             exposes text-query recall (GET /api/v1/palaces/{{id}}/recall?q=<text>), which \
             computes its own embedding server-side. There is no route that accepts a \
             pre-computed vector, so a MemoryStore::search() call cannot be bridged onto it. \
             Use MemoryBackend::Local, or extend the daemon/trait with a text-aware recall \
             path, for vector/semantic search against the HTTP backend."
        ))
    }

    async fn get(&self, segment: Segment, id: &str) -> Result<Option<Value>> {
        let palace_id = Self::palace_id_for(segment);
        let ns_id = Self::ns_id(segment, id);
        let Some(row) = self
            .find_by_tag(&palace_id, &ns_id)
            .await?
            .into_iter()
            .next()
        else {
            return Ok(None);
        };
        // `content` is the JSON-serialized payload written by `insert`.
        // Fall back to a plain string wrap for any drawer written by a
        // different producer (e.g. hand-authored via the admin UI) whose
        // content isn't valid JSON.
        match serde_json::from_str::<Value>(&row.content) {
            Ok(v) => Ok(Some(v)),
            Err(_) => Ok(Some(Value::String(row.content))),
        }
    }

    async fn delete(&self, segment: Segment, id: &str) -> Result<()> {
        let palace_id = Self::palace_id_for(segment);
        let ns_id = Self::ns_id(segment, id);
        let Some(row) = self
            .find_by_tag(&palace_id, &ns_id)
            .await?
            .into_iter()
            .next()
        else {
            // Nothing tagged with this id — caller wanted it gone, it's gone.
            return Ok(());
        };
        self.delete_by_uuid(&palace_id, row.id).await
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
/// Test: `auto_detect_falls_back_to_local` exercises the selection path;
/// `auto_detect_selects_trusty_when_daemon_reachable` exercises the
/// happy path against a mock server mirroring the real route paths.
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
mod tests;
