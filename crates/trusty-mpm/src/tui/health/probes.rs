//! HTTP transport and JSON projections for one daemon's health endpoints.
//!
//! Why: the background poller needs a small, testable transport that yields a
//! projected [`PanelData`] or a clean error string, plus the per-daemon list
//! endpoints that feed the collections pane. Keeping the projections pure lets
//! them be unit-tested without a live daemon.
//! What: the [`HealthClient`] impl (poll / fetch / count / list / stop methods),
//! the [`client_for`] constructor, and the `project_*` helpers that turn raw
//! JSON payloads into typed counts and [`CollectionRow`]s.
//! Test: `health_client_stores_base_url`, `poll_unreachable_daemon_is_offline`,
//! `project_memory_counts_reads_status_fields`, `project_palace_rows_reads_palaces`,
//! `project_log_tail_reads_fields`, `project_edge_kinds_sorts_desc`.

use std::time::Duration;

use crate::tui::health::format::format_relative_time;
use crate::tui::health::types::{
    CollectionRow, Daemon, HealthClient, HealthWire, PanelData, PanelState,
};

/// Per-request timeout for a daemon health probe.
///
/// Why: a hung daemon must not stall the poll task; a short timeout turns an
/// unresponsive daemon into a clean "offline" state on the next tick.
/// What: three seconds, comfortably above a healthy local round-trip.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(3);

impl HealthClient {
    /// Build a client targeting `base` for the given `daemon`.
    ///
    /// Why: the health screen is pointed at a daemon address from a CLI flag or
    /// the documented default.
    /// What: stores the base URL and a pooled `reqwest::Client` whose request
    /// timeout bounds a hung daemon.
    /// Test: `health_client_stores_base_url`.
    pub fn new(base: impl Into<String>, daemon: Daemon) -> Self {
        let http = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .unwrap_or_default();
        Self {
            base: base.into(),
            daemon,
            http,
        }
    }

    /// The base URL this client targets.
    ///
    /// Why: the offline panel renders the daemon address it failed to reach.
    /// What: returns the stored base URL.
    /// Test: `health_client_stores_base_url`.
    pub fn base_url(&self) -> &str {
        &self.base
    }

    /// Poll the daemon and project the result into a [`PanelState`].
    ///
    /// Why: the background task wants one infallible call per tick that always
    /// yields a renderable state — `Online` on success, `Offline` on any
    /// transport or decode failure.
    /// What: GETs `/health`, then the daemon-specific list endpoints for the
    /// key counts, folding everything into [`PanelData`]. Any error along the
    /// way becomes `Offline` carrying the error string.
    /// Test: live behaviour is covered by the daemon suites; the offline path
    /// is exercised by `poll_unreachable_daemon_is_offline`.
    pub async fn poll(&self) -> PanelState {
        match self.fetch().await {
            Ok(data) => PanelState::Online(data),
            Err(e) => PanelState::Offline {
                last_error: e.to_string(),
            },
        }
    }

    /// Fetch and project the panel payload, returning a `Result` for `?`.
    ///
    /// Why: keeps [`Self::poll`]'s error-to-`Offline` mapping in one place
    /// while the happy path stays terse with `?`.
    /// What: GETs `/health` and the daemon's list endpoints; for search the
    /// counts are index count + summed chunk counts, for memory they come from
    /// `/api/v1/status`.
    /// Test: covered indirectly by `poll`; the count projections are unit-tested
    /// via `project_search_counts` / `project_memory_counts`.
    async fn fetch(&self) -> anyhow::Result<PanelData> {
        let health_path = match self.daemon {
            Daemon::Search => "/health",
            Daemon::Memory => "/health",
        };
        let health: HealthWire = self
            .http
            .get(format!("{}{health_path}", self.base))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;

        let (count_a, count_b, count_c, count_d) = match self.daemon {
            Daemon::Search => self.search_counts().await,
            Daemon::Memory => self.memory_counts().await,
        };

        Ok(PanelData {
            version: health.version,
            rss_mb: health.rss_mb,
            cpu_pct: health.cpu_pct,
            uptime_secs: health.uptime_secs,
            disk_bytes: health.disk_bytes,
            count_a,
            count_b,
            count_c,
            count_d,
        })
    }

