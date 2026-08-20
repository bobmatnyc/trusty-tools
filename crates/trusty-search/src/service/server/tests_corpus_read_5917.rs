//! Every corpus-backed endpoint must refuse an index whose corpus cannot be
//! read (#5917), not answer it.
//!
//! Why: the daemon returned `HTTP 200` with `results: []` for indexes holding
//! tens of thousands of chunks, and the only hint was `meta.bm25_lane_degraded`
//! — a flag that means "still warming up". The same fault reached the sibling
//! surfaces wearing different disguises: `grep` reported `{matches: [],
//! total: 0}` ("this literal is nowhere in your code"), `call_chain` reported
//! `404 entry point not found` for a symbol that exists, and the global
//! fan-out dropped the index with a log line and no field in the payload. A
//! single-shot caller had nothing that would reveal the answer was wrong.
//! What: builds a corpus-backed index over a real redb file, breaks its reads
//! the way redb's "Previous I/O error occurred" state does, and asserts each
//! handler's status and body. Asserts through the responses, not the Rust API,
//! so the shape a caller branches on is what is pinned.
//! Test: this module.

use super::*;
use axum::http::StatusCode;
use axum::Json;

/// Same definition redb keyed the corpus rows under — redb identifies a table
/// by name, so dropping this one breaks the real reads.
const CHUNKS_TABLE: redb::TableDefinition<'static, &str, &[u8]> =
    redb::TableDefinition::new("chunks");

/// A registered, corpus-backed index whose reads this module can break.
struct Fixture {
    _tmp: tempfile::TempDir,
    state: Arc<SearchAppState>,
    corpus: Arc<crate::core::corpus::CorpusStore>,
    id: crate::core::registry::IndexId,
}

impl Fixture {
    /// Index one file into a real redb corpus and register it as `id`.
    async fn build(id: &str) -> Self {
        use crate::core::corpus::CorpusStore;
        use crate::core::indexer::CodeIndexer;
        use crate::core::registry::{IndexHandle, IndexId, IndexRegistry, StageStatus};

        let tmp = tempfile::tempdir().unwrap();
        let corpus =
            Arc::new(CorpusStore::open(&tmp.path().join("index.redb")).expect("open corpus"));
        let mut indexer = CodeIndexer::new(id, tmp.path());
        indexer.set_corpus_store(Arc::clone(&corpus));
        indexer
            .index_files_batch(&[("src/a.rs".into(), "fn authenticate_user() {}\n".into())])
            .await
            .expect("index batch");
        // `grep` reads the file fresh from disk, so it has to exist there too.
        std::fs::create_dir_all(tmp.path().join("src")).expect("create src dir");
        std::fs::write(
            tmp.path().join("src/a.rs"),
            "fn authenticate_user() {}\n".as_bytes(),
        )
        .expect("write source file");

        let registry = IndexRegistry::new();
        let index_id = IndexId::new(id);
        let handle = IndexHandle::bare(
            index_id.clone(),
            Arc::new(tokio::sync::RwLock::new(indexer)),
            tmp.path().to_path_buf(),
        );
        let stages = Arc::clone(&handle.stages);
        registry.register(handle);
        {
            let mut s = stages.write().await;
            s.lexical.status = StageStatus::Ready;
        }
        Self {
            _tmp: tmp,
            state: Arc::new(SearchAppState::new(registry)),
            corpus,
            id: index_id,
        }
    }

    /// Drop the chunks table, then evict the in-memory caches that were
    /// standing in for it — the live sequence, where an idle eviction is
    /// followed by a rehydrate that cannot read the corpus back.
    async fn break_corpus_reads(&self) {
        let txn = self.corpus.db().begin_write().expect("begin write txn");
        assert!(
            txn.delete_table(CHUNKS_TABLE).expect("delete chunks table"),
            "the chunks table existed before the test broke it"
        );
        txn.commit().expect("commit table drop");
        // Keep every bounded rehydrate wait short: the scan fails immediately,
        // so this only bounds the wait if the notification is missed.
        // SAFETY: serialized against every other reader/writer of this var by
        // `#[serial_test::serial]` on each test in this module.
        unsafe { std::env::set_var("TRUSTY_REHYDRATE_WAIT_MS", "25") };
        let handle = self.state.registry.get(&self.id).expect("registered");
        let evicted = handle.indexer.read().await.reclaim_memory_now().await;
        assert!(evicted > 0, "eviction dropped the in-memory caches");
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // SAFETY: same serialization as the `set_var` in `break_corpus_reads`.
        unsafe { std::env::remove_var("TRUSTY_REHYDRATE_WAIT_MS") };
    }
}

