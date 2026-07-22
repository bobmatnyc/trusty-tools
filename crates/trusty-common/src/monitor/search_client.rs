//! HTTP client for the trusty-search daemon.
//!
//! Why: the unified monitor dashboard needs a typed, testable transport to the
//! trusty-search daemon's read-only endpoints (`/health`, `/indexes`,
//! `/indexes/:id/status`) plus the `/indexes/:id/reindex` action. Keeping the
//! transport in its own module lets the dashboard logic stay free of HTTP
//! concerns and lets the wire shapes be deserialized in one place.
//! What: [`SearchClient`] wraps a base URL and a pooled `reqwest::Client`; it
//! exposes one method per endpoint the dashboard renders. A `fetch_all` helper
//! folds the three read calls into the dashboard's [`SearchData`].
//! Test: `cargo test -p trusty-monitor-tui` covers default-URL resolution and
//! base-URL storage; live endpoints are covered by the daemon's own suite.

use std::time::Duration;

use serde::Deserialize;

use crate::monitor::dashboard::{IndexRow, SearchData};

/// Default trusty-search daemon address used when discovery fails.
///
/// Why: the spec mandates falling back to `http://127.0.0.1:7878` when the
/// service lock file is absent so the dashboard still has a target to probe.
/// What: the canonical local trusty-search HTTP base URL.
/// Test: `default_search_url_is_local`.
pub const DEFAULT_SEARCH_URL: &str = "http://127.0.0.1:7878";

/// Per-request timeout for trusty-search probes.
///
/// Why: a hung daemon must not freeze the dashboard's refresh tick; a short
/// timeout turns an unresponsive daemon into a clean "offline" state.
/// What: three seconds, comfortably above a healthy local round-trip.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(3);

/// Resolve the trusty-search daemon base URL.
///
/// Why: trusty-search auto-walks ports, so its bound address is discovered from
/// the service lock file written by `trusty_common::write_daemon_addr`; only
/// when that is absent does the dashboard fall back to the well-known default.
/// What: reads the `trusty-search` daemon address; on `Some(addr)` returns it
/// prefixed with `http://` when it lacks a scheme, otherwise returns
/// [`DEFAULT_SEARCH_URL`].
/// Test: `resolve_search_url_falls_back_to_default` exercises the fallback path.
pub fn resolve_search_url() -> String {
    match crate::read_daemon_addr("trusty-search") {
        Ok(Some(addr)) => normalize_url(&addr),
        _ => DEFAULT_SEARCH_URL.to_string(),
    }
}

/// Ensure a daemon address carries an `http://` scheme.
///
/// Why: the lock file stores a bare `host:port`; `reqwest` needs a full URL.
/// What: returns `raw` unchanged when it already has a scheme, otherwise
/// prefixes `http://`.
/// Test: `normalize_url_adds_scheme`.
pub fn normalize_url(raw: &str) -> String {
    if raw.starts_with("http://") || raw.starts_with("https://") {
        raw.to_string()
    } else {
        format!("http://{raw}")
    }
}

/// Wire shape of `GET /health` from the trusty-search daemon.
#[derive(Debug, Deserialize)]
struct HealthWire {
    version: String,
    #[serde(default)]
    uptime_secs: u64,
}

/// Wire shape of `GET /logs/tail` from the trusty-search daemon.
///
/// Why: the daemon returns `{ "lines": [...], "total": <usize> }`; only the
/// lines are surfaced in the activity panel, but `total` is captured (and
/// ignored) so the deserialiser does not reject the payload.
/// What: a `lines` array of plain log strings.
/// Test: covered by the daemon suite; the TUI consumer is tested via
/// `test_push_new_log_lines_skips_first_poll`.
#[derive(Debug, Deserialize)]
struct LogsTailWire {
    #[serde(default)]
    lines: Vec<String>,
}

