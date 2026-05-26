//! HTTP client for the trusty-memory daemon.
//!
//! Why: the unified monitor dashboard needs a typed, testable transport to the
//! trusty-memory daemon's read-only endpoints (`/health`, `/api/v1/status`,
//! `/api/v1/palaces`). Keeping it in its own module mirrors `search_client` and
//! isolates the wire shapes the memory panel renders.
//! What: [`MemoryClient`] wraps a base URL and a pooled `reqwest::Client`; a
//! `fetch_all` helper folds the status and palace calls into [`MemoryData`].
//! Test: `cargo test -p trusty-monitor-tui` covers URL resolution and base-URL
//! storage; live endpoints are covered by the daemon's own suite.

use std::time::Duration;

use serde::Deserialize;

use crate::monitor::dashboard::{MemoryData, PalaceRow};

/// Default trusty-memory daemon address used when discovery fails.
///
/// Why: trusty-memory binds a dynamic port, so the lock file is the primary
/// discovery path; the default only applies when no daemon has ever started.
/// What: a well-known local fallback base URL for trusty-memory.
/// Test: `default_memory_url_is_local`.
pub const DEFAULT_MEMORY_URL: &str = "http://127.0.0.1:7070";

/// Per-request timeout for trusty-memory probes.
///
/// Why: a hung daemon must not stall the dashboard refresh tick.
/// What: three seconds, matching `search_client`.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(3);

/// Resolve the trusty-memory daemon base URL.
///
/// Why: trusty-memory's port is dynamic, so its address is read from the
/// service lock file; the default applies only when discovery yields nothing.
/// What: reads the `trusty-memory` daemon address; returns it `http://`-prefixed
/// when present, otherwise [`DEFAULT_MEMORY_URL`].
/// Test: `resolve_memory_url_returns_http_url`.
pub fn resolve_memory_url() -> String {
    match crate::read_daemon_addr("trusty-memory") {
        Ok(Some(addr)) => normalize_url(&addr),
        _ => DEFAULT_MEMORY_URL.to_string(),
    }
}

/// Ensure a daemon address carries an `http://` scheme.
///
/// Why: the lock file stores a bare `host:port`; `reqwest` needs a full URL.
/// What: returns `raw` unchanged when it already has a scheme, else prefixes
/// `http://`.
/// Test: `normalize_url_adds_scheme`.
pub fn normalize_url(raw: &str) -> String {
    if raw.starts_with("http://") || raw.starts_with("https://") {
        raw.to_string()
    } else {
        format!("http://{raw}")
    }
}

/// Wire shape of `GET /api/v1/status` from the trusty-memory daemon.
#[derive(Debug, Deserialize)]
struct StatusWire {
    #[serde(default)]
    version: String,
    #[serde(default)]
    palace_count: u64,
    #[serde(default)]
    total_drawers: u64,
    #[serde(default)]
    total_vectors: u64,
    #[serde(default)]
    total_kg_triples: u64,
}

/// Wire shape of one palace entry from `GET /api/v1/palaces`.
///
/// Why: the palace list response shape varies slightly between daemon
/// versions; all fields are optional with defaults so a partial payload still
/// deserializes rather than failing the whole poll.
#[derive(Debug, Default, Deserialize)]
struct PalaceWire {
    #[serde(default)]
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default, alias = "vectors", alias = "total_vectors")]
    vector_count: u64,
    #[serde(default)]
    drawer_count: u64,
    #[serde(default)]
    last_write_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default)]
    description: Option<String>,
    /// Number of active KG triples; `#[serde(default)]` for forward-compat.
    #[serde(default)]
    kg_triple_count: u64,
    /// KG node count; `#[serde(default)]` for forward-compat.
    #[serde(default)]
    node_count: u64,
    /// KG edge count; `#[serde(default)]` for forward-compat.
    #[serde(default)]
    edge_count: u64,
    /// Detected community count; `#[serde(default)]` for forward-compat.
    #[serde(default)]
    community_count: u64,
    /// Whether a dream cycle is currently running; `#[serde(default)]` for
    /// forward-compat against pre-spinner daemon builds.
    #[serde(default)]
    is_compacting: bool,
}

