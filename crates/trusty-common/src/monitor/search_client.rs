//! Socket client for the trusty-search daemon's monitor surfaces.
//!
//! Why: the unified monitor dashboard and `trusty-search monitor tui` need a
//! typed, testable transport to the daemon's read surfaces plus the reindex
//! action. ADR-0032 makes trusty-search UDS-only, so this client speaks the
//! daemon's Unix socket and nothing else (#9214).
//! What: [`SearchClient`] wraps the socket path [`resolve_search_socket`]
//! returns and calls one [`crate::search_rpc`] method per surface. A missing
//! socket or a silent daemon is an error naming the socket path; no call falls
//! back to a default address or to empty data.
//! Test: `search_client_tests.rs`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Context;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::monitor::dashboard::{IndexRow, SearchData};
use crate::search_rpc::{
    METHOD_HEALTH, METHOD_INDEX_REINDEX, METHOD_INDEX_STATUS, METHOD_INDEXES_LIST, call_at,
};

#[cfg(test)]
#[path = "search_client_tests.rs"]
mod tests;

/// Per-call timeout for trusty-search probes.
///
/// Why: a hung daemon must not freeze the dashboard's refresh tick; a short
/// timeout turns an unresponsive daemon into a clean "offline" state.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(3);

/// Per-frame read budget on the reindex progress stream.
///
/// Why: a large reindex stalls between batches while the embedder works, so
/// the 3 s probe budget would cut a healthy stream. Five minutes still bounds a
/// daemon that stopped sending (#9214: every socket call is bounded).
const STREAM_FRAME_TIMEOUT: Duration = Duration::from_secs(300);

/// `search.graph.stats` — one index's node, edge and edge-kind counts.
const METHOD_GRAPH_STATS: &str = "search.graph.stats";
/// `search.logs.tail` — the daemon's most recent log lines.
const METHOD_LOGS_TAIL: &str = "search.logs.tail";
/// `search.query` — a hybrid search against one index.
const METHOD_QUERY: &str = "search.query";
/// `search.index.reindex.stream` — one index's reindex progress events.
const METHOD_INDEX_REINDEX_STREAM: &str = "search.index.reindex.stream";

/// Resolve the trusty-search daemon's socket path.
///
/// Why: #9214 retires the `http://127.0.0.1:7878` default and the
/// `read_daemon_addr("trusty-search")` lookup; the socket path is derived on
/// both ends, so there is nothing to discover and nothing to fall back to.
/// What: [`crate::search_rpc::search_socket`], which honours
/// `TRUSTY_SEARCH_SOCKET`.
///
/// # Errors
///
/// When the data directory cannot be resolved.
///
/// Test: `resolve_search_socket_honours_the_env_override`.
pub fn resolve_search_socket() -> anyhow::Result<PathBuf> {
    crate::search_rpc::search_socket()
}

/// `search.health`'s fields the dashboard renders.
#[derive(Debug, Deserialize)]
struct HealthWire {
    version: String,
    #[serde(default)]
    uptime_secs: u64,
}

/// `search.logs.tail`'s `{lines, total}`; only the lines are surfaced.
#[derive(Debug, Deserialize)]
struct LogsTailWire {
    #[serde(default)]
    lines: Vec<String>,
}

/// `search.indexes.list`'s `{indexes: [id, ...]}`.
#[derive(Debug, Deserialize)]
struct IndexListWire {
    #[serde(default)]
    indexes: Vec<String>,
}

/// `search.graph.stats`'s node and edge counts plus the per-kind breakdown.
#[derive(Debug, Deserialize)]
struct GraphStatsWire {
    #[serde(default)]
    node_count: u64,
    #[serde(default)]
    edge_count: u64,
    #[serde(default)]
    edge_kinds: std::collections::HashMap<String, u64>,
}

/// `search.index.status`'s root, chunk count, size and last-indexed time.
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

/// Typed socket client for the trusty-search daemon.
///
/// Why: the dashboard polls trusty-search every refresh tick; one client per
/// dashboard keeps the call sites tidy and the socket path in one place.
/// What: holds the daemon's socket path; each method is one bounded
/// [`crate::search_rpc::call_at`] (or one stream, for reindex progress).
/// Test: `fetch_all_reads_every_surface_over_the_socket`,
/// `fetch_all_names_the_socket_when_no_daemon_answers`.
#[derive(Debug, Clone)]
pub struct SearchClient {
    socket: PathBuf,
}

impl SearchClient {
    /// Build a client targeting the daemon socket at `socket`.
    pub fn new(socket: impl Into<PathBuf>) -> Self {
        Self {
            socket: socket.into(),
        }
    }

    /// A client for the socket [`resolve_search_socket`] names.
    ///
    /// # Errors
    ///
    /// When the data directory cannot be resolved.
    pub fn resolve() -> anyhow::Result<Self> {
        Ok(Self::new(resolve_search_socket()?))
    }