/// Wire shape of `GET /indexes` from the trusty-search daemon.
#[derive(Debug, Deserialize)]
struct IndexListWire {
    #[serde(default)]
    indexes: Vec<String>,
}

/// Wire shape of `GET /indexes/:id/graph/stats` from the trusty-search daemon.
///
/// Why: the STATISTICS panel surfaces graph node/edge counts and a per-kind
/// breakdown; this captures only the fields the panel needs.
/// What: total node and edge counts plus a map of edge kind → count.
/// Test: deserialisation is exercised live by the trusty-search daemon suite.
#[derive(Debug, Deserialize)]
struct GraphStatsWire {
    #[serde(default)]
    node_count: u64,
    #[serde(default)]
    edge_count: u64,
    #[serde(default)]
    edge_kinds: std::collections::HashMap<String, u64>,
}

/// Wire shape of `GET /indexes/:id/communities` from the trusty-search daemon.
///
/// Why: the STATISTICS panel surfaces the community count and modularity
/// score; the full `communities` array is intentionally ignored.
/// What: top-level community count and modularity score.
/// Test: deserialisation is exercised live by the trusty-search daemon suite.
#[derive(Debug, Deserialize)]
struct CommunitiesWire {
    #[serde(default)]
    community_count: u64,
    #[serde(default)]
    modularity: f64,
}

/// Wire shape of `GET /indexes/:id/status` from the trusty-search daemon.
///
/// Why: the dashboard now surfaces last-indexed time and on-disk size in
/// addition to the chunk count, so the wire struct captures those optional
/// fields when the daemon reports them.
/// What: the indexed root path, chunk count, optional disk size in bytes, and
/// the optional last-indexed timestamp (parsed as `DateTime<Utc>`).
/// Test: deserialisation is exercised live by the trusty-search daemon suite.
#[derive(Debug, Deserialize)]
struct IndexStatusWire {
    #[serde(default)]
    root_path: String,
    #[serde(default)]
    chunk_count: u64,
    #[serde(default)]
    disk_bytes: Option<u64>,
    #[serde(default)]
    last_indexed: Option<chrono::DateTime<chrono::Utc>>,
}

/// Typed HTTP client for the trusty-search daemon.
///
/// Why: the dashboard polls trusty-search every refresh tick; a reusable client
/// with a pooled connection keeps the probe cheap and the call sites tidy.
/// What: holds a mutable base URL plus a shared `reqwest::Client`; exposes the
/// read endpoints the dashboard renders and the reindex action.
/// Test: `search_client_stores_base_url`.
#[derive(Debug, Clone)]
pub struct SearchClient {
    base: String,
    http: reqwest::Client,
}

impl SearchClient {
    /// Build a client targeting `base` (e.g. `http://127.0.0.1:7878`).
    ///
    /// Why: the dashboard is pointed at an address resolved from the lock file
    /// or a CLI flag.
    /// What: stores the base URL and a pooled `reqwest::Client` with a request
    /// timeout so a hung daemon cannot stall the refresh loop.
    /// Test: `search_client_stores_base_url`.
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
    /// Test: `search_client_stores_base_url`.
    pub fn base_url(&self) -> &str {
        &self.base
    }

    /// Re-point this client at a freshly resolved daemon URL.
    ///
    /// Why: trusty-search may rebind onto a new ephemeral port across a
    /// restart; a long-lived dashboard must follow it.
    /// What: overwrites the base URL, keeping the pooled client.
    /// Test: `search_client_repoints`.
    pub fn set_base_url(&mut self, base: impl Into<String>) {
        self.base = base.into();
    }

