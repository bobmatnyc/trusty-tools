//! Thin async client to the trusty-search daemon, over its Unix socket.
//!
//! Why: the analyzer is a sidecar — it never reads trusty-search's redb files
//! directly. Instead it pulls chunks from the daemon and runs analysis
//! in-process. Keeping the client tiny (one struct, five calls) makes failure
//! modes obvious.
//!
//! #9214: this client spoke HTTP to `127.0.0.1:7878`, a listener ADR-0032
//! retired, and so ignored `TRUSTY_SEARCH_SOCKET`. It now calls the daemon
//! through `trusty_common::search_rpc`, the one socket client every other
//! trusty-search consumer uses. There is no TCP fallback: a missing socket is
//! an error naming its path.

use crate::types::CodeChunk;
use anyhow::{bail, Context, Result};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::Duration;
use trusty_common::search_rpc::{self, SearchRpcError};

#[cfg(test)]
#[path = "client_tests.rs"]
mod tests;

/// Page size used when paging `search.chunks.list`. The server caps each page
/// at 1000 chunks.
const CHUNK_PAGE_LIMIT: usize = 1000;

/// Per-call bound — the same 30 s the HTTP client applied to a whole request.
const CALL_TIMEOUT: Duration = Duration::from_secs(30);

/// The socket twin of `GET /indexes/{id}/chunks`.
///
/// trusty-common names no constant for it, so the literal lives here; it is
/// pinned by `trusty_search::service::socket::METHODS`.
const METHOD_CHUNKS_LIST: &str = "search.chunks.list";

/// Summary of one registered index.
///
/// Why: callers (the analyzer service, tests) need both the index id and the
/// on-disk root path so project-scoped tools (Roslyn, etc.) can resolve chunk
/// paths to real files without out-of-band configuration.
/// What: populated either by `list_indexes` (id only, root_path = None) or by
/// `index_details` (id + root_path from `{details: true}`).
/// Test: `index_summary_deserializes_with_and_without_root_path` confirms
/// serde round-trips for both shapes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexSummary {
    pub id: String,
    /// Absolute on-disk root path of the indexed source tree.
    /// Present only when fetched via `index_details`.
    #[serde(default)]
    pub root_path: Option<String>,
}

/// Client for the trusty-search daemon's Unix socket.
///
/// Cheap to clone — it holds only the socket path; each call dials afresh.
#[derive(Clone, Debug)]
pub struct TrustySearchClient {
    socket: PathBuf,
}

impl TrustySearchClient {
    /// Construct a client that calls the daemon at `socket`.
    ///
    /// Why: tests and callers that already hold a path (a fake daemon, a
    /// sandbox) pass it straight in. #9214: this took an HTTP base URL.
    /// What: stores the path; nothing is dialled until the first call.
    /// Test: `missing_socket_is_an_error_naming_its_path`.
    pub fn new(socket: impl Into<PathBuf>) -> Self {
        Self {
            socket: socket.into(),
        }
    }

    /// Construct a client at the path every trusty-search client derives.
    ///
    /// Why: #9214 — the analyzer must honour `TRUSTY_SEARCH_SOCKET` like the
    /// daemon's other clients, and otherwise use the standard data-dir path.
    /// What: [`search_rpc::search_socket`], then [`Self::new`].
    /// Test: `health_reaches_trusty_search_at_the_socket_the_env_names`
    /// (tests/search_socket_9214.rs).
    ///
    /// # Errors
    ///
    /// When the data directory cannot be resolved or created.
    pub fn from_env() -> Result<Self> {
        let socket =
            search_rpc::search_socket().context("resolve the trusty-search socket path")?;
        Ok(Self::new(socket))
    }

    /// The socket this client calls, for operator-facing messages.
    pub fn socket_path(&self) -> &Path {
        &self.socket
    }

    /// Call `method` and decode its `result` as `T`.
    ///
    /// A dial failure carries the socket path (from `call_at`); a daemon error
    /// carries the daemon's code and message as a [`SearchRpcError`].
    async fn call<T: DeserializeOwned>(&self, method: &str, params: Value) -> Result<T> {
        let value = search_rpc::call_at(&self.socket, method, params, CALL_TIMEOUT).await?;
        serde_json::from_value(value).with_context(|| {
            format!(
                "decode {method} from the trusty-search daemon at {}",
                self.socket.display()
            )
        })
    }