    /// Resolve the search key counts: `(indexes, total_chunks, 0, 0)`.
    ///
    /// Why: the search panel shows index count and summed chunk count; a
    /// failure to enumerate indexes degrades to zeroes rather than failing the
    /// whole poll, since the resource block already rendered.
    /// What: GETs `/indexes`, then `/indexes/:id/status` per index, summing
    /// `chunk_count`. Any error yields all zeroes.
    /// Test: the JSON projection is unit-tested via `project_search_counts`.
    async fn search_counts(&self) -> (u64, u64, u64, u64) {
        let Ok(list) = self.get_json(format!("{}/indexes", self.base)).await else {
            return (0, 0, 0, 0);
        };
        let ids = list
            .get("indexes")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let mut total_chunks = 0u64;
        for id in &ids {
            if let Ok(status) = self
                .get_json(format!("{}/indexes/{id}/status", self.base))
                .await
            {
                total_chunks = total_chunks.saturating_add(
                    status
                        .get("chunk_count")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0),
                );
            }
        }
        (ids.len() as u64, total_chunks, 0, 0)
    }

    /// Resolve the memory key counts from `/api/v1/status`.
    ///
    /// Why: the memory panel shows palaces, vectors, drawers, and KG triples;
    /// the status endpoint returns all four in one call.
    /// What: GETs `/api/v1/status` and projects `palace_count`, `total_vectors`,
    /// `total_drawers`, `total_kg_triples`. Any error yields all zeroes.
    /// Test: the JSON projection is unit-tested via `project_memory_counts`.
    async fn memory_counts(&self) -> (u64, u64, u64, u64) {
        match self.get_json(format!("{}/api/v1/status", self.base)).await {
            Ok(status) => project_memory_counts(&status),
            Err(_) => (0, 0, 0, 0),
        }
    }

    /// GET `url` and decode the response body as JSON.
    ///
    /// Why: the count probes share the same GET-and-decode shape.
    /// What: GETs `url`, maps a non-2xx response to an error, and decodes the
    /// body into a [`serde_json::Value`].
    /// Test: covered indirectly by `search_counts` / `memory_counts`.
    async fn get_json(&self, url: String) -> anyhow::Result<serde_json::Value> {
        Ok(self
            .http
            .get(url)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?)
    }

    /// Fetch the most recent `n` log lines from the daemon.
    ///
    /// Why: the Logs tab (`[2]`) tails the daemon's in-memory log ring via
    /// `GET /logs/tail?n=…`; both daemons share this endpoint (issue #35).
    /// What: GETs `/logs/tail?n=…` and projects `lines` + `total`. A daemon
    /// without this endpoint (older build) yields `Ok((vec![], 0))` rather
    /// than an error so the tab degrades to a placeholder cleanly.
    /// Test: live behaviour is covered by the daemon suites; the projection
    /// is unit-tested via `project_log_tail`.
    pub async fn logs_tail(&self, n: u32) -> anyhow::Result<(Vec<String>, u64)> {
        let url = format!("{}/logs/tail?n={n}", self.base);
        match self.http.get(url).send().await {
            Ok(resp) if resp.status().is_success() => {
                match resp.json::<serde_json::Value>().await {
                    Ok(body) => Ok(project_log_tail(&body)),
                    Err(_) => Ok((Vec::new(), 0)),
                }
            }
            // 404 or older daemon: no logs endpoint — degrade to empty.
            Ok(_) => Ok((Vec::new(), 0)),
            Err(e) => Err(anyhow::anyhow!("logs_tail: {e}")),
        }
    }

    /// Fetch the search daemon's index list with chunk counts.
    ///
    /// Why: the Collections list (left panel for the search service) wants a
    /// per-index name + chunk count so the operator can see at a glance
    /// which corpora are loaded.
    /// What: GETs `/indexes`, then `GET /indexes/:id/status` per index,
    /// projecting `(id, chunk_count)` into [`CollectionRow`]s. Any error
    /// yields an empty list rather than failing.
    /// Test: live behaviour is covered by the daemon suites; the projection
    /// is unit-tested via `project_index_rows`.
    pub async fn search_collections(&self) -> Vec<CollectionRow> {
        let Ok(list) = self.get_json(format!("{}/indexes", self.base)).await else {
            return Vec::new();
        };
        let ids: Vec<String> = list
            .get("indexes")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        let mut rows = Vec::with_capacity(ids.len());
        for id in ids {
            // Status: chunk count + last_indexed + disk bytes + context embedding.
            let status = self
                .get_json(format!("{}/indexes/{id}/status", self.base))
                .await
                .ok();
            let count = status
                .as_ref()
                .and_then(|v| v.get("chunk_count").and_then(|c| c.as_u64()))
                .unwrap_or(0);
            let last_indexed = status
                .as_ref()
                .and_then(|v| v.get("last_indexed").and_then(|c| c.as_str()))
                .map(str::to_string);
            let disk_bytes = status
                .as_ref()
                .and_then(|v| v.get("disk_bytes").and_then(|c| c.as_u64()))
                .unwrap_or(0);
            let has_context_embedding = status
                .as_ref()
                .and_then(|v| v.get("has_context_embedding").and_then(|c| c.as_bool()))
                .unwrap_or(false);

            // Graph stats: nodes, edges, edge kind histogram. Errors → zeroes.
            let graph = self
                .get_json(format!("{}/indexes/{id}/graph/stats", self.base))
                .await
                .ok();
            let node_count = graph
                .as_ref()
                .and_then(|v| v.get("node_count").and_then(|c| c.as_u64()))
                .unwrap_or(0);
            let edge_count = graph
                .as_ref()
                .and_then(|v| v.get("edge_count").and_then(|c| c.as_u64()))
                .unwrap_or(0);
            let edge_kinds = graph.as_ref().map(project_edge_kinds).unwrap_or_default();

            // Communities: only the top-level summary fields are needed.
            let communities = self
                .get_json(format!("{}/indexes/{id}/communities", self.base))
                .await
                .ok();
            let community_count = communities
                .as_ref()
                .and_then(|v| v.get("community_count").and_then(|c| c.as_u64()))
                .unwrap_or(0);
            let modularity = communities
                .as_ref()
                .and_then(|v| v.get("modularity").and_then(|c| c.as_f64()))
                .unwrap_or(0.0);

            let note = format_relative_time(last_indexed.as_deref());
            rows.push(CollectionRow {
                id,
                count,
                note,
                ok: true,
                last_indexed,
                node_count,
                edge_count,
                edge_kinds,
                community_count,
                modularity,
                disk_bytes,
                has_context_embedding,
                ..Default::default()
            });
        }
        rows
    }

    /// Fetch the memory daemon's palace list with vector and KG counts.
    ///
    /// Why: the Collections list (left panel for the memory service) needs the
    /// per-palace name, vector count, and KG triple count so the operator can
    /// see at a glance which palaces hold the most memory and which carry a
    /// knowledge graph. `/api/v1/status` only exposes aggregate totals; the
    /// per-palace breakdown lives at `/api/v1/palaces`.
    /// What: GETs `/api/v1/palaces` (a JSON array of `PalaceInfo`) and
    /// projects each entry into a [`CollectionRow`]. Any error yields an empty
    /// list.
    /// Test: the projection is unit-tested via `project_palace_rows`.
    pub async fn memory_collections(&self) -> Vec<CollectionRow> {
        let Ok(list) = self.get_json(format!("{}/api/v1/palaces", self.base)).await else {
            return Vec::new();
        };
        project_palace_rows(&list)
    }

    /// Request a graceful shutdown of the daemon via its `admin/stop` endpoint.
    ///
    /// Why: the `[X]` key stops the focused daemon without the operator
    /// resolving a PID; both daemons expose an unauthenticated stop route.
    /// What: POSTs an empty body to the daemon's stop path (`/admin/stop` for
    /// search, `/api/v1/admin/stop` for memory). A non-2xx response is an error.
    /// Test: live behaviour is covered by the daemon suites; the dashboard
    /// records the outcome string in `last_action`.
    pub async fn stop(&self) -> anyhow::Result<()> {
        let path = match self.daemon {
            Daemon::Search => "/admin/stop",
            Daemon::Memory => "/api/v1/admin/stop",
        };
        self.http
            .post(format!("{}{path}", self.base))
            .json(&serde_json::json!({}))
            .send()
            .await?
            .error_for_status()?;
        Ok(())
    }
}