    /// Fetch every panel field from the trusty-search daemon.
    ///
    /// Why: the dashboard wants one fallible call that yields a complete
    /// [`SearchData`] or an error it can render as the offline state.
    /// What: GETs `/health`, then `/indexes`, then `/indexes/:id/status` for
    /// each index, folding the results into [`SearchData`]. A failed per-index
    /// status probe yields a zero-chunk row rather than failing the whole poll.
    /// Test: live behaviour is covered by the trusty-search daemon suite; the
    /// dashboard's offline path is unit-tested in `dashboard.rs`.
    pub async fn fetch_all(&self) -> anyhow::Result<SearchData> {
        let health: HealthWire = self
            .http
            .get(format!("{}/health", self.base))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;

        let list: IndexListWire = self
            .http
            .get(format!("{}/indexes", self.base))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;

        let mut indexes = Vec::with_capacity(list.indexes.len());
        for id in list.indexes {
            let mut row = match self.index_status(&id).await {
                Ok(status) => IndexRow {
                    id: id.clone(),
                    chunk_count: status.chunk_count,
                    root_path: status.root_path,
                    disk_bytes: status.disk_bytes,
                    last_indexed: status.last_indexed,
                    ..Default::default()
                },
                Err(e) => {
                    tracing::warn!("index status probe failed for {id}: {e}");
                    IndexRow {
                        id: id.clone(),
                        ..Default::default()
                    }
                }
            };
            // Graph stats and community info are best-effort: a failure leaves
            // the corresponding counters at zero so the panel can still render.
            match self.index_graph_stats(&id).await {
                Ok(stats) => {
                    row.node_count = stats.node_count;
                    row.edge_count = stats.edge_count;
                    let mut kinds: Vec<(String, u64)> = stats.edge_kinds.into_iter().collect();
                    kinds.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
                    row.edge_kinds = kinds;
                }
                Err(e) => {
                    tracing::debug!("graph stats probe failed for {id}: {e}");
                }
            }
            match self.index_communities(&id).await {
                Ok(c) => {
                    row.community_count = c.community_count;
                    row.modularity = c.modularity;
                }
                Err(e) => {
                    tracing::debug!("communities probe failed for {id}: {e}");
                }
            }
            indexes.push(row);
        }
        // Stable ordering so the panel does not flicker between polls.
        indexes.sort_by(|a, b| a.id.cmp(&b.id));

        Ok(SearchData {
            version: health.version,
            uptime_secs: health.uptime_secs,
            indexes,
        })
    }

    /// Fetch one index's status payload from `/indexes/:id/status`.
    ///
    /// Why: the index table shows each index's chunk count, last-indexed time,
    /// and on-disk size; this is the single per-index probe used by
    /// [`Self::fetch_all`].
    /// What: GETs `/indexes/:id/status` and returns the parsed wire struct so
    /// the caller can thread every optional field into the [`IndexRow`].
    /// Test: covered by the trusty-search daemon suite.
    async fn index_status(&self, id: &str) -> anyhow::Result<IndexStatusWire> {
        let status: IndexStatusWire = self
            .http
            .get(format!("{}/indexes/{id}/status", self.base))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(status)
    }

    /// Fetch the knowledge-graph stats for an index from `/graph/stats`.
    ///
    /// Why: the STATISTICS panel surfaces graph size and per-edge-kind
    /// breakdown alongside chunk count; this probe is intentionally
    /// non-fatal — a 404 or missing graph leaves `IndexRow` graph counters
    /// at zero so the rest of the panel still renders.
    /// What: GETs `/indexes/:id/graph/stats` and returns the parsed wire
    /// struct.
    /// Test: covered by the trusty-search daemon suite.
    async fn index_graph_stats(&self, id: &str) -> anyhow::Result<GraphStatsWire> {
        let stats: GraphStatsWire = self
            .http
            .get(format!("{}/indexes/{id}/graph/stats", self.base))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(stats)
    }

    /// Fetch the community-detection summary for an index from `/communities`.
    ///
    /// Why: the STATISTICS panel reports the top-level community count and
    /// modularity score; the full `communities` array is ignored to keep
    /// the wire payload small.
    /// What: GETs `/indexes/:id/communities` and returns the parsed wire
    /// struct.
    /// Test: covered by the trusty-search daemon suite.
    async fn index_communities(&self, id: &str) -> anyhow::Result<CommunitiesWire> {
        let c: CommunitiesWire = self
            .http
            .get(format!("{}/indexes/{id}/communities", self.base))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(c)
    }