    /// `search.health` — true if the daemon answers with a result.
    ///
    /// Same contract as the HTTP probe it replaced: a daemon that answers with
    /// an error is `Ok(false)` (the old non-2xx), and an unreachable socket is
    /// `Err` naming the path (the old transport failure).
    pub async fn health(&self) -> Result<bool> {
        // #9214: `search.health` over the socket replaces `GET /health`.
        match search_rpc::call_at(
            &self.socket,
            search_rpc::METHOD_HEALTH,
            json!({}),
            CALL_TIMEOUT,
        )
        .await
        {
            Ok(_) => Ok(true),
            Err(e) if e.downcast_ref::<SearchRpcError>().is_some() => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// `search.indexes.list` — list every registered index id.
    pub async fn list_indexes(&self) -> Result<Vec<IndexSummary>> {
        #[derive(Deserialize)]
        struct Listing {
            indexes: Vec<String>,
        }
        // #9214: socket twin of `GET /indexes`.
        let body: Listing = self
            .call(search_rpc::METHOD_INDEXES_LIST, json!({}))
            .await?;
        Ok(body
            .indexes
            .into_iter()
            .map(|id| IndexSummary {
                id,
                root_path: None,
            })
            .collect())
    }

    /// `search.indexes.list` with `{details: true}` — every index with its
    /// root_path.
    ///
    /// Why: project-scoped tools (e.g. `RoslynTool`) need the real on-disk root
    /// path of each index so they can resolve chunk-relative file paths to
    /// absolute paths and invoke the compiler against the real project tree.
    /// `list_indexes()` only returns ids; this method fetches the richer form.
    /// What: expects `{"indexes": [{id, root_path}, ...]}` and deserializes into
    /// `Vec<IndexSummary>` with `root_path` populated where the daemon provides it.
    /// Test: `index_details_deserializes_root_path`,
    /// `every_call_names_its_socket_method`.
    pub async fn index_details(&self) -> Result<Vec<IndexSummary>> {
        #[derive(Deserialize)]
        struct Listing {
            indexes: Vec<IndexSummary>,
        }
        // #9214: socket twin of `GET /indexes?details=true`.
        let body: Listing = self
            .call(search_rpc::METHOD_INDEXES_LIST, json!({ "details": true }))
            .await?;
        Ok(body.indexes)
    }

    /// `search.index.status` — fetch one index's `root_path`.
    ///
    /// Why: the diagnostics path previously called `index_details()` which fetches
    /// ALL indexes and then linearly scans for the matching id just to obtain
    /// `root_path`. This is O(n) per request (issue #1013). The per-index status
    /// call makes the lookup O(1) regardless of how many indexes are registered.
    /// What: extracts the `root_path` string field, and returns `Ok(Some(path))`
    /// when present, `Ok(None)` when the field is absent or null, and `Err` when
    /// the call fails or the index is not found (`-32004`).
    /// Test: `index_status_deserializes_root_path`,
    /// `every_call_names_its_socket_method`.
    pub async fn index_status_root_path(&self, index_id: &str) -> Result<Option<String>> {
        #[derive(Deserialize)]
        struct StatusBody {
            #[serde(default)]
            root_path: Option<String>,
        }
        // #9214: socket twin of `GET /indexes/{id}/status`.
        let body: StatusBody = self
            .call(
                search_rpc::METHOD_INDEX_STATUS,
                json!({ "index_id": index_id }),
            )
            .await?;
        Ok(body.root_path)
    }

    /// `search.chunks.list` — bulk export of every chunk for `index_id`.
    ///
    /// Why (#6043): this used to page with `offset`/`limit`, and that mode
    /// reads trusty-search's *in-memory* chunk map — a cache the search daemon
    /// evicts after 300s idle and rehydrates on a detached background task that
    /// the request does not wait for. `total` in that mode is the map's length,
    /// so an index whose corpus is cold, still rehydrating, or unreadable
    /// answers with `total: 0` and an empty page. The analyzer read that as a
    /// complete, empty corpus and every downstream endpoint then asserted a
    /// confident zero: `complexity_distribution` reported
    /// `total: 0, skipped_non_code: 0` for an index holding 50,929 chunks. The
    /// offset map is also capped by `TRUSTY_MAX_CHUNKS`, so even a warm index
    /// exports fewer chunks than it holds.
    ///
    /// What: walks the cursor mode (`after` + `limit`) instead, which seeks the
    /// durable redb corpus directly and reports `total` from it, then refuses to
    /// return a walk that fell short of that `total`. A response that carries no
    /// `total` makes no completeness claim and is returned as-is. Pages are
    /// sequential because each one needs the previous page's cursor; the seek is
    /// O(page), against offset mode's O(N log N) re-sort of the whole corpus.
    ///
    /// `total` is taken from the most recent page that carried one. During a
    /// concurrent reindex the count can move between pages, so a corpus that
    /// shrinks mid-walk can report a shortfall that is really a mutation. That
    /// is the intended bias: a report computed over a corpus that changed under
    /// the walk is wrong either way, and an error tells the caller to retry.
    ///
    /// Test: `cursor_walk_collects_every_page`,
    /// `cursor_walk_accepts_a_complete_export`,
    /// `short_export_against_reported_total_is_an_error`,
    /// `walk_truncated_after_the_first_page_is_an_error`,
    /// `repeated_cursor_stops_the_walk_instead_of_spinning`,
    /// `export_without_total_makes_no_completeness_claim`.
    pub async fn get_chunks(&self, index_id: &str) -> Result<Vec<CodeChunk>> {
        let mut all_chunks: Vec<CodeChunk> = Vec::new();
        // An empty cursor means "start from the first chunk" — trusty-search
        // treats `after: ""` as present-but-unset, which is how cursor mode is
        // selected at all. Omitting the field would fall back to offset.
        let mut cursor = String::new();
        let mut reported_total: Option<usize> = None;

        loop {
            let page = self.fetch_chunk_page(index_id, &cursor).await?;
            if let Some(total) = page.total {
                reported_total = Some(total);
            }
            let received = page.chunks.len();
            all_chunks.extend(page.chunks);

            let Some(next) = page.next_cursor.filter(|c| !c.is_empty()) else {
                break;
            };
            // A cursor that repeats, or one offered alongside an empty page,
            // cannot advance the walk. Stop rather than spin — the shortfall
            // check below is what turns that into a reported failure.
            if received == 0 || next == cursor {
                break;
            }
            cursor = next;
        }

        // #6043: a short export must not pass as the whole corpus.
        if let Some(total) = reported_total {
            if all_chunks.len() < total {
                bail!(
                    "index '{index_id}': chunk export is incomplete — trusty-search reports \
                     {total} chunks but the cursor walk returned {}. The corpus is cold, \
                     still rehydrating, or unreadable; analysis over this partial corpus \
                     would understate every metric, so it is refused rather than reported \
                     as a result (see #6043, #5917)",
                    all_chunks.len()
                );
            }
        }

        Ok(all_chunks)
    }

    /// Fetch a single chunk page at `cursor`.
    async fn fetch_chunk_page(&self, index_id: &str, cursor: &str) -> Result<ChunksPage> {
        // #9214: socket twin of `GET /indexes/{id}/chunks?after=&limit=`.
        let params = json!({ "index_id": index_id, "after": cursor, "limit": CHUNK_PAGE_LIMIT });
        self.call(METHOD_CHUNKS_LIST, params)
            .await
            .with_context(|| format!("{METHOD_CHUNKS_LIST} index '{index_id}' after {cursor:?}"))
    }
}

/// One page of `search.chunks.list` in cursor mode.
///
/// `total` and `next_cursor` are optional so a stub (or an older daemon) that
/// answers with `chunks` alone still deserializes — an absent `total` is read as
/// "this response makes no claim about the corpus size", never as zero.
#[derive(Deserialize)]
struct ChunksPage {
    chunks: Vec<CodeChunk>,
    #[serde(default)]
    total: Option<usize>,
    #[serde(default)]
    next_cursor: Option<String>,
}
