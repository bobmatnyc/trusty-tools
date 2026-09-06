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
//!
//! The same fixture carries the #6581 half. There the corpus reads FINE and is
//! merely emptied — M005 clears it and re-chunks it in batches — so each handler
//! reaches the identical false answer by a second route, and owes the identical
//! refusal under its own error code (`index_migration_in_progress`). Only
//! `search` had that guard; the corpus- and graph-backed handlers beside it
//! never call `search_with_drops` and so had none.
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

/// #6581: the global search fan-out counts an index it dropped because a schema
/// migration is rebuilding its corpus.
///
/// Why: the same reporting problem #5917 fixed for an unreadable corpus, one
/// state over. M005 empties the corpus for the length of its re-chunk while
/// every read still succeeds, so a fan-out during a boot-time migration dropped
/// that index with no counter incremented at all — not even the generic
/// corpus-failed one — and the caller saw a complete-looking sweep.
/// What: opens the migration window on the only index, runs the fan-out, and
/// asserts the count reaches the caller beside an empty result set.
/// Test: this test.
#[tokio::test]
#[serial_test::serial]
async fn global_search_reports_the_index_it_dropped_for_a_running_migration() {
    let fx = Fixture::build("global-search-6581").await;
    let handle = fx
        .state
        .registry
        .get(&fx.id)
        .expect("the fixture registers its index");
    let flag = handle.indexer.read().await.migration_flag();
    let _window = crate::core::indexer::MigrationWindow::open(flag);

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
        body["migration_in_progress_indexes_skipped"],
        serde_json::json!(1),
        "a migrating index must be counted, not silently dropped; body={body:?}"
    );
    assert_eq!(
        body["corpus_read_failed_indexes_skipped"],
        serde_json::json!(0),
        "a migration is not a read failure; body={body:?}"
    );
    assert!(
        body["results"]
            .as_array()
            .expect("results array")
            .is_empty(),
        "the migrating index contributes no lane; body={body:?}"
    );
}

// ─── #6581: the corpus reads fine, but a migration has emptied it ────────────

/// Open M005's migration window on `fx`'s index until the guard drops.
///
/// Why: every #6581 test below stages the same state — the flag M005 raises
/// while its re-chunk holds the corpus empty. Raising it by hand rather than
/// running M005 is what keeps these tests about the HANDLERS.
/// Test: the six `*_during_a_migration_*` tests below.
async fn open_migration_window(fx: &Fixture) -> crate::core::indexer::MigrationWindow {
    let handle = fx
        .state
        .registry
        .get(&fx.id)
        .expect("the fixture registers its index");
    let flag = handle.indexer.read().await.migration_flag();
    crate::core::indexer::MigrationWindow::open(flag)
}

/// Assert the shared `503 index_migration_in_progress` contract on one body.
///
/// Why: routing every handler through one producer is worth nothing unless the
/// bodies actually match, and #5917 is the precedent for how they drift — two
/// producers under one error code, one sending `retryable` and the other
/// `failure_kind` + `transient`. Six surfaces asserting one field set is what
/// pins it.
/// Test: the six `*_during_a_migration_*` tests below.
fn assert_migration_in_progress(
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
    assert_eq!(body["error"], "index_migration_in_progress", "{surface}");
    assert_eq!(body["index_id"], index_id, "{surface}");
    assert_eq!(body["failure_kind"], "migrating", "{surface}");
    assert_eq!(body["transient"], serde_json::json!(true), "{surface}");
    assert_eq!(body["retryable"], serde_json::json!(true), "{surface}");
    let message = body["message"].as_str().expect("message is a string");
    assert!(
        message.contains(index_id) && message.contains("#6581"),
        "{surface}: the message names the index and the ticket; message={message}"
    );
}

fn chunks_params() -> super::files::ChunksParams {
    serde_json::from_value(serde_json::json!({})).expect("default chunks params")
}