/// Typed HTTP client for the trusty-memory daemon.
///
/// Why: the dashboard polls trusty-memory every refresh tick; a reusable client
/// keeps the probe cheap and the call sites tidy.
/// What: holds a mutable base URL plus a shared `reqwest::Client`; exposes the
/// read endpoints the memory panel renders.
/// Test: `memory_client_stores_base_url`.
#[derive(Debug, Clone)]
pub struct MemoryClient {
    base: String,
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

/// One recalled memory from a trusty-memory query, projected for the log.
///
/// Why: the memory TUI renders a compact one-line summary per recall hit; a
/// small typed struct keeps the renderer free of raw JSON.
/// What: the source palace id and a short content snippet with its score.
/// Test: `parse_recall_hits_projects_fields`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RecallHit {
    /// The palace the memory was recalled from.
    pub palace_id: String,
    /// A short, single-line snippet of the recalled content.
    pub snippet: String,
    /// The relevance score of the recall (higher is closer).
    pub score: f32,
}

/// Project a `/api/v1/recall` JSON payload into [`RecallHit`]s.
///
/// Why: the recall endpoint returns a bare array of result objects;
/// centralising the projection keeps the client testable and resilient to
/// absent optional fields.
/// What: accepts a JSON array, and for each entry takes `palace_id`, the first
/// line of `content`, and `score`. Any other shape yields an empty list.
/// Test: `parse_recall_hits_projects_fields`.
pub fn parse_recall_hits(raw: &serde_json::Value) -> Vec<RecallHit> {
    let serde_json::Value::Array(items) = raw else {
        return Vec::new();
    };
    items
        .iter()
        .map(|item| {
            let palace_id = item
                .get("palace_id")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let snippet = item
                .get("content")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .lines()
                .next()
                .unwrap_or_default()
                .trim()
                .to_string();
            let score = item.get("score").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
            RecallHit {
                palace_id,
                snippet,
                score,
            }
        })
        .collect()
}

/// Aggregate counts returned by a `POST /api/v1/dream/run` cycle.
///
/// Why: the memory TUI shows what a dream cycle changed; a typed struct keeps
/// the renderer free of raw JSON.
/// What: the merged / pruned / compacted memory counts.
/// Test: `parse_dream_stats_reads_counts`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DreamStats {
    /// Memories merged into existing ones during the cycle.
    pub merged: u64,
    /// Memories pruned (forgotten) during the cycle.
    pub pruned: u64,
    /// Memories compacted during the cycle.
    pub compacted: u64,
}

/// Project a `/api/v1/dream/run` JSON payload into a [`DreamStats`].
///
/// Why: the dream endpoint returns an object with several aggregate counters;
/// the TUI surfaces three of them.
/// What: reads `merged`, `pruned`, and `compacted`, defaulting absent fields
/// to zero.
/// Test: `parse_dream_stats_reads_counts`.
pub fn parse_dream_stats(raw: &serde_json::Value) -> DreamStats {
    let u64_of = |key: &str| raw.get(key).and_then(|v| v.as_u64()).unwrap_or(0);
    DreamStats {
        merged: u64_of("merged"),
        pruned: u64_of("pruned"),
        compacted: u64_of("compacted"),
    }
}