/// Assert the shared `503 index_corpus_unavailable` contract on one body.
fn assert_corpus_unavailable(
    status: StatusCode,
    body: &serde_json::Value,
    index_id: &str,
    surface: &str,
) {
    assert_eq!(
        status,
        StatusCode::SERVICE_UNAVAILABLE,
        "{surface}: body={body:?}"
    );
    assert_eq!(body["error"], "index_corpus_unavailable", "{surface}");
    assert_eq!(body["index_id"], index_id, "{surface}");
    assert_eq!(body["retryable"], serde_json::json!(true), "{surface}");
    assert_eq!(body["failure_kind"], "read_failed", "{surface}");
    let message = body["message"].as_str().expect("message is a string");
    assert!(
        message.contains(index_id) && message.contains("#5917"),
        "{surface}: the message names the index and the ticket; message={message}"
    );
}

fn search_query(text: &str) -> crate::core::indexer::SearchQuery {
    crate::core::indexer::SearchQuery {
        text: text.to_string(),
        top_k: 10,
        expand_graph: false,
        compact: false,
        branch_files: None,
        branch_boost: 1.0,
        branch: None,
        stage: None,
        mode: crate::core::indexer::SearchMode::All,
        exclude_archived: false,
        refine_query: None,
        path_prefix: None,
        repos: Vec::new(),
    }
}

fn grep_request(pattern: &str) -> crate::service::grep::GrepRequest {
    serde_json::from_value(serde_json::json!({ "pattern": pattern })).expect("default grep request")
}

/// #5917: an unreadable corpus is a `503 index_corpus_unavailable` naming the
/// index and the fault, never a `200` with an empty result set.
///
/// Why: #4087's guard covers a corpus that fails to OPEN. This one opened, was
/// populated, and then went bad — so the guard passes, the rehydrate that would
/// refill the in-memory caches fails silently, and every lane answers from
/// nothing.
/// What: proves the query works, breaks the reads, and re-runs it. Before the
/// fix the handler returned `200` with `results: []`.
/// Test: this test.
#[tokio::test]
#[serial_test::serial]
async fn search_over_an_unreadable_corpus_returns_503_naming_the_index() {
    let fx = Fixture::build("corpus-read-5917").await;
    let call = |q| {
        search_handler(
            axum::extract::State(Arc::clone(&fx.state)),
            axum::extract::Path("corpus-read-5917".to_string()),
            axum::extract::Json(q),
        )
    };

    let Json(warm) = call(search_query("authenticate_user"))
        .await
        .expect("the query answers while the corpus reads");
    assert!(
        !warm["results"]
            .as_array()
            .expect("results array")
            .is_empty(),
        "the query matches before the corpus breaks; body={warm:?}"
    );

    fx.break_corpus_reads().await;
    let (status, Json(body)) = call(search_query("authenticate_user"))
        .await
        .expect_err("an unreadable corpus must be refused");
    assert_corpus_unavailable(status, &body, "corpus-read-5917", "search");
}

/// #5917: `POST /indexes/:id/grep` over an unreadable corpus is a refusal, not
/// an empty match set.
///
/// Why: grep derives its file set from the chunk corpus, so an unreadable one
/// scans nothing and answers `{matches: [], total: 0}` — which reads as "this
/// literal is nowhere in your code" for a corpus that was never opened.
/// What: proves the pattern matches warm, breaks the reads, re-runs it.
/// Test: this test.
#[tokio::test]
#[serial_test::serial]
async fn grep_over_an_unreadable_corpus_returns_503_naming_the_index() {
    let fx = Fixture::build("grep-read-5917").await;
    let call = || {
        super::files::grep_handler(
            axum::extract::State(Arc::clone(&fx.state)),
            axum::extract::Path("grep-read-5917".to_string()),
            axum::extract::Json(grep_request("authenticate_user")),
        )
    };

    let Json(warm) = call().await.expect("grep answers while the corpus reads");
    assert!(
        !warm.matches.is_empty(),
        "the pattern matches before the corpus breaks"
    );

    fx.break_corpus_reads().await;
    let (status, Json(body)) = call()
        .await
        .expect_err("an unreadable corpus must be refused, not reported as zero matches");
    assert_corpus_unavailable(status, &body, "grep-read-5917", "grep");
}

