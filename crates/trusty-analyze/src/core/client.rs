//! Thin async HTTP client to the trusty-search daemon.
//!
//! Why: the analyzer is a sidecar — it never reads trusty-search's redb files
//! directly. Instead it pulls chunks over HTTP and runs analysis in-process.
//! Keeping the client tiny (one struct, three GETs) makes failure modes
//! obvious and lets us swap to a different transport later if needed.

use crate::types::CodeChunk;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Page size used when paging `GET /indexes/:id/chunks`. The server caps each
/// page at 1000 chunks.
const CHUNK_PAGE_LIMIT: usize = 1000;

/// Connection-pool depth for the loopback HTTP/2 client.
const CONNECTION_POOL_DEPTH: usize = 4;

/// Summary of one registered index.
///
/// Why: callers (the analyzer service, tests) need both the index id and the
/// on-disk root path so project-scoped tools (Roslyn, etc.) can resolve chunk
/// paths to real files without out-of-band configuration.
/// What: populated either by `list_indexes` (id only, root_path = None) or by
/// `index_details` (id + root_path from `?details=true`).
/// Test: `index_summary_deserializes_with_and_without_root_path` confirms
/// serde round-trips for both shapes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexSummary {
    pub id: String,
    /// Absolute on-disk root path of the indexed source tree.
    /// Present only when fetched via `index_details` (`?details=true`).
    #[serde(default)]
    pub root_path: Option<String>,
}

/// HTTP/2 client for trusty-search (port 7878).
/// Uses `http2_prior_knowledge` (loopback, no TLS) for low-latency chunk paging.
///
/// Cheap to clone — internally a `reqwest::Client` (which is already an `Arc`
/// under the hood).
#[derive(Clone)]
pub struct TrustySearchClient {
    base_url: String,
    http: reqwest::Client,
}

