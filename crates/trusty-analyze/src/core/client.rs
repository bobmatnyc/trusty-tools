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

/// How a chunk walk tolerates a slow page (#10781).
///
/// Why: one timed-out page used to abort the whole walk and discard every
/// chunk already fetched; under trusty-search load a 1000-row page can time
/// out where a 100-row page answers in half a second.
/// What: a page that fails in transport (timeout, broken connection) is
/// retried up to `attempts` times in total, halving the page `limit` on each
/// retry down to `min_limit`, with a linear `backoff`. A page the daemon
/// answered with an error is never retried. The reduced limit stays in force
/// for the rest of the walk.
/// Test: `a_page_that_times_out_once_still_completes_the_walk`,
/// `a_page_that_always_times_out_fails_naming_its_cursor`.
#[derive(Clone, Debug)]
pub struct PageRetryPolicy {
    /// Bound on one `search.chunks.list` call.
    pub call_timeout: Duration,
    /// Total tries per page, the first included. Values below 1 act as 1.
    pub attempts: u32,
    /// The page limit never drops below this.
    pub min_limit: usize,
    /// Sleep before retry `n` is `backoff * n`.
    pub backoff: Duration,
}

impl Default for PageRetryPolicy {
    fn default() -> Self {
        Self {
            call_timeout: CALL_TIMEOUT,
            attempts: 4,
            min_limit: 100,
            backoff: Duration::from_millis(250),
        }
    }
}

/// What `search.index.status` says about an index's current corpus (#10781).
///
/// Why: the per-index analysis cache must notice that an index changed.
/// trusty-search exposes no monotonic generation, so this is the strongest
/// signal it serves: the last completed reindex, the chunk count, and the
/// lifecycle status.
/// What: two fingerprints are equal only when all three fields are equal. A
/// fingerprint carrying neither `last_indexed` nor `chunk_count` cannot
/// detect any change and is not [`Self::is_verifiable`].
/// Test: `fingerprint_reads_last_indexed_chunk_count_and_status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexFingerprint {
    #[serde(default)]
    pub last_indexed: Option<String>,
    #[serde(default)]
    pub chunk_count: Option<u64>,
    #[serde(default)]
    pub status: Option<String>,
}

impl IndexFingerprint {
    /// True when the fingerprint carries at least one change signal.
    pub fn is_verifiable(&self) -> bool {
        self.last_indexed.is_some() || self.chunk_count.is_some()
    }
}