/// #5917: `POST /grep` refuses when any index in the fan-out cannot be read.
///
/// Why: the global response carries one flat match list and no per-index
/// status, so returning the readable indexes' matches would present a partial
/// sweep as a complete one.
/// What: breaks the only index's corpus and asserts the fan-out refuses.
/// Test: this test.
#[tokio::test]
#[serial_test::serial]
async fn global_grep_over_an_unreadable_corpus_returns_503() {
    let fx = Fixture::build("global-grep-5917").await;
    fx.break_corpus_reads().await;

    let (status, Json(body)) = super::files::global_grep_handler(
        axum::extract::State(Arc::clone(&fx.state)),
        axum::extract::Json(grep_request("authenticate_user")),
    )
    .await
    .expect_err("a fan-out over an unreadable corpus must be refused");
    assert_corpus_unavailable(status, &body, "global-grep-5917", "global grep");
}

/// #5917: `GET /indexes/:id/call_chain` over an unreadable corpus is a `503`,
/// not a `404` saying the entry point does not exist.
///
/// Why: the entry point is resolved against the chunk snapshot. An unreadable
/// corpus produced an empty snapshot, so a real symbol came back as "entry
/// point not found" — an answer about the caller's code rather than about the
/// daemon's state.
/// What: breaks the reads and asserts the status and body.
/// Test: this test.
#[tokio::test]
#[serial_test::serial]
async fn call_chain_over_an_unreadable_corpus_is_503_not_404() {
    let fx = Fixture::build("callchain-read-5917").await;
    fx.break_corpus_reads().await;

    let params: super::files::CallChainParams =
        serde_json::from_value(serde_json::json!({ "entry_point": "authenticate_user" }))
            .expect("call chain params");
    let (status, Json(body)) = super::files::call_chain_handler(
        axum::extract::State(Arc::clone(&fx.state)),
        axum::extract::Path("callchain-read-5917".to_string()),
        axum::extract::Query(params),
    )
    .await
    .expect_err("an unreadable corpus must not render as 'entry point not found'");
    assert_ne!(
        status,
        StatusCode::NOT_FOUND,
        "a real symbol must not be reported nonexistent; body={body:?}"
    );
    assert_corpus_unavailable(status, &body, "callchain-read-5917", "call_chain");
}

/// #5917: the global search fan-out counts an index it dropped for an
/// unreadable corpus, in the response body.
///
/// Why: the fan-out swallows a per-index error so one broken index cannot 500
/// the whole request — correct, but the caller was then told the sweep was
/// complete. `corpus_failed_indexes_skipped` already reports the #4087
/// open-failure case; the read-failure case had no counterpart and no counter.
/// What: breaks the only index's corpus, runs the fan-out, and asserts the
/// count reaches the caller beside an empty result set.
/// Test: this test.
#[tokio::test]
#[serial_test::serial]
async fn global_search_reports_the_index_it_dropped_for_an_unreadable_corpus() {
    let fx = Fixture::build("global-search-5917").await;
    fx.break_corpus_reads().await;

    let Json(body) = super::search_global::global_search_handler(
        axum::extract::State(Arc::clone(&fx.state)),
        axum::extract::Json(super::search_global::GlobalSearchRequest {
            query: "authenticate_user".to_string(),
            top_k: 5,
            full_content: false,
            indexes: None,
            routing: None,
            routing_n: None,
            routing_threshold: None,
            max_fanout_concurrency: None,
            serial: false,
            path_prefix: None,
            repos: Vec::new(),
        }),
    )
    .await
    .expect("the fan-out itself still answers");

    assert_eq!(
        body["corpus_read_failed_indexes_skipped"],
        serde_json::json!(1),
        "the dropped index must be counted in the payload; body={body:?}"
    );
    assert!(
        body["results"]
            .as_array()
            .expect("results array")
            .is_empty(),
        "the broken index contributed no lane; body={body:?}"
    );
}
