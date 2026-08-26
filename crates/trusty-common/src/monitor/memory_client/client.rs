//! Typed client for the trusty-memory daemon, over its Unix socket.
//!
//! Why: the dashboard polls trusty-memory every refresh tick. Until #6286 it
//! did so with its own pooled `reqwest::Client` against `/api/v1/*` — a second
//! independent client that [`crate::memory_rpc`]'s module doc named and
//! explicitly declined to reuse. Those routes are gone, so the choice is no
//! longer between two clients but between one and a new one.
//! What: [`MemoryClient`] holds a socket path and a per-call budget; every
//! method is one [`crate::memory_rpc::call_memory_tool_at_with_timeout`] plus
//! the projection that used to run on the HTTP body. `fetch_all` folds the
//! status and palace calls into [`MemoryData`].
//! Test: `cargo test -p trusty-common --features monitor-tui` — see `tests.rs`.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::memory_rpc::call_memory_tool_at_with_timeout;
use crate::monitor::dashboard::{MemoryData, PalaceRow};

use super::parsers::{
    parse_drawers, parse_dream_stats, parse_memory_details, parse_memory_event,
    parse_palace_detail, parse_recall_hits,
};
use super::types::{DrawerInfo, DreamStats, MemoryDetail, MemoryEvent, REQUEST_TIMEOUT, RecallHit};

/// How many activity rows one [`MemoryClient::recent_events`] poll reads.
///
/// Why: the retired `/sse` stream pushed each event as it happened, so the TUI
/// never had to bound anything. Polling does, and the bound has to exceed what
/// the daemon can produce between two ticks or events are dropped silently.
/// 100 rows against a 2-second tick is far past any observed rate.
const EVENT_PAGE: usize = 100;

/// Typed client for the trusty-memory daemon.
///
/// Why: the dashboard polls trusty-memory every refresh tick; keeping the
/// socket path and the budget together keeps the call sites tidy.
/// What: holds a mutable socket path plus the per-call timeout; exposes the
/// read methods the memory panel renders. Cloning is cheap — there is no
/// connection pool to share, because each call dials, exchanges one frame pair,
/// and closes.
/// Test: `memory_client_stores_its_socket`.
#[derive(Debug, Clone)]
pub struct MemoryClient {
    pub(super) socket: PathBuf,
}

impl MemoryClient {
    /// Build a client dialling `socket`.
    ///
    /// Test: `memory_client_stores_its_socket`.
    pub fn new(socket: impl Into<PathBuf>) -> Self {
        Self {
            socket: socket.into(),
        }
    }

    /// The socket this client dials.
    ///
    /// Test: `memory_client_stores_its_socket`.
    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// Re-point this client at a freshly resolved socket.
    ///
    /// Why: the path is derived rather than published since #6286, so it does
    /// not change under a running dashboard the way a dynamic port did. The
    /// setter stays because a `TRUSTY_DATA_DIR_OVERRIDE` or
    /// `TRUSTY_MEMORY_SOCKET` change between ticks still moves it, and the
    /// poller re-resolves each time.
    /// Test: `memory_client_repoints`.
    pub fn set_socket(&mut self, socket: impl Into<PathBuf>) {
        self.socket = socket.into();
    }

    /// One RPC call on this client's socket, bounded by [`REQUEST_TIMEOUT`].
    async fn call(&self, method: &str, params: serde_json::Value) -> anyhow::Result<Value> {
        call_memory_tool_at_with_timeout(&self.socket, method, params, REQUEST_TIMEOUT).await
    }

