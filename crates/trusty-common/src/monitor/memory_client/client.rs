//! Typed HTTP client for the trusty-memory daemon.
//!
//! Why: the dashboard polls trusty-memory every refresh tick; a reusable client
//! keeps the probe cheap and the call sites tidy.
//! What: [`MemoryClient`] wraps a base URL and a pooled `reqwest::Client`; a
//! `fetch_all` helper folds the status and palace calls into [`MemoryData`].
//! Test: `cargo test -p trusty-common --features monitor-tui` covers URL
//! resolution and base-URL storage; live endpoints are covered by the
//! trusty-memory daemon's own suite.

use std::time::Duration;

use crate::monitor::dashboard::{MemoryData, PalaceRow};

use super::parsers::{
    parse_drawers, parse_dream_stats, parse_memory_details, parse_memory_event, parse_palaces,
    parse_recall_hits,
};
use super::types::{DrawerInfo, DreamStats, MemoryDetail, MemoryEvent, REQUEST_TIMEOUT, RecallHit};

/// Typed HTTP client for the trusty-memory daemon.
///
/// Why: the dashboard polls trusty-memory every refresh tick; a reusable client
/// keeps the probe cheap and the call sites tidy.
/// What: holds a mutable base URL plus a shared `reqwest::Client`; exposes the
/// read endpoints the memory panel renders.
/// Test: `memory_client_stores_base_url`.
#[derive(Debug, Clone)]
pub struct MemoryClient {
    pub(super) base: String,
    http: reqwest::Client,
}

impl MemoryClient {
    /// Build a client targeting `base` (e.g. `http://127.0.0.1:7070`).
    ///
    /// Why: the dashboard is pointed at an address resolved from the lock file.
    /// What: stores the base URL and a pooled `reqwest::Client` with a request
    /// timeout.
    /// Test: `memory_client_stores_base_url`.
    pub fn new(base: impl Into<String>) -> Self {
        let http = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .unwrap_or_default();
        Self {
            base: base.into(),
            http,
        }
    }

    /// The base URL this client targets.
    ///
    /// Why: the dashboard renders the daemon address and re-resolution compares
    /// against the current target.
    /// What: returns the stored base URL.
    /// Test: `memory_client_stores_base_url`.
    pub fn base_url(&self) -> &str {
        &self.base
    }

    /// Re-point this client at a freshly resolved daemon URL.
    ///
    /// Why: trusty-memory rebinds a fresh dynamic port on every restart, so a
    /// long-lived dashboard must follow it.
    /// What: overwrites the base URL, keeping the pooled client.
    /// Test: `memory_client_repoints`.
    pub fn set_base_url(&mut self, base: impl Into<String>) {
        self.base = base.into();
    }

    /// Fetch every panel field from the trusty-memory daemon.
    ///
    /// Why: the dashboard wants one fallible call that yields a complete
    /// [`MemoryData`] or an error it can render as the offline state.
    /// What: GETs `/api/v1/status`, then `/api/v1/palaces`, folding both into
    /// [`MemoryData`]. A failed palace-list probe yields an empty list rather
    /// than failing the whole poll, since the aggregate counts still render.
    /// Test: live behaviour is covered by the trusty-memory daemon suite; the
    /// dashboard's offline path is unit-tested in `dashboard.rs`.
    pub async fn fetch_all(&self) -> anyhow::Result<MemoryData> {
        use super::types::StatusWire;

        let status: StatusWire = self
            .http
            .get(format!("{}/api/v1/status", self.base))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;

        let palaces = match self.palaces().await {
            Ok(rows) => rows,
            Err(e) => {
                tracing::warn!("palace list probe failed: {e}");
                Vec::new()
            }
        };

        Ok(MemoryData {
            version: status.version,
            palace_count: status.palace_count,
            total_drawers: status.total_drawers,
            total_vectors: status.total_vectors,
            total_kg_triples: status.total_kg_triples,
            palaces,
        })
    }