/// One live event from the trusty-memory `/sse` stream.
///
/// Why: the memory TUI reacts to push events (dream cycles, drawer changes,
/// palace creation) in its activity log; a typed enum lets the renderer format
/// each distinctly without parsing raw JSON in the event loop.
/// What: mirrors the daemon's `DaemonEvent` — the `type`-tagged variants the
/// TUI displays. Unknown / housekeeping frames (`connected`, `lag`) are
/// dropped by [`parse_memory_event`].
/// Test: `parse_memory_event_maps_type_tag`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemoryEvent {
    /// A new palace was created.
    PalaceCreated {
        /// The new palace's friendly name.
        name: String,
    },
    /// A drawer was added to a palace.
    DrawerAdded {
        /// The palace the drawer belongs to.
        palace_id: String,
        /// The palace's drawer count after the addition.
        drawer_count: u64,
        /// Short preview of the drawer's content (whitespace-collapsed,
        /// truncated to ~80 chars). Empty when the daemon did not provide
        /// the field (older daemons predate the wire field).
        content_preview: String,
    },
    /// A drawer was deleted from a palace.
    DrawerDeleted {
        /// The palace the drawer belonged to.
        palace_id: String,
        /// The palace's drawer count after the deletion.
        drawer_count: u64,
    },
    /// A dream cycle completed.
    DreamCompleted {
        /// Memories merged during the cycle.
        merged: u64,
        /// Memories pruned during the cycle.
        pruned: u64,
        /// Memories compacted during the cycle.
        compacted: u64,
    },
}

/// Parse one `/sse` `data:` JSON object into a [`MemoryEvent`].
///
/// Why: the daemon serializes `DaemonEvent` as `{"type": "...", ...fields}`;
/// the TUI needs the four user-facing variants and ignores housekeeping
/// frames, so this folds the wire shape into [`MemoryEvent`].
/// What: dispatches on the `type` tag — `palace_created`, `drawer_added`,
/// `drawer_deleted`, `dream_completed`. Returns `None` for `connected`, `lag`,
/// `status_changed`, or any unrecognised tag.
/// Test: `parse_memory_event_maps_type_tag`.
pub fn parse_memory_event(value: &serde_json::Value) -> Option<MemoryEvent> {
    let tag = value.get("type").and_then(|v| v.as_str())?;
    let str_of = |key: &str| {
        value
            .get(key)
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string()
    };
    let u64_of = |key: &str| value.get(key).and_then(|v| v.as_u64()).unwrap_or(0);
    match tag {
        "palace_created" => Some(MemoryEvent::PalaceCreated {
            name: str_of("name"),
        }),
        "drawer_added" => Some(MemoryEvent::DrawerAdded {
            palace_id: str_of("palace_id"),
            drawer_count: u64_of("drawer_count"),
            content_preview: str_of("content_preview"),
        }),
        "drawer_deleted" => Some(MemoryEvent::DrawerDeleted {
            palace_id: str_of("palace_id"),
            drawer_count: u64_of("drawer_count"),
        }),
        "dream_completed" => Some(MemoryEvent::DreamCompleted {
            merged: u64_of("merged"),
            pruned: u64_of("pruned"),
            compacted: u64_of("compacted"),
        }),
        _ => None,
    }
}

/// Project a palace-list JSON payload into [`PalaceRow`]s.
///
/// Why: the trusty-memory palace endpoint has shipped both a bare-array shape
/// and an object-wrapped shape across versions; centralising the parsing keeps
/// the client resilient to either and makes it unit-testable without a daemon.
/// What: accepts a JSON array of palace objects, or an object carrying a
/// `palaces` array, and returns the projected rows; any other shape yields an
/// empty list.
/// Test: `palace_list_accepts_array_and_object_shapes`.
pub fn parse_palaces(raw: &serde_json::Value) -> Vec<PalaceRow> {
    let array = match raw {
        serde_json::Value::Array(items) => items.clone(),
        serde_json::Value::Object(obj) => match obj.get("palaces") {
            Some(serde_json::Value::Array(items)) => items.clone(),
            _ => Vec::new(),
        },
        _ => Vec::new(),
    };
    array
        .into_iter()
        .filter_map(|v| serde_json::from_value::<PalaceWire>(v).ok())
        .map(|p| PalaceRow {
            id: p.id,
            name: p.name,
            vector_count: p.vector_count,
            drawer_count: p.drawer_count,
            last_write_at: p.last_write_at,
            description: p.description,
            kg_triple_count: p.kg_triple_count,
            node_count: p.node_count,
            edge_count: p.edge_count,
            community_count: p.community_count,
            is_compacting: p.is_compacting,
        })
        .collect()
}