/// Build a [`HealthClient`] for the given daemon at the given base URL.
///
/// Why: the background poller and the `[S]`/`[X]` key handlers all need a
/// client; centralising construction keeps the daemon→client mapping in one
/// place.
/// What: returns a [`HealthClient`] tagged with `daemon`.
/// Test: covered by `health_client_stores_base_url`.
pub fn client_for(daemon: Daemon, base_url: &str) -> HealthClient {
    HealthClient::new(base_url, daemon)
}

/// Project a `/api/v1/status` payload into `(palaces, vectors, drawers, kg)`.
///
/// Why: centralising the projection keeps [`HealthClient::memory_counts`]
/// testable without a live daemon and resilient to absent optional fields.
/// What: reads `palace_count`, `total_vectors`, `total_drawers`, and
/// `total_kg_triples`, defaulting any absent field to zero.
/// Test: `project_memory_counts`.
pub(crate) fn project_memory_counts(status: &serde_json::Value) -> (u64, u64, u64, u64) {
    let u = |key: &str| status.get(key).and_then(|v| v.as_u64()).unwrap_or(0);
    (
        u("palace_count"),
        u("total_vectors"),
        u("total_drawers"),
        u("total_kg_triples"),
    )
}

/// Project a `/logs/tail` response into `(lines, total)`.
///
/// Why: keeps the wire-shape parsing in one testable function so the client
/// stays terse and an older daemon's quirky payload cannot crash the TUI.
/// What: reads `lines` (array of strings, defaulting to `[]`) and `total`
/// (u64, defaulting to `lines.len()`).
/// Test: `project_log_tail_reads_fields`.
pub(crate) fn project_log_tail(body: &serde_json::Value) -> (Vec<String>, u64) {
    let lines: Vec<String> = body
        .get("lines")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let total = body
        .get("total")
        .and_then(|v| v.as_u64())
        .unwrap_or(lines.len() as u64);
    (lines, total)
}