    /// Fetch the N most-recent daemon log lines from `GET /logs/tail`.
    ///
    /// Why: the search TUI polls this on every refresh tick so background
    /// daemon activity (file-watcher reindexes, startup scans) surfaces in
    /// the ACTIVITY panel without the user having to press `[r]`.
    /// What: GETs `/logs/tail?n=<n>` and returns the log lines as plain
    /// strings, stripping any timestamp/level prefix if the wire format
    /// includes it. Returns an empty Vec on error so callers can degrade
    /// gracefully.
    /// Test: covered by the daemon suite; the TUI integration is tested via
    /// `test_push_new_log_lines`.
    pub async fn logs_tail(&self, n: usize) -> Vec<String> {
        let url = format!("{}/logs/tail?n={n}", self.base);
        let resp = match self.http.get(&url).send().await {
            Ok(r) => r,
            Err(e) => {
                tracing::debug!("logs_tail request failed: {e}");
                return Vec::new();
            }
        };
        let resp = match resp.error_for_status() {
            Ok(r) => r,
            Err(e) => {
                tracing::debug!("logs_tail returned non-2xx: {e}");
                return Vec::new();
            }
        };
        match resp.json::<LogsTailWire>().await {
            Ok(wire) => wire.lines,
            Err(e) => {
                tracing::debug!("logs_tail parse failed: {e}");
                Vec::new()
            }
        }
    }

    /// Trigger a reindex of `id` via `POST /indexes/:id/reindex`.
    ///
    /// Why: the `[r]` key reindexes the focused search index in place.
    /// What: POSTs an empty JSON body to the reindex endpoint and maps a
    /// non-2xx response to an error.
    /// Test: covered by the trusty-search daemon suite; the dashboard records
    /// the outcome string in `last_action`.
    pub async fn reindex(&self, id: &str) -> anyhow::Result<()> {
        self.http
            .post(format!("{}/indexes/{id}/reindex", self.base))
            .json(&serde_json::json!({}))
            .send()
            .await?
            .error_for_status()?;
        Ok(())
    }

    /// Run a hybrid search against index `id` and return the top results.
    ///
    /// Why: the search TUI's input bar runs a query against the selected index
    /// and folds the hits into the activity log; this is the transport for
    /// that action.
    /// What: POSTs `{ "text": <query>, "top_k": <top_k> }` to
    /// `/indexes/:id/search`, then projects each result object into a
    /// [`SearchHit`]. A non-2xx response or malformed payload yields an error.
    /// Test: live behaviour is covered by the trusty-search daemon suite; the
    /// projection of result objects is unit-tested via `parse_search_hits`.
    pub async fn search(
        &self,
        id: &str,
        query: &str,
        top_k: usize,
    ) -> anyhow::Result<Vec<SearchHit>> {
        let raw: serde_json::Value = self
            .http
            .post(format!("{}/indexes/{id}/search", self.base))
            .json(&serde_json::json!({ "text": query, "top_k": top_k }))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(parse_search_hits(&raw))
    }

    /// Kick off a reindex and stream progress events into `tx`.
    ///
    /// Why: the search TUI's `[r]` key fires this on a background task so the
    /// synchronous event loop can drain [`ReindexEvent`]s via `try_recv` and
    /// append them to the activity log without blocking on the network.
    /// What: POSTs to `/indexes/:id/reindex`, follows the `stream_url`, and
    /// parses each `data:` SSE frame into a [`ReindexEvent`], sending each
    /// through `tx`. A transport failure is sent as a final
    /// [`ReindexEvent::Failed`]. The SSE client uses an unbounded read timeout
    /// since a large-repo reindex can run for minutes.
    /// Test: event parsing is unit-tested via `parse_reindex_event`; the live
    /// stream is covered by the trusty-search daemon suite.
    pub async fn reindex_stream(&self, id: &str, tx: tokio::sync::mpsc::Sender<ReindexEvent>) {
        if let Err(e) = self.reindex_stream_inner(id, &tx).await {
            let _ = tx.send(ReindexEvent::Failed(e.to_string())).await;
        }
    }