/// One drawer row projected for the TUI activity panel.
///
/// Why: the activity panel renders a compact one-line summary per drawer —
/// truncated id, creation timestamp, creator tag, and memory count.
/// Keeping a typed projection out of the renderer means the parsing /
/// creator-tag extraction logic is unit-testable without a live daemon.
/// What: a stable id, a created_at timestamp (UTC), the resolved creator
/// label (`"—"` when no creator tag was found), the drawer's tag list
/// surfaced so future panels can present richer detail without
/// re-fetching, and an optional content snippet for inline display
/// (issue #202; `None` when the body was empty or the daemon predates
/// the snippet field).
/// Test: `parse_drawers_projects_fields`, `creator_label_picks_first_match`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DrawerInfo {
    /// Stable drawer identifier (UUID as string).
    pub id: String,
    /// Creation timestamp as parsed from the wire payload.
    pub created_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Resolved creator label (e.g. `"msg:from=cto"`, `"creator:client=mpm"`)
    /// or `"—"` when no recognised creator tag was attached.
    pub creator: String,
    /// All tags as carried on the wire (for downstream filtering / display).
    pub tags: Vec<String>,
    /// Short whitespace-collapsed snippet of the drawer body (issue #202).
    ///
    /// Populated by the daemon's `/api/v1/palaces/{id}/drawers` endpoint
    /// (truncated to ~60 chars with `…`). The client falls back to
    /// truncating the full `content` field when the daemon predates the
    /// `snippet` wire field; `None` when neither is available.
    pub snippet: Option<String>,
}

/// Fallback creator label rendered when no recognised creator tag is found.
///
/// Why: the panel must distinguish "writer didn't self-identify" from a real
/// label; using an em-dash mirrors the convention used by the statistics
/// panel for "never written" timestamps.
/// What: the literal em-dash glyph.
/// Test: `creator_label_picks_first_match`.
pub const NO_CREATOR_LABEL: &str = "—";

/// Maximum characters retained when the client falls back to truncating
/// `content` because the daemon didn't return a `snippet` (issue #202).
///
/// Why: the activity panel must show a usable snippet against older
/// daemons that predate the wire field. Matching the server's
/// `DRAWER_SNIPPET_MAX_CHARS` keeps the rendered width consistent across
/// daemon versions.
/// What: 60 characters; matches `trusty_memory::service::DRAWER_SNIPPET_MAX_CHARS`.
/// Test: `parse_drawers_projects_fields`.
const DRAWER_SNIPPET_FALLBACK_MAX: usize = 60;