impl TrustySearchClient {
    /// Construct a client pointed at `base_url` (e.g. `http://127.0.0.1:7878`).
    /// Trailing slashes are tolerated.
    pub fn new(base_url: impl Into<String>) -> Self {
        let mut base = base_url.into();
        if base.ends_with('/') {
            base.pop();
        }
        let http = reqwest::ClientBuilder::new()
            .tcp_keepalive(Duration::from_secs(60))
            .pool_max_idle_per_host(CONNECTION_POOL_DEPTH)
            .http2_prior_knowledge() // both processes are on 127.0.0.1, no TLS needed
            .timeout(Duration::from_secs(30))
            .connect_timeout(Duration::from_secs(5))
            .build()
            .expect("failed to build HTTP client");
        Self {
            base_url: base,
            http,
        }
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// `GET /health` — true if the daemon answers 2xx.
    pub async fn health(&self) -> Result<bool> {
        let url = format!("{}/health", self.base_url);
        let resp = self.http.get(&url).send().await.context("GET /health")?;
        Ok(resp.status().is_success())
    }

    /// `GET /indexes` — list every registered index id.
    pub async fn list_indexes(&self) -> Result<Vec<IndexSummary>> {
        #[derive(Deserialize)]
        struct Listing {
            indexes: Vec<String>,
        }
        let url = format!("{}/indexes", self.base_url);
        let body: Listing = self
            .http
            .get(&url)
            .send()
            .await
            .with_context(|| format!("GET {url}"))?
            .error_for_status()
            .with_context(|| format!("non-2xx from {url}"))?
            .json()
            .await
            .with_context(|| format!("decode {url}"))?;
        Ok(body
            .indexes
            .into_iter()
            .map(|id| IndexSummary {
                id,
                root_path: None,
            })
            .collect())
    }

    /// `GET /indexes?details=true` — return every index with its root_path.
    ///
    /// Why: project-scoped tools (e.g. `RoslynTool`) need the real on-disk root
    /// path of each index so they can resolve chunk-relative file paths to
    /// absolute paths and invoke the compiler against the real project tree.
    /// `list_indexes()` only returns ids; this method fetches the richer form.
    /// What: GETs `{base}/indexes?details=true`, expects the response shape
    /// `{"indexes": [{id, root_path}, ...]}`, and deserializes into
    /// `Vec<IndexSummary>` with `root_path` populated where the daemon provides it.
    /// Test: `index_details_deserializes_root_path` tests the serde shape directly
    /// without a live server by parsing a hand-built JSON string.
    pub async fn index_details(&self) -> Result<Vec<IndexSummary>> {
        #[derive(Deserialize)]
        struct Listing {
            indexes: Vec<IndexSummary>,
        }
        let url = format!("{}/indexes?details=true", self.base_url);
        let body: Listing = self
            .http
            .get(&url)
            .send()
            .await
            .with_context(|| format!("GET {url}"))?
            .error_for_status()
            .with_context(|| format!("non-2xx from {url}"))?
            .json()
            .await
            .with_context(|| format!("decode {url}"))?;
        Ok(body.indexes)
    }

    /// `GET /indexes/:id/status` — fetch the status of a single index, including `root_path`.
    ///
    /// Why: the diagnostics path previously called `index_details()` which fetches
    /// ALL indexes and then linearly scans for the matching id just to obtain
    /// `root_path`. This is O(n) per request (issue #1013). This method calls the
    /// per-index status endpoint directly, making the lookup O(1) regardless of
    /// how many indexes are registered.
    /// What: GETs `{base}/indexes/{id}/status`, extracts the `root_path` string
    /// field, and returns `Ok(Some(path))` when present, `Ok(None)` when the field
    /// is absent or null, and `Err` when the HTTP call fails or the index is not
    /// found (404).
    /// Test: `index_status_deserializes_root_path` tests the serde extraction
    /// without a live server by parsing a hand-built JSON string.
    pub async fn index_status_root_path(&self, index_id: &str) -> Result<Option<String>> {
        #[derive(Deserialize)]
        struct StatusBody {
            #[serde(default)]
            root_path: Option<String>,
        }
        let url = format!("{}/indexes/{}/status", self.base_url, index_id);
        let body: StatusBody = self
            .http
            .get(&url)
            .send()
            .await
            .with_context(|| format!("GET {url}"))?
            .error_for_status()
            .with_context(|| format!("non-2xx from {url}"))?
            .json()
            .await
            .with_context(|| format!("decode {url}"))?;
        Ok(body.root_path)
    }

    /// `GET /indexes/:id/chunks` — bulk export of every chunk for `index_id`.
    /// Trusty-search must expose this endpoint (added as part of issue #40).
    ///
    /// Why (#6043): this used to page with `?offset=&limit=`, and that mode
    /// reads trusty-search's *in-memory* chunk map — a cache the search daemon
    /// evicts after 300s idle and rehydrates on a detached background task that
    /// the request does not wait for. `total` in that mode is the map's length,
    /// so an index whose corpus is cold, still rehydrating, or unreadable
    /// answers HTTP 200 with `total: 0` and an empty page. The analyzer read
    /// that as a complete, empty corpus and every downstream endpoint then
    /// asserted a confident zero: `complexity_distribution` reported
    /// `total: 0, skipped_non_code: 0` for an index holding 50,929 chunks. The
    /// offset map is also capped by `TRUSTY_MAX_CHUNKS`, so even a warm index
    /// exports fewer chunks than it holds.
    ///
    /// What: walks the cursor mode (`?after=&limit=`) instead, which seeks the
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
        let base = format!("{}/indexes/{}/chunks", self.base_url, index_id);

        let mut all_chunks: Vec<CodeChunk> = Vec::new();
        // An empty cursor means "start from the first chunk" — trusty-search
        // treats `after=` as present-but-unset, which is how cursor mode is
        // selected at all. Omitting the parameter would fall back to offset.
        let mut cursor = String::new();
        let mut reported_total: Option<usize> = None;

        loop {
            let page = fetch_chunk_page(&self.http, &base, &cursor, CHUNK_PAGE_LIMIT).await?;
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
}

/// One page of `GET /indexes/:id/chunks` in cursor mode.
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

/// Fetch a single chunk page at `cursor`. Free function so the future borrows
/// only the client, not the whole `TrustySearchClient`.
async fn fetch_chunk_page(
    http: &reqwest::Client,
    base_url: &str,
    cursor: &str,
    limit: usize,
) -> Result<ChunksPage> {
    let limit = limit.to_string();
    let req = http
        .get(base_url)
        .query(&[("after", cursor), ("limit", limit.as_str())]);
    let page: ChunksPage = req
        .send()
        .await
        .with_context(|| format!("GET {base_url}?after={cursor}"))?
        .error_for_status()
        .with_context(|| format!("non-2xx from {base_url}?after={cursor}"))?
        .json()
        .await
        .with_context(|| format!("decode {base_url}?after={cursor}"))?;
    Ok(page)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn index_summary_deserializes_with_and_without_root_path() {
        // With root_path present.
        let json = r#"{"id":"abc","root_path":"/home/user/proj"}"#;
        let s: IndexSummary = serde_json::from_str(json).expect("deserialize with root_path");
        assert_eq!(s.id, "abc");
        assert_eq!(s.root_path.as_deref(), Some("/home/user/proj"));

        // Without root_path (serde default = None).
        let json2 = r#"{"id":"xyz"}"#;
        let s2: IndexSummary = serde_json::from_str(json2).expect("deserialize without root_path");
        assert_eq!(s2.id, "xyz");
        assert!(s2.root_path.is_none());
    }

    #[test]
    fn index_details_deserializes_root_path() {
        // Simulate the JSON body returned by GET /indexes?details=true.
        // Tests that the response shape {indexes:[{id, root_path}]} is parsed correctly.
        let json = r#"{"indexes":[{"id":"idx1","root_path":"/src/myapp"},{"id":"idx2"}]}"#;
        #[derive(serde::Deserialize)]
        struct Listing {
            indexes: Vec<IndexSummary>,
        }
        let listing: Listing = serde_json::from_str(json).expect("parse listing");
        assert_eq!(listing.indexes.len(), 2);
        assert_eq!(listing.indexes[0].id, "idx1");
        assert_eq!(listing.indexes[0].root_path.as_deref(), Some("/src/myapp"));
        assert_eq!(listing.indexes[1].id, "idx2");
        assert!(listing.indexes[1].root_path.is_none());
    }

    /// Spawn a cursor-aware stub trusty-search and return its base URL.
    ///
    /// Why: a stub that answers every request with the same body cannot
    /// exercise a cursor walk at all — the loop's continuation, its
    /// no-progress guard, and the post-walk shortfall check all stay dark
    /// while the tests still pass. Keying the response on `after=` is what
    /// makes a multi-page walk observable.
    /// What: `pages` maps the `after` cursor value a request carries (`""` for
    /// the first page) to the body to answer it with. An unmapped cursor is a
    /// test bug, so it answers 500 rather than an empty page that would look
    /// like a clean end of corpus.
    /// Test: used by every `get_chunks` test below.
    async fn spawn_chunks_stub(pages: Vec<(&'static str, serde_json::Value)>) -> String {
        let pages: std::collections::HashMap<String, serde_json::Value> =
            pages.into_iter().map(|(k, v)| (k.to_string(), v)).collect();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = axum::Router::new().route(
            "/indexes/{id}/chunks",
            axum::routing::get(
                move |axum::extract::Query(q): axum::extract::Query<HashMap<String, String>>| {
                    let pages = pages.clone();
                    async move {
                        let cursor = q.get("after").cloned().unwrap_or_default();
                        match pages.get(&cursor) {
                            Some(body) => Ok(axum::Json(body.clone())),
                            None => Err((
                                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                                format!("stub has no page for cursor {cursor:?}"),
                            )),
                        }
                    }
                },
            ),
        );
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        format!("http://{addr}")
    }

    /// One page body: `chunks` for the given ids, plus `total` and the cursor
    /// to follow (`None` ends the walk).
    fn page(total: usize, ids: &[&str], next_cursor: Option<&str>) -> serde_json::Value {
        serde_json::json!({
            "total": total,
            "chunks": ids.iter().map(|i| chunk_json(i)).collect::<Vec<_>>(),
            "next_cursor": next_cursor,
        })
    }

    fn chunk_json(id: &str) -> serde_json::Value {
        serde_json::json!({
            "id": id, "file": "src/lib.rs", "start_line": 1, "end_line": 2,
            "content": "fn f() {}", "score": 0.0, "match_reason": "enumerate"
        })
    }

    /// #6043: the exact response the live daemon served for the `trusty-tools`
    /// index — `total: 50929` beside zero rows and `next_cursor: null`.
    ///
    /// Why: the walk reads `next_cursor: null` as "corpus exhausted", so before
    /// this fix `get_chunks` returned `Ok(vec![])` and
    /// `complexity_distribution` scored an empty corpus and published
    /// `total: 0, skipped_non_code: 0` — a confident, wrong measurement of a
    /// 50,929-chunk index.
    /// What: asserts the shortfall against the server's own `total` is an error
    /// naming both numbers.
    /// Test: this function IS the test.
    #[tokio::test]
    async fn short_export_against_reported_total_is_an_error() {
        let base = spawn_chunks_stub(vec![(
            "",
            serde_json::json!({
                "index_id": "trusty-tools", "total": 50929,
                "chunks": [], "next_cursor": null
            }),
        )])
        .await;
        let err = TrustySearchClient::new(base)
            .get_chunks("trusty-tools")
            .await
            .expect_err("a zero-row export against total: 50929 must not pass as the corpus");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("50929") && msg.contains("incomplete"),
            "the error must name the shortfall, got: {msg}"
        );
    }

    /// A response with no `total` makes no completeness claim, so there is
    /// nothing to check it against — the export is returned as-is.
    #[tokio::test]
    async fn export_without_total_makes_no_completeness_claim() {
        let base = spawn_chunks_stub(vec![(
            "",
            serde_json::json!({ "chunks": [chunk_json("a:1:2")] }),
        )])
        .await;
        let chunks = TrustySearchClient::new(base)
            .get_chunks("idx")
            .await
            .expect("no total means no claim to verify");
        assert_eq!(chunks.len(), 1);
    }

    /// A complete single-page export satisfies the reported total.
    #[tokio::test]
    async fn cursor_walk_accepts_a_complete_export() {
        let base = spawn_chunks_stub(vec![("", page(2, &["a:1:2", "b:1:2"], None))]).await;
        let chunks = TrustySearchClient::new(base)
            .get_chunks("idx")
            .await
            .expect("a complete export is accepted");
        assert_eq!(chunks.len(), 2);
    }

    /// The walk follows `next_cursor` across every page and stops on the page
    /// that offers none.
    ///
    /// Why: a corpus larger than one page is the ordinary case — the live
    /// `trusty-tools` index is 51 pages at the 1000-row server cap — and none
    /// of the loop's continuation was exercised while the stub answered every
    /// request identically. A walk that silently stopped after page one would
    /// have looked correct in every other test here.
    /// What: three pages chained by cursor, asserting all rows arrive exactly
    /// once and in order.
    /// Test: this function IS the test.
    #[tokio::test]
    async fn cursor_walk_collects_every_page() {
        let base = spawn_chunks_stub(vec![
            ("", page(5, &["a:1:2", "b:1:2"], Some("b:1:2"))),
            ("b:1:2", page(5, &["c:1:2", "d:1:2"], Some("d:1:2"))),
            ("d:1:2", page(5, &["e:1:2"], None)),
        ])
        .await;
        let chunks = TrustySearchClient::new(base)
            .get_chunks("idx")
            .await
            .expect("a three-page walk completes");
        let ids: Vec<&str> = chunks.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["a:1:2", "b:1:2", "c:1:2", "d:1:2", "e:1:2"],
            "every page's rows arrive exactly once, in walk order"
        );
    }

    /// A multi-page walk that dies partway is a shortfall, not a short corpus.
    ///
    /// Why: `chunks_after` turns a mid-walk redb read failure into an empty
    /// page with `next_cursor: null`, which is bit-identical to a clean end of
    /// corpus. Page one succeeding is what makes this the insidious shape — the
    /// client has rows in hand and no reason to doubt them.
    /// What: page one returns 2 of 5 rows, page two returns the empty
    /// end-of-corpus shape. Asserts the walk refuses rather than returning 2.
    /// Test: this function IS the test.
    #[tokio::test]
    async fn walk_truncated_after_the_first_page_is_an_error() {
        let base = spawn_chunks_stub(vec![
            ("", page(5, &["a:1:2", "b:1:2"], Some("b:1:2"))),
            ("b:1:2", page(5, &[], None)),
        ])
        .await;
        let err = TrustySearchClient::new(base)
            .get_chunks("idx")
            .await
            .expect_err("2 of 5 rows must not pass as the whole corpus");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("5 chunks") && msg.contains("returned 2"),
            "the error must name both numbers, got: {msg}"
        );
    }