/// Project a memory daemon `/api/v1/palaces` payload into palace rows.
///
/// Why: centralising the projection keeps the renderer terse and lets a unit
/// test assert the shape without a live daemon. The wire format is a JSON
/// array of `PalaceInfo` objects with per-palace `vector_count`,
/// `drawer_count`, and `kg_triple_count`. `/api/v1/status` exposes only
/// aggregate totals, so this is the only source of per-palace counts. Empty
/// palaces (no vectors AND no KG triples) are filtered out so the left pane
/// only lists palaces that actually hold memory — an empty palace is
/// indistinguishable from a placeholder and just adds visual noise.
/// What: reads the top-level array, projecting each entry's `name` (falling
/// back to `id`), `vector_count`, and `kg_triple_count` (any absent field
/// defaults to zero). Rows where both counts are zero are dropped. A
/// non-array payload yields an empty list.
/// Test: `project_palace_rows_reads_palaces`,
/// `project_palace_rows_filters_empty`.
pub(crate) fn project_palace_rows(list: &serde_json::Value) -> Vec<CollectionRow> {
    let Some(arr) = list.as_array() else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|p| {
            let id = p
                .get("name")
                .or_else(|| p.get("id"))
                .and_then(|v| v.as_str())
                .unwrap_or("?")
                .to_string();
            let count = p.get("vector_count").and_then(|v| v.as_u64()).unwrap_or(0);
            let kg_count = p
                .get("kg_triple_count")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let drawer_count = p.get("drawer_count").and_then(|v| v.as_u64()).unwrap_or(0);
            let wing_count = p.get("wing_count").and_then(|v| v.as_u64()).unwrap_or(0);
            let last_write_at = p
                .get("last_write_at")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let node_count = p.get("node_count").and_then(|v| v.as_u64()).unwrap_or(0);
            let edge_count = p.get("edge_count").and_then(|v| v.as_u64()).unwrap_or(0);
            let community_count = p
                .get("community_count")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let is_compacting = p
                .get("is_compacting")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            // Skip palaces with no vectors and no graph triples: they hold
            // nothing the operator can act on and clutter the left pane.
            if count == 0 && kg_count == 0 {
                return None;
            }
            Some(CollectionRow {
                id,
                count,
                kg_count,
                drawer_count,
                wing_count,
                last_write_at,
                node_count,
                edge_count,
                community_count,
                is_compacting,
                // Note left empty: the row shows vector + graph counts inline
                // (e.g. `12v 34g`), so a trailing badge would be redundant.
                note: String::new(),
                ok: true,
                ..Default::default()
            })
        })
        .collect()
}

/// Project a `/indexes/:id/graph/stats` payload's `edge_kinds` map into a
/// vec sorted by count descending.
///
/// Why: the INDEX tab renders one row per edge kind, ordered so the heaviest
/// relationship appears at the top; keeping the projection pure makes it
/// testable without a live daemon.
/// What: reads the `edge_kinds` object from `stats`, collects `(name, count)`
/// pairs, and sorts by count descending (ties broken by name ascending).
/// A missing object yields an empty vec.
/// Test: `project_edge_kinds_sorts_desc`.
pub(crate) fn project_edge_kinds(stats: &serde_json::Value) -> Vec<(String, u64)> {
    let Some(map) = stats.get("edge_kinds").and_then(|v| v.as_object()) else {
        return Vec::new();
    };
    let mut pairs: Vec<(String, u64)> = map
        .iter()
        .map(|(k, v)| (k.clone(), v.as_u64().unwrap_or(0)))
        .collect();
    pairs.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    pairs
}