    /// The socket path this client dials.
    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// One bounded call, decoded into `T`.
    async fn call<T: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        params: Value,
    ) -> anyhow::Result<T> {
        let raw = call_at(&self.socket, method, params, REQUEST_TIMEOUT).await?;
        serde_json::from_value(raw).with_context(|| {
            format!(
                "decode {method} from the trusty-search daemon at {}",
                self.socket.display()
            )
        })
    }

    /// Fetch every panel field from the trusty-search daemon.
    ///
    /// Why: the dashboard wants one fallible call that yields a complete
    /// [`SearchData`] or an error it can render as the offline state.
    /// What: `search.health`, then `search.indexes.list`, then
    /// `search.index.status` and `search.graph.stats` per index. Health and the
    /// list must answer; a failed per-index status yields a zero-chunk row and
    /// a failed graph read leaves the graph counters at zero, as the HTTP read
    /// did. Rows are sorted by id.
    /// Test: `fetch_all_reads_every_surface_over_the_socket`,
    /// `fetch_all_names_the_socket_when_no_daemon_answers`.
    pub async fn fetch_all(&self) -> anyhow::Result<SearchData> {
        // #9214: socket only — a dead socket is an error naming its path.
        let health: HealthWire = self.call(METHOD_HEALTH, json!({})).await?;
        let list: IndexListWire = self.call(METHOD_INDEXES_LIST, json!({})).await?;

        let mut indexes = Vec::with_capacity(list.indexes.len());
        for id in list.indexes {
            let params = json!({ "index_id": id });
            let status = self
                .call::<IndexStatusWire>(METHOD_INDEX_STATUS, params.clone())
                .await;
            let mut row = match status {
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
            // #6382: no `communities` probe — the daemon never served one.
            match self
                .call::<GraphStatsWire>(METHOD_GRAPH_STATS, params)
                .await
            {
                Ok(stats) => {
                    row.node_count = stats.node_count;
                    row.edge_count = stats.edge_count;
                    let mut kinds: Vec<(String, u64)> = stats.edge_kinds.into_iter().collect();
                    kinds.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
                    row.edge_kinds = kinds;
                }
                Err(e) => tracing::debug!("graph stats probe failed for {id}: {e}"),
            }
            indexes.push(row);
        }
        indexes.sort_by(|a, b| a.id.cmp(&b.id));

        Ok(SearchData {
            version: health.version,
            uptime_secs: health.uptime_secs,
            indexes,
        })
    }

    /// Fetch the `n` most recent daemon log lines through `search.logs.tail`.
    ///
    /// Why: the search TUI polls this each tick so background daemon activity
    /// reaches the ACTIVITY panel without a key press.
    /// What: one `search.logs.tail` call with `{n}`.
    ///
    /// # Errors
    ///
    /// When the daemon does not answer (#9214: never an empty list in its
    /// place, so a caller cannot mistake "down" for "quiet").
    ///
    /// Test: `logs_tail_reads_the_socket_and_fails_when_it_is_absent`.
    pub async fn logs_tail(&self, n: usize) -> anyhow::Result<Vec<String>> {
        let wire: LogsTailWire = self.call(METHOD_LOGS_TAIL, json!({ "n": n })).await?;
        Ok(wire.lines)
    }

    /// Queue a reindex of `id` through `search.index.reindex`.
    ///
    /// # Errors
    ///
    /// When the daemon does not answer or refuses the reindex.
    ///
    /// Test: `reindex_and_search_call_their_socket_methods`.
    pub async fn reindex(&self, id: &str) -> anyhow::Result<()> {
        call_at(
            &self.socket,
            METHOD_INDEX_REINDEX,
            json!({ "index_id": id }),
            REQUEST_TIMEOUT,
        )
        .await?;
        Ok(())
    }

    /// Run a hybrid search against index `id` through `search.query`.
    ///
    /// What: sends `{index_id, body: {text, top_k}}` — the HTTP body, wrapped
    /// in the index envelope — and projects the hits with
    /// [`parse_search_hits`].
    ///
    /// # Errors
    ///
    /// When the daemon does not answer or refuses the query.
    ///
    /// Test: `reindex_and_search_call_their_socket_methods`.
    pub async fn search(
        &self,
        id: &str,
        query: &str,
        top_k: usize,
    ) -> anyhow::Result<Vec<SearchHit>> {
        let params = json!({ "index_id": id, "body": { "text": query, "top_k": top_k } });
        let raw = call_at(&self.socket, METHOD_QUERY, params, REQUEST_TIMEOUT).await?;
        Ok(parse_search_hits(&raw))
    }

    /// Kick off a reindex and stream progress events into `tx`.
    ///
    /// Why: the search TUI's `[r]` key fires this on a background task so the
    /// event loop can drain [`ReindexEvent`]s without blocking.
    /// What: [`Self::reindex`], then `search.index.reindex.stream`, parsing each
    /// item into a [`ReindexEvent`]. Any failure — including a missing socket —
    /// is sent as a final [`ReindexEvent::Failed`] naming the socket.
    /// Test: `reindex_stream_reports_a_missing_socket_as_failed`; event parsing
    /// is `parse_reindex_event_maps_event_field`.
    pub async fn reindex_stream(&self, id: &str, tx: tokio::sync::mpsc::Sender<ReindexEvent>) {
        if let Err(e) = self.reindex_stream_inner(id, &tx).await {
            let _ = tx.send(ReindexEvent::Failed(format!("{e:#}"))).await;
        }
    }

    /// Inner body of [`Self::reindex_stream`] returning a `Result` for `?`.
    async fn reindex_stream_inner(
        &self,
        id: &str,
        tx: &tokio::sync::mpsc::Sender<ReindexEvent>,
    ) -> anyhow::Result<()> {
        self.reindex(id).await?;
        let request = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": METHOD_INDEX_REINDEX_STREAM,
            "params": { "index_id": id },
            "stream": true,
        });
        let open = crate::uds::send_framed_stream_request::<_, Value>(
            &self.socket,
            &request,
            STREAM_FRAME_TIMEOUT,
        )
        .await;
        let mut stream = open.with_context(|| {
            format!(
                "open {METHOD_INDEX_REINDEX_STREAM} on the trusty-search daemon at {}",
                self.socket.display()
            )
        })?;
        while let Some(item) = stream.next_frame().await {
            let event = parse_reindex_event(&item?);
            let terminal = matches!(event, ReindexEvent::Complete { .. });
            if tx.send(event).await.is_err() || terminal {
                return Ok(()); // receiver gone, or the run finished.
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