/// Client for the trusty-search daemon's Unix socket.
///
/// Cheap to clone — it holds only the socket path and the page retry policy;
/// each call dials afresh.
#[derive(Clone, Debug)]
pub struct TrustySearchClient {
    socket: PathBuf,
    // #10781: page-walk timeout and retry bounds.
    page_retry: PageRetryPolicy,
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
            page_retry: PageRetryPolicy::default(),
        }
    }

    /// Replace the page retry policy (#10781).
    ///
    /// Test: `a_page_that_times_out_once_still_completes_the_walk`.
    pub fn with_page_retry(mut self, policy: PageRetryPolicy) -> Self {
        self.page_retry = policy;
        self
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

    /// `search.index.status` — the index's change signals (#10781).
    ///
    /// What: reads `last_indexed`, `chunk_count` and `status` from the status
    /// body; an absent field is `None`. A failed call is `Err`.
    /// Test: `fingerprint_reads_last_indexed_chunk_count_and_status`.
    pub async fn index_fingerprint(&self, index_id: &str) -> Result<IndexFingerprint> {
        self.call(
            search_rpc::METHOD_INDEX_STATUS,
            json!({ "index_id": index_id }),
        )
        .await
    }

    /// `search.chunks.list` scoped to one `path_prefix` (#10781).
    ///
    /// Why: a PR review needs the changed files, not the corpus. trusty-search
    /// filters by a segment-boundary prefix (#7677), so a scoped walk is a few
    /// small pages instead of hundreds of large ones.
    /// What: the same cursor walk, completeness check and page retry as
    /// [`Self::get_chunks`], with `path_prefix` on every page. A page that does
    /// not echo the prefix came from a daemon that ignored the filter; the
    /// walk is refused rather than allowed to page the whole corpus.
    /// Test: `scoped_walk_sends_the_prefix_on_every_page`,
    /// `a_daemon_that_ignores_path_prefix_is_refused`.
    pub async fn get_chunks_under(&self, index_id: &str, prefix: &str) -> Result<Vec<CodeChunk>> {
        self.walk_chunks(index_id, Some(prefix)).await
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
    /// `export_without_total_makes_no_completeness_claim`,
    /// `a_page_that_times_out_once_still_completes_the_walk` (#10781 retry).
    pub async fn get_chunks(&self, index_id: &str) -> Result<Vec<CodeChunk>> {
        self.walk_chunks(index_id, None).await
    }

    /// The cursor walk behind [`Self::get_chunks`] and
    /// [`Self::get_chunks_under`]; `prefix` scopes every page.
    async fn walk_chunks(&self, index_id: &str, prefix: Option<&str>) -> Result<Vec<CodeChunk>> {
        let mut all_chunks: Vec<CodeChunk> = Vec::new();
        // An empty cursor means "start from the first chunk" — trusty-search
        // treats `after: ""` as present-but-unset, which is how cursor mode is
        // selected at all. Omitting the field would fall back to offset.
        let mut cursor = String::new();
        let mut reported_total: Option<usize> = None;
        let mut limit = CHUNK_PAGE_LIMIT;

        loop {
            let page = self
                .fetch_page_with_retry(index_id, &cursor, prefix, &mut limit)
                .await?;
            // #10781: an unscoped page from a pre-#7677 daemon would make this
            // "scoped" walk page the whole corpus. Refuse it.
            if let Some(p) = prefix {
                if page.path_prefix.as_deref() != Some(p) {
                    bail!(
                        "index '{index_id}': trusty-search did not apply path_prefix {p:?} \
                         (it echoed {:?}); it predates #7677, so a scoped analysis would \
                         page the whole corpus and is refused",
                        page.path_prefix
                    );
                }
            }
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
                let scope = prefix.map_or(String::new(), |p| format!(" under {p:?}"));
                bail!(
                    "index '{index_id}'{scope}: chunk export is incomplete — trusty-search reports \
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

    /// Fetch one page at `cursor`, retrying a transport failure (#10781).
    ///
    /// What: applies [`PageRetryPolicy`]; `limit` is halved in place on each
    /// retry so later pages keep the smaller size. The final error names the
    /// cursor, the attempt count and the last limit tried.
    async fn fetch_page_with_retry(
        &self,
        index_id: &str,
        cursor: &str,
        prefix: Option<&str>,
        limit: &mut usize,
    ) -> Result<ChunksPage> {
        let policy = &self.page_retry;
        let attempts = policy.attempts.max(1);
        let mut attempt = 1;
        loop {
            let err = match self
                .fetch_chunk_page(index_id, cursor, prefix, *limit)
                .await
            {
                Ok(page) => return Ok(page),
                Err(e) => e,
            };
            // The daemon answered: a retry would get the same answer.
            if err.downcast_ref::<SearchRpcError>().is_some() || attempt >= attempts {
                return Err(err.context(format!(
                    "index '{index_id}': chunk page after cursor {cursor:?} failed on attempt \
                     {attempt} of {attempts} (page limit {limit})"
                )));
            }
            tracing::warn!(
                index_id,
                cursor,
                attempt,
                limit = *limit,
                "chunk page failed in transport; retrying at a smaller limit: {err:#}"
            );
            *limit = (*limit / 2).max(policy.min_limit.max(1));
            tokio::time::sleep(policy.backoff * attempt).await;
            attempt += 1;
        }
    }

    /// Fetch a single chunk page at `cursor`.
    async fn fetch_chunk_page(
        &self,
        index_id: &str,
        cursor: &str,
        prefix: Option<&str>,
        limit: usize,
    ) -> Result<ChunksPage> {
        // #9214: socket twin of `GET /indexes/{id}/chunks?after=&limit=`.
        let mut params = json!({ "index_id": index_id, "after": cursor, "limit": limit });
        if let Some(p) = prefix {
            params["path_prefix"] = json!(p);
        }
        let value = search_rpc::call_at(
            &self.socket,
            METHOD_CHUNKS_LIST,
            params,
            self.page_retry.call_timeout,
        )
        .await
        .with_context(|| format!("{METHOD_CHUNKS_LIST} index '{index_id}' after {cursor:?}"))?;
        serde_json::from_value(value).with_context(|| {
            format!(
                "decode {METHOD_CHUNKS_LIST} from the trusty-search daemon at {}",
                self.socket.display()
            )
        })
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
    /// #10781: the prefix trusty-search applied, echoed only when one was.
    #[serde(default)]
    path_prefix: Option<String>,
}