    /// Probe whether the trusty-memory daemon is reachable.
    ///
    /// Why: the memory panel shows an offline badge when the daemon is down;
    /// the cheap `/health` probe decides reachability before the heavier
    /// status calls run.
    /// What: GETs `/health`, returns `true` on any 2xx response.
    /// Test: covered by the trusty-memory daemon suite.
    pub async fn is_healthy(&self) -> bool {
        matches!(
            self.http.get(format!("{}/health", self.base)).send().await,
            Ok(r) if r.status().is_success()
        )
    }

    /// Fetch the palace list from `GET /api/v1/palaces`.
    ///
    /// Why: the memory panel renders one row per palace with its vector count.
    /// What: GETs the palace list and projects each entry to a [`PalaceRow`].
    /// The endpoint may return either a bare array or an object with a
    /// `palaces` field; both shapes are accepted.
    /// Test: `palace_list_accepts_array_and_object_shapes`.
    async fn palaces(&self) -> anyhow::Result<Vec<PalaceRow>> {
        let raw: serde_json::Value = self
            .http
            .get(format!("{}/api/v1/palaces", self.base))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(parse_palaces(&raw))
    }

    /// Recall memories matching `query` from `GET /api/v1/recall`.
    ///
    /// Why: the memory TUI's input bar runs a cross-palace recall and folds the
    /// hits into the activity log; this is the transport for that action.
    /// What: GETs `/api/v1/recall?q=<query>&top_k=<top_k>`, then projects each
    /// result object into a [`RecallHit`]. A non-2xx response or malformed
    /// payload yields an error.
    /// Test: live behaviour is covered by the trusty-memory daemon suite; the
    /// projection is unit-tested via `parse_recall_hits`.
    pub async fn recall(&self, query: &str, top_k: usize) -> anyhow::Result<Vec<RecallHit>> {
        let raw: serde_json::Value = self
            .http
            .get(format!("{}/api/v1/recall", self.base))
            .query(&[("q", query), ("top_k", &top_k.to_string())])
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(parse_recall_hits(&raw))
    }

    /// List drawers in `palace_id`, newest first, with offset pagination.
    ///
    /// Why: the TUI activity panel (#184) shows a paged drawer list for the
    /// selected palace. The daemon's `/api/v1/palaces/{id}/drawers` endpoint
    /// (extended in the same change-set) supports `sort=created_desc` and an
    /// `offset` so the panel can walk through arbitrarily many drawers
    /// without re-sorting on the client.
    /// What: GETs `…/drawers?limit=<limit>&offset=<offset>&sort=created_desc`
    /// and projects each entry into a [`DrawerInfo`] via [`parse_drawers`]. A
    /// non-2xx response yields an error; an empty body yields an empty list.
    /// Test: live behaviour covered by the trusty-memory daemon suite; the
    /// projection is unit-tested via [`parse_drawers`].
    pub async fn list_drawers(
        &self,
        palace_id: &str,
        limit: usize,
        offset: usize,
    ) -> anyhow::Result<Vec<DrawerInfo>> {
        let raw: serde_json::Value = self
            .http
            .get(format!(
                "{}/api/v1/palaces/{}/drawers",
                self.base, palace_id,
            ))
            .query(&[
                ("limit", limit.to_string()),
                ("offset", offset.to_string()),
                ("sort", "created_desc".to_string()),
            ])
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(parse_drawers(&raw))
    }