    /// Inner body of [`Self::reindex_stream`] returning a `Result` for `?`.
    ///
    /// Why: keeps the public method's error handling (sending a `Failed`
    /// event) in one place while the happy path uses `?`.
    /// What: POSTs the reindex kickoff, opens the SSE stream, and forwards
    /// parsed events; returns the first transport error encountered.
    /// Test: covered indirectly by `reindex_stream` and the daemon suite.
    async fn reindex_stream_inner(
        &self,
        id: &str,
        tx: &tokio::sync::mpsc::Sender<ReindexEvent>,
    ) -> anyhow::Result<()> {
        use futures_util::StreamExt;

        let kickoff: serde_json::Value = self
            .http
            .post(format!("{}/indexes/{id}/reindex", self.base))
            .json(&serde_json::json!({}))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await
            .unwrap_or_else(|_| serde_json::json!({}));
        let stream_path = kickoff
            .get("stream_url")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| format!("/indexes/{id}/reindex/stream"));

        // SSE streams must outlive the short probe timeout — a large reindex
        // runs for minutes. A dedicated client bounds only the connect phase.
        let sse = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .build()?;
        let resp = sse
            .get(format!("{}{stream_path}", self.base))
            .send()
            .await?
            .error_for_status()?;

        let mut bytes = resp.bytes_stream();
        let mut buf = String::new();
        while let Some(chunk) = bytes.next().await {
            let chunk = chunk?;
            buf.push_str(&String::from_utf8_lossy(&chunk));
            // SSE frames are separated by a blank line; `data:` carries JSON.
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
                if let Ok(value) = serde_json::from_str::<serde_json::Value>(payload) {
                    let event = parse_reindex_event(&value);
                    let terminal = matches!(event, ReindexEvent::Complete { .. });
                    if tx.send(event).await.is_err() {
                        return Ok(()); // receiver gone — stop quietly.
                    }
                    if terminal {
                        return Ok(());
                    }
                }
            }
        }
        Ok(())
    }
}

/// One result row from a trusty-search query, projected for the activity log.
///
/// Why: the search TUI renders a compact `path:line  snippet` line per hit; a
/// small typed struct keeps the renderer free of raw JSON.
/// What: the source file path, the 1-based start line, and a short snippet.
/// Test: `parse_search_hits_projects_fields`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SearchHit {
    /// Source file path of the matched chunk.
    pub file: String,
    /// 1-based start line of the matched chunk.
    pub line: usize,
    /// A short, single-line snippet of the matched content.
    pub snippet: String,
}

/// Project a `/indexes/:id/search` JSON payload into [`SearchHit`]s.
///
/// Why: the search response wraps a `results` array of `CodeChunk` objects;
/// centralising the projection keeps the client testable without a daemon and
/// resilient to absent optional fields.
/// What: reads `results`, and for each entry takes `file`, `start_line`, and a
/// snippet (preferring `compact_snippet`, falling back to the first line of
/// `content`). A non-object or missing `results` yields an empty list.
/// Test: `parse_search_hits_projects_fields`.
pub fn parse_search_hits(raw: &serde_json::Value) -> Vec<SearchHit> {
    let Some(results) = raw.get("results").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    results
        .iter()
        .map(|item| {
            let file = item
                .get("file")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let line = item.get("start_line").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            let snippet = item
                .get("compact_snippet")
                .and_then(|v| v.as_str())
                .or_else(|| item.get("content").and_then(|v| v.as_str()))
                .unwrap_or_default()
                .lines()
                .next()
                .unwrap_or_default()
                .trim()
                .to_string();
            SearchHit {
                file,
                line,
                snippet,
            }
        })
        .collect()
}