/// #6581: `GET /indexes/:id/chunks` during a migration is a refusal, not a page
/// of a corpus that is mid-rebuild.
///
/// Why: M005 clears the corpus and refills it batch by batch, so `total` and the
/// page beneath it describe however much has been re-committed so far — with no
/// field saying so. An exporter walking the cursor to exhaustion records that
/// partial corpus as the index's contents.
/// What: proves the page answers outside the window, opens it, and re-runs.
/// Test: this test.
#[tokio::test]
#[serial_test::serial]
async fn chunks_during_a_migration_is_503_not_a_partial_page() {
    let fx = Fixture::build("chunks-migrating-6581").await;
    let call = || {
        super::files::get_index_chunks_handler(
            axum::extract::State(Arc::clone(&fx.state)),
            axum::extract::Path("chunks-migrating-6581".to_string()),
            axum::extract::Query(chunks_params()),
        )
    };

    let Json(warm) = call().await.expect("the page answers outside the window");
    assert!(
        warm["total"].as_u64().expect("total is a number") > 0,
        "the fixture holds chunks before the window opens; body={warm:?}"
    );

    let _window = open_migration_window(&fx).await;
    let (status, Json(body)) = call()
        .await
        .expect_err("a mid-rebuild corpus must not be paged as the whole one");
    assert_migration_in_progress(status, &body, "chunks-migrating-6581", "chunks");
}

/// #6581: `GET /indexes/:id/call_chain` during a migration is a refusal, not a
/// `404` saying the entry point does not exist.
///
/// Why: the identical disguise the #5917 sibling covers, reached by a different
/// route — the snapshot reads cleanly here and is simply empty, and the symbol
/// graph stays empty until M005's Step 6 rebuild even after the corpus refills.
/// What: opens the window and asserts the status and body.
/// Test: this test.
#[tokio::test]
#[serial_test::serial]
async fn call_chain_during_a_migration_is_503_not_404() {
    let fx = Fixture::build("callchain-migrating-6581").await;
    let _window = open_migration_window(&fx).await;

    let params: super::files::CallChainParams =
        serde_json::from_value(serde_json::json!({ "entry_point": "authenticate_user" }))
            .expect("call chain params");
    let (status, Json(body)) = super::files::call_chain_handler(
        axum::extract::State(Arc::clone(&fx.state)),
        axum::extract::Path("callchain-migrating-6581".to_string()),
        axum::extract::Query(params),
    )
    .await
    .expect_err("a migrating index must not render as 'entry point not found'");
    assert_ne!(
        status,
        StatusCode::NOT_FOUND,
        "a real symbol must not be reported nonexistent; body={body:?}"
    );
    assert_migration_in_progress(status, &body, "callchain-migrating-6581", "call_chain");
}

/// #6581: `POST /indexes/:id/grep` during a migration is a refusal, not an empty
/// match set.
///
/// Why: grep derives its file set from the chunk corpus, so a pass that has
/// emptied it scans no files and answers `{matches: [], total: 0}` — "this
/// literal is nowhere in your code" for a literal that is right there on disk.
/// What: proves the pattern matches outside the window, opens it, and re-runs.
/// Test: this test.
#[tokio::test]
#[serial_test::serial]
async fn grep_during_a_migration_is_503_not_zero_matches() {
    let fx = Fixture::build("grep-migrating-6581").await;
    let call = || {
        super::files::grep_handler(
            axum::extract::State(Arc::clone(&fx.state)),
            axum::extract::Path("grep-migrating-6581".to_string()),
            axum::extract::Json(grep_request("authenticate_user")),
        )
    };

    let Json(warm) = call().await.expect("grep answers outside the window");
    assert!(
        !warm.matches.is_empty(),
        "the pattern matches before the window opens"
    );

    let _window = open_migration_window(&fx).await;
    let (status, Json(body)) = call()
        .await
        .expect_err("a migrating index must not be reported as zero matches");
    assert_migration_in_progress(status, &body, "grep-migrating-6581", "grep");
}