    /// A server that keeps handing back the cursor it was given must not spin.
    ///
    /// Why: the walk advances only because the cursor is exclusive and strictly
    /// increasing. A daemon that repeats it — a bug, or a corpus mutating under
    /// the walk — would otherwise loop forever inside a request handler.
    /// What: a page whose `next_cursor` equals the cursor that fetched it. The
    /// guard breaks the loop and the shortfall check reports it, so the test
    /// terminating at all is half the assertion.
    /// Test: this function IS the test.
    #[tokio::test]
    async fn repeated_cursor_stops_the_walk_instead_of_spinning() {
        let base = spawn_chunks_stub(vec![
            ("", page(9, &["a:1:2"], Some("a:1:2"))),
            ("a:1:2", page(9, &["a:1:2"], Some("a:1:2"))),
        ])
        .await;
        let err = TrustySearchClient::new(base)
            .get_chunks("idx")
            .await
            .expect_err("a non-advancing cursor is a failed walk, not a complete one");
        assert!(format!("{err:#}").contains("incomplete"), "got: {err:#}");
    }

    #[test]
    fn index_status_deserializes_root_path() {
        // Simulate the JSON body returned by GET /indexes/:id/status.
        // The status endpoint returns a richer object; only root_path is needed here.

        // With root_path present (the common production case).
        #[derive(serde::Deserialize)]
        struct StatusBody {
            #[serde(default)]
            root_path: Option<String>,
        }
        let json_with = r#"{"index_id":"myproj","root_path":"/home/user/myproj","chunk_count":42}"#;
        let s: StatusBody = serde_json::from_str(json_with).expect("parse status with root_path");
        assert_eq!(s.root_path.as_deref(), Some("/home/user/myproj"));

        // Without root_path (e.g. a future schema change or a test stub).
        let json_without = r#"{"index_id":"myproj","chunk_count":0}"#;
        let s2: StatusBody =
            serde_json::from_str(json_without).expect("parse status without root_path");
        assert!(s2.root_path.is_none());
    }
}