    /// Fetch a single drawer's full content / tags by id (issue #215).
    ///
    /// Why: the TUI drawer-detail modal needs the verbatim drawer body —
    /// the activity panel only carries the snippet. Each drawer in
    /// trusty-memory is itself the "memory" unit, so "detail" means
    /// fetching the same drawer payload the list endpoint returns but
    /// projected to the fields the modal renders. The endpoint reuses
    /// the existing `/api/v1/palaces/{id}/drawers` route — there is no
    /// dedicated single-drawer GET — and we filter the result client-side
    /// to the requested `drawer_id` so the wire shape stays stable.
    /// What: GETs `…/drawers?limit=<limit>&sort=created_desc`, then projects
    /// each entry into a [`MemoryDetail`]. The caller is expected to pass
    /// a reasonable `limit` (e.g. 50). Returns the full list — the caller
    /// can find the row they want by id, or render them all if the modal
    /// scrolls through every memory in the drawer's neighborhood. A
    /// non-2xx response yields an error; an unrecognised body yields an
    /// empty list.
    /// Test: live behaviour covered by the trusty-memory daemon suite; the
    /// projection is unit-tested via [`parse_memory_details`].
    pub async fn fetch_drawer_detail(
        &self,
        palace_id: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<MemoryDetail>> {
        let raw: serde_json::Value = self
            .http
            .get(format!(
                "{}/api/v1/palaces/{}/drawers",
                self.base, palace_id,
            ))
            .query(&[
                ("limit", limit.to_string()),
                ("sort", "created_desc".to_string()),
            ])
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(parse_memory_details(&raw))
    }

    /// Trigger a dream cycle via `POST /api/v1/dream/run`.
    ///
    /// Why: the memory TUI's `[d]` key runs a dream cycle (merge / prune /
    /// compact) across every palace and shows the resulting counts.
    /// What: POSTs an empty body to `/api/v1/dream/run` and projects the
    /// response into a [`DreamStats`]. A non-2xx response yields an error.
    /// Test: live behaviour is covered by the trusty-memory daemon suite; the
    /// projection is unit-tested via `parse_dream_stats`.
    pub async fn dream_run(&self) -> anyhow::Result<DreamStats> {
        let raw: serde_json::Value = self
            .http
            .post(format!("{}/api/v1/dream/run", self.base))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(parse_dream_stats(&raw))
    }

    /// Subscribe to the daemon's `/sse` stream and forward events into `tx`.
    ///
    /// Why: the memory TUI subscribes once at startup so palace / drawer /
    /// dream events appear in the activity log without polling; the background
    /// task drives this while the synchronous event loop drains `tx`.
    /// What: GETs `/sse`, parses each `data:` frame's `type`-tagged JSON into a
    /// [`MemoryEvent`], and sends each through `tx`. Returns quietly when the
    /// stream ends, the receiver is dropped, or a transport error occurs — the
    /// caller treats SSE as best-effort and keeps polling regardless.
    /// Test: event parsing is unit-tested via `parse_memory_event`.
    pub async fn sse_stream(&self, tx: tokio::sync::mpsc::Sender<MemoryEvent>) {
        let _ = self.sse_stream_inner(&tx).await;
    }

    /// Inner body of [`Self::sse_stream`] returning a `Result` for `?`.
    ///
    /// Why: keeps the public method's best-effort error swallowing in one
    /// place while the happy path uses `?`.
    /// What: opens the SSE stream and forwards parsed [`MemoryEvent`]s; returns
    /// the first transport error.
    /// Test: covered indirectly by `sse_stream` and the daemon suite.
    async fn sse_stream_inner(
        &self,
        tx: &tokio::sync::mpsc::Sender<MemoryEvent>,
    ) -> anyhow::Result<()> {
        use futures_util::StreamExt;

        // SSE is long-lived — bound only the connect phase, not the read.
        let sse = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .build()?;
        let resp = sse
            .get(format!("{}/sse", self.base))
            .send()
            .await?
            .error_for_status()?;

        let mut bytes = resp.bytes_stream();
        let mut buf = String::new();
        while let Some(chunk) = bytes.next().await {
            let chunk = chunk?;
            buf.push_str(&String::from_utf8_lossy(&chunk));
            while let Some(nl) = buf.find('\n') {
                let line = buf[..nl].trim_end_matches('\r').to_string();
                buf.drain(..=nl);
                let Some(payload) = line.strip_prefix("data:") else {
                    continue;
                };
                let payload = payload.trim();
                if payload.is_empty() {
                    continue;
                }
                if let Ok(value) = serde_json::from_str::<serde_json::Value>(payload)
                    && let Some(event) = parse_memory_event(&value)
                    && tx.send(event).await.is_err()
                {
                    return Ok(()); // receiver gone — stop quietly.
                }
            }
        }
        Ok(())
    }
}