/// #6581: `POST /grep` refuses when any index in the fan-out is migrating.
///
/// Why: the global response carries one flat match list and no per-index status,
/// so returning the other indexes' matches presents an incomplete sweep as a
/// complete one — the same argument the #5917 fan-out refusal makes.
/// What: opens the window on the only index and asserts the fan-out refuses.
/// Test: this test.
#[tokio::test]
#[serial_test::serial]
async fn global_grep_during_a_migration_is_503() {
    let fx = Fixture::build("global-grep-migrating-6581").await;
    let _window = open_migration_window(&fx).await;

    let (status, Json(body)) = super::files::global_grep_handler(
        axum::extract::State(Arc::clone(&fx.state)),
        axum::extract::Json(grep_request("authenticate_user")),
    )
    .await
    .expect_err("a fan-out across a migrating index must be refused");
    assert_migration_in_progress(status, &body, "global-grep-migrating-6581", "global grep");
}

/// #6581: `GET /indexes/:id/graph/neighbors` during a migration is a refusal,
/// not `count: 0`.
///
/// Why: M005's clear empties the symbol graph outright and nothing refills it
/// until Step 6, so a BFS in that window answers "this node has no edges" for a
/// graph that is merely mid-rebuild.
/// What: opens the window and asserts the status and body. The request is
/// deliberately well-formed: the guard sits AFTER the direction and edge-kind
/// parsing, so a malformed one still earns its permanent 400 rather than a
/// retryable 503 that would loop a caller with a typo.
/// Test: this test.
#[tokio::test]
#[serial_test::serial]
async fn graph_neighbors_during_a_migration_is_503_not_count_zero() {
    let fx = Fixture::build("neighbors-migrating-6581").await;
    let _window = open_migration_window(&fx).await;

    let (status, Json(body)) = super::contrib_graph::graph_neighbors_handler(
        axum::extract::State(Arc::clone(&fx.state)),
        axum::extract::Path("neighbors-migrating-6581".to_string()),
        axum::extract::Query(super::contrib_graph::NeighborsParams {
            node: "authenticate_user".to_string(),
            direction: None,
            edge_kinds: None,
            max_hops: None,
        }),
    )
    .await
    .expect_err("a migrating index must not answer an empty neighbour list");
    assert_migration_in_progress(status, &body, "neighbors-migrating-6581", "graph neighbors");
}

/// #6581: `POST /indexes/:id/graph` during a migration is refused before it
/// writes anything.
///
/// Why: this is the WRITE on the surface. It calls `rebuild_symbol_graph_now`,
/// which races M005's own Step 6 rebuild over that same graph, and it answers
/// with post-merge totals read from a corpus M005 has emptied. Refusing before
/// the persist is what keeps the caller from holding a durable contribution
/// behind a 503 whose body says nothing about whether anything was stored.
/// What: opens the window, ingests, and asserts both the refusal and that no
/// contribution reached `kg_contrib`.
/// Test: this test.
#[tokio::test]
#[serial_test::serial]
async fn graph_ingest_during_a_migration_is_503_and_stores_nothing() {
    let fx = Fixture::build("ingest-migrating-6581").await;
    let _window = open_migration_window(&fx).await;

    let (status, Json(body)) = super::contrib_graph::ingest_graph_handler(
        axum::extract::State(Arc::clone(&fx.state)),
        axum::extract::Path("ingest-migrating-6581".to_string()),
        axum::extract::Json(super::contrib_graph::IngestGraphRequest {
            schema: Some("navigatsql/kggraph@1".into()),
            producer: "navigatsql".into(),
            producer_version: None,
            git_sha: None,
            nodes: vec![crate::core::corpus::contrib::ContribNode {
                id: "dbo.orders".into(),
                kind: "table".into(),
            }],
            edges: Vec::new(),
        }),
    )
    .await
    .expect_err("an ingest during a migration must be refused, not merged");
    assert_migration_in_progress(status, &body, "ingest-migrating-6581", "graph ingest");
    assert!(
        fx.corpus
            .load_contrib_graphs()
            .expect("read kg_contrib")
            .is_empty(),
        "a refused ingest must not have persisted a contribution"
    );
}