    /// Fetch every panel field from the trusty-memory daemon.
    ///
    /// Why: the dashboard wants one fallible call that yields a complete
    /// [`MemoryData`] or an error it can render as the offline state.
    /// What: calls `memory.status`, then [`Self::palaces`], folding both into
    /// [`MemoryData`]. A failed palace probe yields an empty list rather than
    /// failing the whole poll, since the aggregate counts still render.
    /// Test: live behaviour is covered by the trusty-memory daemon suite; the
    /// dashboard's offline path is unit-tested in `dashboard.rs`.
    pub async fn fetch_all(&self) -> anyhow::Result<MemoryData> {
        use super::types::StatusWire;

        let raw = self.call("memory.status", json!({})).await?;
        let status: StatusWire = serde_json::from_value(raw)?;

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
    /// the cheap `memory.health` call decides reachability before the heavier
    /// status calls run.
    /// What: one `memory.health` call; any error is `false`.
    /// Test: covered by the trusty-memory daemon suite.
    pub async fn is_healthy(&self) -> bool {
        self.call("memory.health", json!({})).await.is_ok()
    }

    /// Fetch the palace list with live counts.
    ///
    /// Why (#6286): the retired `GET /api/v1/palaces` returned one row per
    /// palace WITH its counts. Nothing on the socket does — `palace_list`
    /// answers bare ids, and `memory.palace_get` answers one palace. So this
    /// asks for the ids and then fans out one `memory.palace_get` per id.
    ///
    /// The cost is not what the fan-out makes it look like. The bulk route used
    /// `PalaceRegistry::peek` since #4640, so it reported `cached: false` and
    /// placeholder zeros for any palace not already resident — which is why
    /// `fetch_palace` had to exist at all (#4682). `memory.palace_get` opens the
    /// palace, so the FIRST poll pays a cold open per palace and every poll
    /// after it hits the daemon's registry. The rows are also real counts
    /// rather than zeros, which the bulk route only managed for resident
    /// palaces.
    ///
    /// What: `palace_list`, then a `memory.palace_get` per id. A palace whose
    /// detail call fails is dropped rather than rendered as a row of zeros.
    /// Test: live behaviour is covered by the trusty-memory daemon suite.
    async fn palaces(&self) -> anyhow::Result<Vec<PalaceRow>> {
        let listed = self.call("palace_list", json!({})).await?;
        let ids: Vec<String> = listed
            .get("palaces")
            .and_then(|v| v.as_array())
            .map(|items| {
                items
                    .iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();

        let mut rows = Vec::with_capacity(ids.len());
        for id in ids {
            match self.fetch_palace(&id).await {
                Ok(row) => rows.push(row),
                Err(e) => tracing::debug!(palace = %id, "palace detail probe failed: {e}"),
            }
        }
        Ok(rows)
    }

    /// Fetch one palace's live counts.
    ///
    /// Why (#4682): the counts have to be measurements, not the placeholder
    /// zeros a peek-based listing reports for an unresident palace.
    /// What: `memory.palace_get`, projected by [`parse_palace_detail`]. A body
    /// that is not a palace object is an error rather than a row of silent
    /// zeros.
    /// Test: live behaviour is covered by the trusty-memory daemon suite.
    pub async fn fetch_palace(&self, palace_id: &str) -> anyhow::Result<PalaceRow> {
        let raw = self
            .call("memory.palace_get", json!({ "palace_id": palace_id }))
            .await?;
        parse_palace_detail(&raw)
            .ok_or_else(|| anyhow::anyhow!("unexpected palace payload for '{palace_id}'"))
    }

    /// Recall memories matching `query` across every palace.
    ///
    /// Why: the memory TUI's input bar runs a cross-palace recall and folds the
    /// hits into the activity log; this is the transport for that action.
    /// What: `memory_recall_all`, whose `results` array [`parse_recall_hits`]
    /// projects. The retired `GET /api/v1/recall` answered the array bare; the
    /// tool wraps it alongside the echoed query.
    /// Test: the projection is unit-tested via `parse_recall_hits`.
    pub async fn recall(&self, query: &str, top_k: usize) -> anyhow::Result<Vec<RecallHit>> {
        let raw = self
            .call("memory_recall_all", json!({ "q": query, "top_k": top_k }))
            .await?;
        Ok(parse_recall_hits(
            raw.get("results").unwrap_or(&Value::Null),
        ))
    }

    /// List drawers in `palace_id`, newest first, with offset pagination.
    ///
    /// Why: the TUI activity panel (#184) shows a paged drawer list for the
    /// selected palace.
    /// What: `memory.drawers_list` with the same `limit` / `offset` /
    /// `sort=created_desc` the query string carried, projected by
    /// [`parse_drawers`].
    /// Test: the projection is unit-tested via [`parse_drawers`].
    pub async fn list_drawers(
        &self,
        palace_id: &str,
        limit: usize,
        offset: usize,
    ) -> anyhow::Result<Vec<DrawerInfo>> {
        let raw = self
            .call(
                "memory.drawers_list",
                json!({
                    "palace_id": palace_id,
                    "limit": limit,
                    "offset": offset,
                    "sort": "created_desc",
                }),
            )
            .await?;
        Ok(parse_drawers(&raw))
    }

    /// Fetch drawers with their full bodies for the detail modal (#215).
    ///
    /// Why: the activity panel's row carries a truncated snippet; the modal that
    /// opens on `Enter` needs the verbatim body.
    /// What: the same `memory.drawers_list` call, projected by
    /// [`parse_memory_details`] instead. There is no single-drawer read, so the
    /// caller passes a reasonable `limit` and finds its row by id.
    /// Test: the projection is unit-tested via [`parse_memory_details`].
    pub async fn fetch_drawer_detail(
        &self,
        palace_id: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<MemoryDetail>> {
        let raw = self
            .call(
                "memory.drawers_list",
                json!({
                    "palace_id": palace_id,
                    "limit": limit,
                    "sort": "created_desc",
                }),
            )
            .await?;
        Ok(parse_memory_details(&raw))
    }

    /// Trigger a dream cycle across every palace.
    ///
    /// Why: the memory TUI's `[d]` key runs a dream cycle (merge / prune /
    /// compact) and shows the resulting counts.
    /// What: `memory.dream_run`, projected by [`parse_dream_stats`].
    /// Test: the projection is unit-tested via `parse_dream_stats`.
    pub async fn dream_run(&self) -> anyhow::Result<DreamStats> {
        let raw = self.call("memory.dream_run", json!({})).await?;
        Ok(parse_dream_stats(&raw))
    }

    /// Read activity rows newer than `after_id` (#6286).
    ///
    /// Why: this replaces the `/sse` subscription. That stream's only
    /// subscribers were trusty-memory's own SPA and this client, and ADR-0032
    /// left no HTTP listener to carry it — so the TUI polls the same events out
    /// of the activity log the stream was fed from.
    ///
    /// **The push-to-poll swap is a behaviour change, not a transport swap.**
    /// An event now appears on the next tick rather than immediately, and an
    /// event evicted from the activity log between two ticks is never seen. The
    /// [`EVENT_PAGE`] window is what bounds the second case.
    ///
    /// What: `memory.activity` for the newest [`EVENT_PAGE`] rows, keeps those
    /// with an id above `after_id`, reverses them into chronological order, and
    /// maps each row's `payload` through [`parse_memory_event`] — which is the
    /// same `type`-tagged `DaemonEvent` body the SSE frames carried. Returns the
    /// highest id seen so the caller can advance its cursor.
    /// Test: `recent_events_keeps_only_rows_past_the_cursor`.
    pub async fn recent_events(&self, after_id: u64) -> anyhow::Result<(u64, Vec<MemoryEvent>)> {
        let raw = self
            .call("memory.activity", json!({ "limit": EVENT_PAGE }))
            .await?;
        Ok(project_events(&raw, after_id))
    }
}

/// [`MemoryClient::recent_events`]' body, over an already-fetched page.
///
/// Why: separated so the cursor arithmetic and the ordering are testable
/// without a daemon — the only part of the SSE replacement that can be wrong
/// silently.
/// Test: `recent_events_keeps_only_rows_past_the_cursor`.
pub(super) fn project_events(raw: &Value, after_id: u64) -> (u64, Vec<MemoryEvent>) {
    let Some(entries) = raw.get("entries").and_then(|v| v.as_array()) else {
        return (after_id, Vec::new());
    };
    let mut highest = after_id;
    let mut events = Vec::new();
    // The daemon answers newest-first; the log renders oldest-first.
    for entry in entries.iter().rev() {
        let id = entry
            .get("id")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        if id <= after_id {
            continue;
        }
        highest = highest.max(id);
        if let Some(payload) = entry.get("payload")
            && let Some(event) = parse_memory_event(payload)
        {
            events.push(event);
        }
    }
    (highest, events)
}