/// Project a `…/drawers` JSON payload into [`DrawerInfo`]s.
///
/// Why: the endpoint may return either a bare JSON array (the
/// trusty-memory daemon's current shape) or — defensively — an object with a
/// `drawers` array. The projection tolerates both, and absent / malformed
/// fields fall through to safe defaults so a single corrupt row does not
/// drop the whole page.
/// What: accepts a JSON array (or object with `drawers`); for each entry
/// reads `id` (UUID string), `created_at` (RFC 3339), and `tags`, then
/// resolves the creator label via [`creator_label`]. The optional
/// `snippet` field is preferred; when absent the client falls back to
/// truncating `content` to [`DRAWER_SNIPPET_FALLBACK_MAX`] chars so older
/// daemons still surface a usable snippet (issue #202).
/// Test: `parse_drawers_projects_fields`.
pub fn parse_drawers(raw: &serde_json::Value) -> Vec<DrawerInfo> {
    let array: Vec<serde_json::Value> = match raw {
        serde_json::Value::Array(items) => items.clone(),
        serde_json::Value::Object(obj) => match obj.get("drawers") {
            Some(serde_json::Value::Array(items)) => items.clone(),
            _ => Vec::new(),
        },
        _ => Vec::new(),
    };
    array
        .into_iter()
        .map(|item| {
            let id = item
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let created_at = item
                .get("created_at")
                .and_then(|v| v.as_str())
                .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                .map(|dt| dt.with_timezone(&chrono::Utc));
            let tags: Vec<String> = item
                .get("tags")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|t| t.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default();
            let creator = creator_label(&tags);
            // Issue #202: prefer the daemon-supplied `snippet` field
            // (already truncated and whitespace-collapsed). Fall back
            // to a client-side truncation of `content` for daemons that
            // predate the field. `null` / empty / whitespace-only
            // values yield `None` so the renderer can omit the column.
            let snippet = item
                .get("snippet")
                .and_then(|v| v.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .or_else(|| {
                    item.get("content")
                        .and_then(|v| v.as_str())
                        .map(client_snippet)
                        .filter(|s| !s.is_empty())
                });
            DrawerInfo {
                id,
                created_at,
                creator,
                tags,
                snippet,
            }
        })
        .collect()
}

/// Client-side fallback snippet builder for daemons that predate the
/// `snippet` wire field (issue #202).
///
/// Why: keep the activity panel useful across daemon versions; the
/// projection should still surface a glanceable summary even when the
/// daemon returns only the raw `content`.
/// What: whitespace-collapses, trims, and truncates to
/// [`DRAWER_SNIPPET_FALLBACK_MAX`] with a trailing `…` when cut. Empty
/// / whitespace-only inputs yield the empty string so the caller can
/// drop the snippet via `filter(|s| !s.is_empty())`.
/// Test: covered by `parse_drawers_projects_fields` via the fallback path.
fn client_snippet(content: &str) -> String {
    let normalised: String = content.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalised.chars().count() <= DRAWER_SNIPPET_FALLBACK_MAX {
        normalised
    } else {
        let kept: String = normalised
            .chars()
            .take(DRAWER_SNIPPET_FALLBACK_MAX.saturating_sub(1))
            .collect();
        format!("{kept}…")
    }
}

/// Pick the first recognised creator tag from a drawer's tag list.
///
/// Why: trusty-memory drawers carry their writer's identity in tag form —
/// `msg:from=<peer>` for cross-instance messages, `creator:client=<name>` /
/// `creator:source=<src>` for HTTP / MCP writers, and `tag:creator:…` for the
/// legacy CTO/MPM convention. The TUI surfaces whichever match comes first
/// so the operator can trace authorship at a glance.
/// What: scans `tags` in order looking for a prefix in
/// (`msg:from=`, `tag:creator:`, `creator:`). Returns the matching tag
/// verbatim or [`NO_CREATOR_LABEL`] when none match.
/// Test: `creator_label_picks_first_match`.
pub fn creator_label(tags: &[String]) -> String {
    for tag in tags {
        if tag.starts_with("msg:from=")
            || tag.starts_with("tag:creator:")
            || tag.starts_with("creator:")
        {
            return tag.clone();
        }
    }
    NO_CREATOR_LABEL.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_memory_url_is_local() {
        assert!(DEFAULT_MEMORY_URL.starts_with("http://127.0.0.1"));
    }

    #[test]
    fn normalize_url_adds_scheme() {
        assert_eq!(normalize_url("127.0.0.1:7070"), "http://127.0.0.1:7070");
        assert_eq!(
            normalize_url("http://127.0.0.1:7070"),
            "http://127.0.0.1:7070"
        );
    }

    #[test]
    fn memory_client_stores_base_url() {
        let client = MemoryClient::new("http://127.0.0.1:7070");
        assert_eq!(client.base_url(), "http://127.0.0.1:7070");
    }

    #[test]
    fn memory_client_repoints() {
        let mut client = MemoryClient::new("http://127.0.0.1:7070");
        client.set_base_url("http://127.0.0.1:8080");
        assert_eq!(client.base_url(), "http://127.0.0.1:8080");
    }

    #[test]
    fn resolve_memory_url_returns_http_url() {
        let url = resolve_memory_url();
        assert!(url.starts_with("http://") || url.starts_with("https://"));
    }

    #[test]
    fn palace_list_accepts_array_and_object_shapes() {
        // Bare-array shape.
        let arr = serde_json::json!([
            {"id": "p1", "name": "default", "vector_count": 8400},
            {"id": "p2", "name": "work", "vectors": 0},
        ]);
        let rows = parse_palaces(&arr);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].id, "p1");
        assert_eq!(rows[0].vector_count, 8400);
        // The `vectors` alias is honoured.
        assert_eq!(rows[1].name, "work");

        // Object-wrapped shape.
        let obj = serde_json::json!({
            "palaces": [{"id": "p3", "name": "notes", "total_vectors": 12}],
        });
        let rows = parse_palaces(&obj);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].vector_count, 12);

        // An unexpected shape yields no rows rather than panicking.
        assert!(parse_palaces(&serde_json::json!("nonsense")).is_empty());
    }

    #[test]
    fn parse_recall_hits_projects_fields() {
        // The recall endpoint returns a bare array; each hit projects
        // palace_id, a one-line snippet, and the score.
        let raw = serde_json::json!([
            {
                "palace_id": "default",
                "content": "JWT middleware added to auth flow\nmore detail",
                "score": 0.83,
            },
            {
                "palace_id": "work",
                "content": "  single line  ",
                "score": 0.5,
            },
        ]);
        let hits = parse_recall_hits(&raw);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].palace_id, "default");
        assert_eq!(hits[0].snippet, "JWT middleware added to auth flow");
        assert!((hits[0].score - 0.83).abs() < 1e-6);
        assert_eq!(hits[1].snippet, "single line");
        // A non-array payload yields no hits.
        assert!(parse_recall_hits(&serde_json::json!({})).is_empty());
    }

    #[test]
    fn parse_dream_stats_reads_counts() {
        let raw = serde_json::json!({
            "merged": 3, "pruned": 1, "compacted": 0,
            "closets_updated": 5, "duration_ms": 42,
        });
        assert_eq!(
            parse_dream_stats(&raw),
            DreamStats {
                merged: 3,
                pruned: 1,
                compacted: 0,
            }
        );
        // Absent fields default to zero.
        assert_eq!(
            parse_dream_stats(&serde_json::json!({})),
            DreamStats::default()
        );
    }

    #[test]
    fn parse_memory_event_maps_type_tag() {
        assert_eq!(
            parse_memory_event(&serde_json::json!({
                "type": "palace_created", "id": "p1", "name": "notes",
            })),
            Some(MemoryEvent::PalaceCreated {
                name: "notes".into(),
            })
        );
        // drawer_added with a content preview round-trips the preview.
        assert_eq!(
            parse_memory_event(&serde_json::json!({
                "type": "drawer_added",
                "palace_id": "default",
                "drawer_count": 14,
                "content_preview": "How the migration system handles…",
            })),
            Some(MemoryEvent::DrawerAdded {
                palace_id: "default".into(),
                drawer_count: 14,
                content_preview: "How the migration system handles…".into(),
            })
        );
        // Older daemons omit `content_preview`; the field defaults to empty.
        assert_eq!(
            parse_memory_event(&serde_json::json!({
                "type": "drawer_added", "palace_id": "default", "drawer_count": 14,
            })),
            Some(MemoryEvent::DrawerAdded {
                palace_id: "default".into(),
                drawer_count: 14,
                content_preview: String::new(),
            })
        );
        assert_eq!(
            parse_memory_event(&serde_json::json!({
                "type": "dream_completed", "merged": 3, "pruned": 1, "compacted": 0,
            })),
            Some(MemoryEvent::DreamCompleted {
                merged: 3,
                pruned: 1,
                compacted: 0,
            })
        );
        // Housekeeping and unmodelled frames are dropped.
        assert!(parse_memory_event(&serde_json::json!({"type": "connected"})).is_none());
        assert!(parse_memory_event(&serde_json::json!({"type": "lag", "skipped": 2})).is_none());
        assert!(parse_memory_event(&serde_json::json!({"no": "type"})).is_none());
    }

    #[test]
    fn parse_drawers_projects_fields() {
        // Bare array shape — the daemon's current response. Row 0
        // carries an explicit `snippet`; row 1 only has `content` (the
        // fallback path); row 2 carries neither.
        let raw = serde_json::json!([
            {
                "id": "11111111-1111-1111-1111-111111111111",
                "created_at": "2026-05-20T12:34:56Z",
                "tags": ["msg:from=cto", "user-tag"],
                "content": "ignored when snippet is present",
                "snippet": "JWT middleware added",
            },
            {
                "id": "22222222-2222-2222-2222-222222222222",
                "created_at": "2026-05-19T08:00:00Z",
                "tags": ["creator:client=mpm", "creator:source=http"],
                "content": "Plain content for the legacy fallback path",
            },
            {
                "id": "33333333-3333-3333-3333-333333333333",
                "created_at": "bad-timestamp",
                "tags": [],
            },
        ]);
        let drawers = parse_drawers(&raw);
        assert_eq!(drawers.len(), 3);
        assert_eq!(drawers[0].id, "11111111-1111-1111-1111-111111111111");
        assert_eq!(drawers[0].creator, "msg:from=cto");
        assert_eq!(drawers[0].tags.len(), 2);
        assert!(drawers[0].created_at.is_some());
        // Issue #202: explicit snippet wins over content.
        assert_eq!(drawers[0].snippet.as_deref(), Some("JWT middleware added"));

        assert_eq!(drawers[1].creator, "creator:client=mpm");
        // Issue #202: fall back to truncating `content` when snippet is absent.
        assert_eq!(
            drawers[1].snippet.as_deref(),
            Some("Plain content for the legacy fallback path"),
        );

        // Malformed timestamp drops to None; missing creator tag → em-dash;
        // no snippet and no content → snippet is None.
        assert!(drawers[2].created_at.is_none());
        assert_eq!(drawers[2].creator, NO_CREATOR_LABEL);
        assert!(drawers[2].snippet.is_none());

        // Object-wrapped shape.
        let obj = serde_json::json!({
            "drawers": [{"id": "abc", "tags": []}],
        });
        let drawers = parse_drawers(&obj);
        assert_eq!(drawers.len(), 1);
        assert_eq!(drawers[0].id, "abc");

        // Unexpected shape yields an empty list.
        assert!(parse_drawers(&serde_json::json!("nope")).is_empty());

        // An explicit `null` snippet (daemon returned `Value::Null`) also
        // yields `None` — neither the snippet nor the absent content
        // fields fill it in.
        let null_snippet = serde_json::json!([{
            "id": "44444444-4444-4444-4444-444444444444",
            "snippet": serde_json::Value::Null,
            "tags": [],
        }]);
        let drawers = parse_drawers(&null_snippet);
        assert!(drawers[0].snippet.is_none());

        // Long content gets truncated by the client fallback.
        let long_content = "x".repeat(200);
        let long = serde_json::json!([{
            "id": "55555555-5555-5555-5555-555555555555",
            "content": long_content,
            "tags": [],
        }]);
        let drawers = parse_drawers(&long);
        let snippet = drawers[0].snippet.as_deref().expect("fallback snippet");
        assert_eq!(snippet.chars().count(), DRAWER_SNIPPET_FALLBACK_MAX);
        assert!(
            snippet.ends_with('…'),
            "long fallback snippet must be truncated with ellipsis",
        );
    }

    #[test]
    fn creator_label_picks_first_match() {
        // First matching tag wins, in the tag list's order.
        let label = creator_label(&[
            "user-tag".into(),
            "msg:from=cto".into(),
            "creator:client=mpm".into(),
        ]);
        assert_eq!(label, "msg:from=cto");

        // `tag:creator:` legacy prefix is recognised.
        let label = creator_label(&["tag:creator:client=mpm".into()]);
        assert_eq!(label, "tag:creator:client=mpm");

        // `creator:` alone (HTTP attribution) is recognised.
        let label = creator_label(&["creator:source=http".into()]);
        assert_eq!(label, "creator:source=http");

        // No recognised tags → em-dash placeholder.
        assert_eq!(
            creator_label(&["user-tag".into(), "kind:note".into()]),
            NO_CREATOR_LABEL,
        );
        assert_eq!(creator_label(&[]), NO_CREATOR_LABEL);
    }
}