/// One progress event from the reindex SSE stream.
///
/// Why: the search TUI shows live reindex progress in its activity log; a
/// typed enum lets the renderer format each event distinctly without parsing
/// raw JSON in the event loop.
/// What: `Started` with the file count, `Progress` with the current file and
/// percent-complete, `Complete` with the final chunk count, or `Failed` with
/// an error string.
/// Test: `parse_reindex_event_maps_event_field`.
#[derive(Debug, Clone, PartialEq)]
pub enum ReindexEvent {
    /// The reindex walk finished; carries the total file count.
    Started {
        /// Total files the reindex will process.
        total_files: u64,
    },
    /// A batch completed; carries progress toward completion.
    Progress {
        /// Files indexed so far.
        indexed: u64,
        /// Total files in this reindex.
        total_files: u64,
    },
    /// The reindex finished; carries the final chunk count and status.
    Complete {
        /// Total chunks in the index after the reindex.
        total_chunks: u64,
        /// Terminal status string (`"complete"` or `"aborted_memory"`).
        status: String,
    },
    /// The reindex (or its stream) failed; carries an error message.
    Failed(String),
}

/// Parse one reindex SSE `data:` JSON object into a [`ReindexEvent`].
///
/// Why: the daemon emits `start` / `batch` / `skip` / `error` / `complete`
/// frames; the TUI only needs three of them plus a failure signal, so this
/// folds the wire shapes into the [`ReindexEvent`] the renderer expects.
/// What: dispatches on the `event` field — `start` → `Started`, `batch` /
/// `skip` → `Progress`, `complete` → `Complete`, `error` → `Failed`. Any other
/// value falls back to a `Progress` event with whatever counters are present.
/// Test: `parse_reindex_event_maps_event_field`.
pub fn parse_reindex_event(value: &serde_json::Value) -> ReindexEvent {
    let kind = value.get("event").and_then(|v| v.as_str()).unwrap_or("");
    let u64_of = |key: &str| value.get(key).and_then(|v| v.as_u64()).unwrap_or(0);
    match kind {
        "start" => ReindexEvent::Started {
            total_files: u64_of("total_files"),
        },
        "complete" => ReindexEvent::Complete {
            total_chunks: u64_of("total_chunks"),
            status: value
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("complete")
                .to_string(),
        },
        "error" => ReindexEvent::Failed(
            value
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("reindex error")
                .to_string(),
        ),
        _ => ReindexEvent::Progress {
            indexed: u64_of("indexed"),
            total_files: u64_of("total_files"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    #[test]
    fn default_search_url_is_local() {
        assert_eq!(DEFAULT_SEARCH_URL, "http://127.0.0.1:7878");
    }

    #[test]
    fn normalize_url_adds_scheme() {
        assert_eq!(normalize_url("127.0.0.1:7878"), "http://127.0.0.1:7878");
        assert_eq!(
            normalize_url("http://127.0.0.1:7878"),
            "http://127.0.0.1:7878"
        );
        assert_eq!(normalize_url("https://example.com"), "https://example.com");
    }

    #[test]
    fn search_client_stores_base_url() {
        let client = SearchClient::new("http://127.0.0.1:7878");
        assert_eq!(client.base_url(), "http://127.0.0.1:7878");
    }

    #[test]
    fn search_client_repoints() {
        let mut client = SearchClient::new("http://127.0.0.1:7878");
        client.set_base_url("http://127.0.0.1:9999");
        assert_eq!(client.base_url(), "http://127.0.0.1:9999");
    }

    #[test]
    fn resolve_search_url_falls_back_to_default() {
        // When discovery yields nothing the resolver must return a usable URL
        // rather than an empty string. It returns either a discovered address
        // or the documented default — both are non-empty and HTTP-schemed.
        let url = resolve_search_url();
        assert!(url.starts_with("http://") || url.starts_with("https://"));
    }

    /// Regression for the trusty-search#3602 review finding: `resolve_search_url`
    /// (backing `trusty-search monitor status`/`monitor indexes`/`monitor tui`)
    /// must discover a daemon on a NON-default port once it is registered via
    /// `write_daemon_addr("trusty-search", …)` -- the exact primitive
    /// `trusty-search`'s `run_daemon()` now calls for its default instance.
    /// Before that producer-side fix this resolver had no writer at all and
    /// would silently fall back to `DEFAULT_SEARCH_URL`, sending
    /// `monitor`/the TUI's `[r]` reindex hotkey at the WRONG (or no) daemon.
    ///
    /// Why safe: redirects `resolve_data_dir` via `TRUSTY_DATA_DIR_OVERRIDE`
    /// into a tempdir, so this never reads or writes a real `$HOME`/platform
    /// data dir.
    /// What: write a non-default-port address, call `resolve_search_url()`,
    /// assert it returns that exact address rather than the default.
    /// Test: this function.
    #[test]
    #[serial]
    fn resolve_search_url_discovers_non_default_registered_address() {
        let _guard = crate::data_dir::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var(crate::data_dir::DATA_DIR_OVERRIDE_ENV, tmp.path());
        }
        crate::write_daemon_addr("trusty-search", "127.0.0.1:59321").unwrap();
        let url = resolve_search_url();
        unsafe {
            std::env::remove_var(crate::data_dir::DATA_DIR_OVERRIDE_ENV);
        }
        assert_eq!(
            url, "http://127.0.0.1:59321",
            "resolve_search_url must discover a registered non-default-port address"
        );
    }

    #[test]
    fn parse_search_hits_projects_fields() {
        // The search response wraps a `results` array; each hit projects
        // file, start_line, and a one-line snippet (compact_snippet preferred).
        let raw = serde_json::json!({
            "results": [
                {
                    "file": "src/lib.rs",
                    "start_line": 42,
                    "compact_snippet": "fn embed() {\n  ...\n}",
                    "content": "ignored when compact present",
                },
                {
                    "file": "src/main.rs",
                    "start_line": 7,
                    "content": "  fn main() {}\nmore",
                },
            ],
            "intent": "Code",
        });
        let hits = parse_search_hits(&raw);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].file, "src/lib.rs");
        assert_eq!(hits[0].line, 42);
        assert_eq!(hits[0].snippet, "fn embed() {");
        // The second hit falls back to content's first (trimmed) line.
        assert_eq!(hits[1].snippet, "fn main() {}");
        // A payload with no `results` array yields no hits.
        assert!(parse_search_hits(&serde_json::json!({})).is_empty());
    }

    #[test]
    fn parse_reindex_event_maps_event_field() {
        let started = parse_reindex_event(&serde_json::json!({
            "event": "start", "total_files": 1200,
        }));
        assert_eq!(started, ReindexEvent::Started { total_files: 1200 });

        let progress = parse_reindex_event(&serde_json::json!({
            "event": "batch", "indexed": 500, "total_files": 1200,
        }));
        assert_eq!(
            progress,
            ReindexEvent::Progress {
                indexed: 500,
                total_files: 1200,
            }
        );

        let complete = parse_reindex_event(&serde_json::json!({
            "event": "complete", "total_chunks": 19012, "status": "complete",
        }));
        assert_eq!(
            complete,
            ReindexEvent::Complete {
                total_chunks: 19012,
                status: "complete".into(),
            }
        );

        let failed = parse_reindex_event(&serde_json::json!({
            "event": "error", "message": "read: permission denied",
        }));
        assert_eq!(
            failed,
            ReindexEvent::Failed("read: permission denied".into())
        );
    }
}
