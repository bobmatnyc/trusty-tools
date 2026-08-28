//! Socket-versus-HTTP parity for the query surface (#6285 slice 3).
//!
//! Why: the failure this rules out is the two transports answering a QUESTION
//! differently — a consumer migrated onto the socket by the retire slice
//! getting different hits, a different fusion order, a different drop tally, or
//! a different refusal than the HTTP route it replaced. Every test here drives
//! the REAL axum router and the REAL RPC router against ONE shared
//! `SearchAppState`, so a `*_report` function that stopped being the body both
//! transports serve fails these rather than surfacing in a consumer.
//!
//! **The index holds a real corpus and the queries are real.** Slice 2's read
//! parity could be proved over shapes; a search cannot. The fixture wires a
//! `MockEmbedder` and a `UsearchStore` so the vector lane is live, plants four
//! chunks across three files, and every case asserts a non-empty answer BEFORE
//! comparing the two transports — an empty result set would make a body
//! comparison pass while proving nothing about ranking or fusion.
//!
//! **Host-sampled fields are excluded, and only those.** Every query body
//! carries `latency_ms`, measured from the host clock around the call, so two
//! runs microseconds apart legitimately differ. That is the #6358 exclusion
//! precedent slice 1 applied to health and slice 2 to `graph`'s `generated_at`.
//! Every other field is derived from state and must match byte for byte.
//!
//! Test: this file IS the test module for `super`.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use tower::ServiceExt;
use trusty_common::uds::server::{RpcError, RpcRouter, CODE_INVALID_PARAMS};

use crate::core::chunker::{ChunkType, RawChunk};
use crate::core::embed::{Embedder, MockEmbedder};
use crate::core::indexer::CodeIndexer;
use crate::core::registry::{IndexHandle, IndexId, IndexRegistry};
use crate::core::store::{UsearchStore, VectorStore};
use crate::service::concurrency::ConcurrencyLimiter;
use crate::service::query_timeout::QueryTimeoutConfig;
use crate::service::rpc::error::{CODE_DEADLINE_EXCEEDED, CODE_NOT_FOUND, CODE_UNAVAILABLE};
use crate::service::rpc::queries;
use crate::service::server::{build_router_on, SearchAppState};

/// The index every parity case queries.
const INDEX: &str = "parity-6285-queries";

/// An id no store holds, for the not-found arm.
const MISSING: &str = "parity-6285-queries-absent";

/// The vector dimensionality the mock embedder and the HNSW store agree on.
const DIM: usize = 16;

/// The term every non-trivial query in this file searches for.
///
/// It appears in the content of two of the four planted chunks and in one
/// function name, so a lexical hit, a vector hit and a typeahead prefix hit are
/// all reachable from one word.
const TERM: &str = "authenticate";

/// One planted chunk.
fn raw(id: &str, file: &str, function_name: Option<&str>, content: &str) -> RawChunk {
    RawChunk {
        id: id.to_string(),
        file: file.to_string(),
        start_line: 1,
        end_line: 4,
        content: content.to_string(),
        function_name: function_name.map(str::to_string),
        language: Some("rust".to_string()),
        chunk_type: ChunkType::Code,
        calls: Vec::new(),
        inherits_from: Vec::new(),
        chunk_depth: 0,
        parent_chunk_id: None,
        child_chunk_ids: Vec::new(),
        nlp_keywords: Vec::new(),
        nlp_code_refs: Vec::new(),
        virtual_terms: Vec::new(),
    }
}

/// One state, both routers, over an index with a live corpus and vector lane.
///
/// Why both routers are built from the SAME `Arc`: that is the property under
/// test at one level down — `build_router_on` and `queries::register` must read
/// one registry, one limiter and one deadline. Two `Arc::new` calls here would
/// make every assertion below pass against two independent daemons.
async fn fixture_with(state: SearchAppState) -> (Arc<SearchAppState>, Router, RpcRouter) {
    let state = Arc::new(state);
    let http = build_router_on(
        Arc::clone(&state),
        trusty_common::server::SelfOrigins::default(),
    );
    let rpc = queries::register(RpcRouter::new(), &state);
    (state, http, rpc)
}

/// The four chunks every fixture plants: `(chunk id, file, function, content)`.
const CORPUS: [(&str, &str, Option<&str>, &str); 4] = [
    (
        "src/auth.rs:1:4",
        "src/auth.rs",
        Some(TERM),
        "fn authenticate(token: &str) -> bool { verify(token) }",
    ),
    (
        "src/middleware.rs:1:4",
        "src/middleware.rs",
        Some("guard"),
        "fn guard(req: Request) { authenticate(req.token); }",
    ),
    (
        "src/lib.rs:1:4",
        "src/lib.rs",
        Some("boot"),
        "fn boot() { println!(\"start\"); }",
    ),
    (
        "src/util.rs:1:4",
        "src/util.rs",
        None,
        "const RETRIES: usize = 3;",
    ),
];

/// The registry every fixture serves, plus the embedder it was built with.
async fn planted_registry(root: &std::path::Path) -> (IndexRegistry, Arc<dyn Embedder>) {
    let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::new(DIM));
    let store: Arc<dyn VectorStore> = Arc::new(UsearchStore::new(DIM).expect("usearch"));
    let indexer =
        CodeIndexer::new(INDEX, root).with_components(Arc::clone(&embedder), Arc::clone(&store));
    for (id, file, name, content) in CORPUS {
        indexer
            .add_chunk(raw(id, file, name, content))
            .await
            .expect("plant a chunk");
    }

    let registry = IndexRegistry::new();
    registry.register(IndexHandle::bare(
        IndexId::new(INDEX),
        Arc::new(tokio::sync::RwLock::new(indexer)),
        root.to_path_buf(),
    ));
    (registry, embedder)
}

/// The root the corpus-reading fixtures use.
///
/// Why a path that does not exist: `search_report` wakes an idle watcher for
/// the index it serves and, when one actually starts, kicks a background
/// reconcile. A reconcile walking a real tree would re-chunk it BETWEEN this
/// file's two transport calls, and the parity comparison would fail on a
/// difference that is not a transport difference at all. A root nothing can
/// watch keeps the corpus exactly as planted. Only the grep cases need real
/// files — see [`grep_fixture`].
const ABSENT_ROOT: &str = "/nonexistent/parity-6285-queries";

/// The default fixture: production guards, one planted index, no files on disk.
async fn fixture() -> (Arc<SearchAppState>, Router, RpcRouter) {
    let (registry, embedder) = planted_registry(std::path::Path::new(ABSENT_ROOT)).await;
    let (state, http, rpc) = fixture_with(SearchAppState::new(registry)).await;
    state.install_embedder(embedder).await;
    (state, http, rpc)
}

/// The same fixture with the two guards shrunk to something a test can trip:
/// one admission slot with no queue, and a 50 ms query deadline.
async fn guarded_fixture() -> (Arc<SearchAppState>, Router, RpcRouter) {
    let (registry, embedder) = planted_registry(std::path::Path::new(ABSENT_ROOT)).await;
    let state = SearchAppState::new(registry).with_query_guards(
        ConcurrencyLimiter::with_limits(1, 0),
        QueryTimeoutConfig::from_duration(std::time::Duration::from_millis(50)),
    );
    let (state, http, rpc) = fixture_with(state).await;
    state.install_embedder(embedder).await;
    (state, http, rpc)
}

/// The grep fixture: the same corpus, over a root whose files really exist.
///
/// Why separate: grep resolves its file list from the chunk corpus and then
/// reads each file FROM DISK, so it is the one family whose answer depends on
/// the tree rather than the index. Neither grep route wakes a watcher, so the
/// reconcile hazard [`ABSENT_ROOT`] avoids does not apply here.
///
/// The `TempDir` is returned because dropping it deletes the tree the assertions
/// depend on.
async fn grep_fixture() -> (tempfile::TempDir, Router, RpcRouter) {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(tmp.path().join("src")).expect("create src/");
    for (_, file, _, content) in CORPUS {
        std::fs::write(tmp.path().join(file), content).expect("write a source file");
    }

    let (registry, embedder) = planted_registry(tmp.path()).await;
    let (state, http, rpc) = fixture_with(SearchAppState::new(registry)).await;
    state.install_embedder(embedder).await;
    (tmp, http, rpc)
}

/// One request against the real router, as `(status, body-text)`.
async fn http_raw(router: &Router, request: Request<Body>) -> (StatusCode, String) {
    let response = router
        .clone()
        .oneshot(request)
        .await
        .expect("the router must answer");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read the body");
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

/// `POST uri` with a JSON body, as parsed JSON, asserting a 200.
async fn http_post(router: &Router, uri: &str, body: serde_json::Value) -> serde_json::Value {
    let request = Request::post(uri)
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).expect("encode")))
        .expect("build the request");
    let (status, text) = http_raw(router, request).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "POST {uri} answered {status}: {text}"
    );
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("POST {uri} body is not JSON: {e}"))
}

/// `GET uri`, as parsed JSON, asserting a 200.
async fn http_get(router: &Router, uri: &str) -> serde_json::Value {
    let request = Request::get(uri)
        .body(Body::empty())
        .expect("build the request");
    let (status, text) = http_raw(router, request).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "GET {uri} answered {status}: {text}"
    );
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("GET {uri} body is not JSON: {e}"))
}

async fn dispatch(
    rpc: &RpcRouter,
    method: &str,
    params: serde_json::Value,
) -> trusty_common::uds::server::RpcResponse {
    let frame = serde_json::to_vec(&serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": method, "params": params,
    }))
    .expect("encode the frame");
    rpc.dispatch(&frame).await
}

/// One RPC call against the real router, asserting a result frame.
async fn rpc_ok(rpc: &RpcRouter, method: &str, params: serde_json::Value) -> serde_json::Value {
    let response = dispatch(rpc, method, params).await;
    assert!(
        response.error.is_none(),
        "{method} must answer a result: {:?}",
        response.error
    );
    response.result.expect("a non-error frame carries a result")
}

/// One RPC call against the real router, asserting an error frame.
async fn rpc_err(rpc: &RpcRouter, method: &str, params: serde_json::Value) -> RpcError {
    let response = dispatch(rpc, method, params).await;
    response
        .error
        .unwrap_or_else(|| panic!("{method} must be refused; got {:?}", response.result))
}

/// Drop `latency_ms`, which both sides sample from the host clock (#6358).
fn without_latency(mut body: serde_json::Value) -> serde_json::Value {
    if let Some(obj) = body.as_object_mut() {
        obj.remove("latency_ms");
    }
    body
}

/// Assert `body` carries at least one result row, so the parity comparison
/// below is comparing an ANSWER rather than two empty lists.
fn assert_non_trivial(body: &serde_json::Value, field: &str, what: &str) {
    let rows = body[field]
        .as_array()
        .unwrap_or_else(|| panic!("{what}: `{field}` must be an array, got {body}"));
    assert!(
        !rows.is_empty(),
        "{what}: the fixture must produce a non-empty answer, or this parity case proves nothing: {body}"
    );
}

/// Why: `search` is the most-read body this daemon serves — the lane
/// down-shift, the five drop counters and the whole `meta` block ride on it, and
/// a socket that fused differently would answer a different question in the same
/// shape.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
async fn search_over_the_socket_matches_the_http_body() {
    let (_state, http, rpc) = fixture().await;
    let query = serde_json::json!({ "text": TERM, "top_k": 5, "expand_graph": false });

    let over_socket = rpc_ok(
        &rpc,
        queries::METHOD_QUERY,
        serde_json::json!({ "index_id": INDEX, "body": query }),
    )
    .await;
    assert_non_trivial(&over_socket, "results", "search");
    assert_eq!(
        over_socket["meta"]["dropped_total"],
        serde_json::json!(0),
        "no row should be dropped for this query: {over_socket}"
    );

    let over_http = http_post(&http, &format!("/indexes/{INDEX}/search"), query).await;
    assert_eq!(without_latency(over_socket), without_latency(over_http));
}

/// Why: the fan-out re-runs RRF across per-index lanes and reports three
/// separate incompleteness counters. A socket that dropped one of them would
/// tell a caller its sweep was complete when it was not.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
async fn global_search_over_the_socket_matches_the_http_body() {
    let (_state, http, rpc) = fixture().await;
    let req = serde_json::json!({ "query": TERM, "top_k": 5 });

    let over_socket = rpc_ok(&rpc, queries::METHOD_QUERY_ALL, req.clone()).await;
    assert_non_trivial(&over_socket, "results", "global search");
    assert_eq!(
        over_socket["indexes_searched"],
        serde_json::json!([INDEX]),
        "the one planted index must contribute a lane: {over_socket}"
    );

    let over_http = http_post(&http, "/search", req).await;
    assert_eq!(without_latency(over_socket), without_latency(over_http));
}

/// Why: grep compiles caller-supplied regex and glob options into a matcher.
/// Every option that changed the matcher on one transport and not the other
/// would silently return a different set of lines, so this drives the flags a
/// caller most commonly sends together.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
async fn grep_over_the_socket_matches_the_http_body() {
    let (_tmp, http, rpc) = grep_fixture().await;
    let req = serde_json::json!({
        "pattern": "AUTHENTICATE", "case_insensitive": true, "glob": "src/*.rs", "max_results": 10,
    });

    let over_socket = rpc_ok(
        &rpc,
        queries::METHOD_GREP,
        serde_json::json!({ "index_id": INDEX, "body": req.clone() }),
    )
    .await;
    assert_non_trivial(&over_socket, "matches", "grep");

    let over_http = http_post(&http, &format!("/indexes/{INDEX}/grep"), req).await;
    // `GrepResponse` carries no host-sampled field, so this is a whole-body
    // comparison with nothing excluded.
    assert_eq!(over_socket, over_http);
}

/// Why: the global grep's tolerance rules differ from the per-index route's —
/// an unknown `index_id` narrows the sweep to nothing rather than 404-ing. A
/// socket that inherited the per-index rule instead would turn a tolerated
/// stale id into a hard failure.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
async fn global_grep_over_the_socket_matches_the_http_body() {
    let (_tmp, http, rpc) = grep_fixture().await;

    let req = serde_json::json!({ "pattern": TERM, "max_results": 10 });
    let over_socket = rpc_ok(&rpc, queries::METHOD_GREP_ALL, req.clone()).await;
    assert_non_trivial(&over_socket, "matches", "global grep");
    assert_eq!(over_socket, http_post(&http, "/grep", req).await);

    // An id no store holds narrows to an empty sweep on both transports.
    let req = serde_json::json!({ "pattern": TERM, "index_id": MISSING, "max_results": 10 });
    let over_socket = rpc_ok(&rpc, queries::METHOD_GREP_ALL, req.clone()).await;
    assert_eq!(
        over_socket["total"],
        serde_json::json!(0),
        "an unknown index_id narrows the fan-out rather than refusing it"
    );
    assert_eq!(over_socket, http_post(&http, "/grep", req).await);
}

/// Why: `search_similar` resolves a seed chunk, then re-embeds it when the LRU
/// cache misses (#484). A socket that took the other branch would answer from a
/// different seed embedding and return different neighbours.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
async fn search_similar_over_the_socket_matches_the_http_body() {
    let (_state, http, rpc) = fixture().await;
    let req = serde_json::json!({ "file": "src/auth.rs", "function": TERM, "top_k": 3 });

    let over_socket = rpc_ok(
        &rpc,
        queries::METHOD_SIMILAR,
        serde_json::json!({ "index_id": INDEX, "body": req.clone() }),
    )
    .await;
    assert_non_trivial(&over_socket, "results", "search_similar");
    assert_eq!(
        over_socket["seed_chunk_id"],
        serde_json::json!("src/auth.rs:1:4"),
        "the seed must resolve to the planted chunk: {over_socket}"
    );

    let over_http = http_post(&http, &format!("/indexes/{INDEX}/search_similar"), req).await;
    assert_eq!(without_latency(over_socket), without_latency(over_http));
}

/// Why: typeahead is the one query route whose HTTP form is a query string, so
/// it is the one that carries slice 2's `IndexScoped` flatten. The clamp on
/// `limit` and the lane a `mode` selects are both echoed in the body, so a
/// transport that decoded either differently shows up here.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
async fn typeahead_over_the_socket_matches_the_http_body() {
    let (_state, http, rpc) = fixture().await;

    let over_socket = rpc_ok(
        &rpc,
        queries::METHOD_TYPEAHEAD,
        serde_json::json!({ "index_id": INDEX, "q": "auth", "limit": 5, "mode": "lexical" }),
    )
    .await;
    assert_non_trivial(&over_socket, "hits", "typeahead");
    assert_eq!(over_socket["mode"], serde_json::json!("lexical"));

    let over_http = http_get(
        &http,
        &format!("/indexes/{INDEX}/typeahead?q=auth&limit=5&mode=lexical"),
    )
    .await;
    assert_eq!(without_latency(over_socket), without_latency(over_http));

    // The clamp: `limit` above `MAX_TYPEAHEAD_LIMIT` is capped identically, and
    // a query string delivers it as text where the socket delivers a number.
    let over_socket = rpc_ok(
        &rpc,
        queries::METHOD_TYPEAHEAD,
        serde_json::json!({ "index_id": INDEX, "q": "auth", "limit": 999 }),
    )
    .await;
    let over_http = http_get(
        &http,
        &format!("/indexes/{INDEX}/typeahead?q=auth&limit=999"),
    )
    .await;
    assert_eq!(without_latency(over_socket), without_latency(over_http));
}

/// Why: an id no store holds is the most-hit refusal on this surface, and the
/// retire slice's consumers will branch on the code alone. Every index-scoped
/// query must answer the SAME code for it — including `search`, whose route
/// reaches the cold store through the lazy-load path rather than a bare
/// registry lookup.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
async fn an_unknown_index_reports_not_found_on_every_query_method() {
    let (_state, _http, rpc) = fixture().await;

    for (method, params) in [
        (
            queries::METHOD_QUERY,
            serde_json::json!({ "index_id": MISSING, "body": { "text": TERM } }),
        ),
        (
            queries::METHOD_GREP,
            serde_json::json!({ "index_id": MISSING, "body": { "pattern": TERM } }),
        ),
        (
            queries::METHOD_SIMILAR,
            serde_json::json!({ "index_id": MISSING, "body": { "file": "src/auth.rs" } }),
        ),
        (
            queries::METHOD_TYPEAHEAD,
            serde_json::json!({ "index_id": MISSING, "q": "auth" }),
        ),
    ] {
        let err = rpc_err(&rpc, method, params).await;
        assert_eq!(
            err.code, CODE_NOT_FOUND,
            "{method} must report an unknown index as not-found, got {err}"
        );
        assert!(
            err.message.contains(MISSING),
            "{method} must name the index an operator has to act on: {err}"
        );
    }
}

/// Why: #882 rejects an empty query before it reaches the index, because an
/// empty one degenerates into an arbitrary top-k k-NN sweep. That guard lives
/// in the shared core, so both transports must refuse — and the socket must say
/// `invalid_params` rather than folding it into the internal-error bucket,
/// which would tell a client to file a bug instead of fixing its request.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
async fn an_empty_query_reports_invalid_params_on_both_transports() {
    let (_state, http, rpc) = fixture().await;

    let request = Request::post(format!("/indexes/{INDEX}/search"))
        .header("content-type", "application/json")
        .body(Body::from(r#"{"text":"   "}"#))
        .expect("build the request");
    let (status, _body) = http_raw(&http, request).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let err = rpc_err(
        &rpc,
        queries::METHOD_QUERY,
        serde_json::json!({ "index_id": INDEX, "body": { "text": "   " } }),
    )
    .await;
    assert_eq!(err.code, CODE_INVALID_PARAMS, "{err}");
    assert!(err.message.contains("must not be empty"), "{err}");
}

/// Why: a pattern that does not compile is the caller's fault on both
/// transports, and it is refused before any index is touched — so the code must
/// be `invalid_params` even though nothing about the index was wrong.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
async fn a_bad_regex_reports_invalid_params_on_both_transports() {
    let (_state, http, rpc) = fixture().await;

    let request = Request::post(format!("/indexes/{INDEX}/grep"))
        .header("content-type", "application/json")
        .body(Body::from(r#"{"pattern":"fn ("}"#))
        .expect("build the request");
    let (status, _body) = http_raw(&http, request).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let err = rpc_err(
        &rpc,
        queries::METHOD_GREP,
        serde_json::json!({ "index_id": INDEX, "body": { "pattern": "fn (" } }),
    )
    .await;
    assert_eq!(err.code, CODE_INVALID_PARAMS, "{err}");
}

/// Why: `SearchQuery` rejects unknown fields because a misspelled FILTER means
/// "returns too much data" (#3401), and that guard is the reason `IndexBody`
/// nests the request rather than flattening it — a flattened decode would not
/// hand serde the same document. Each transport classifies a malformed request
/// in its own vocabulary (axum answers a 4xx, the router answers
/// `invalid_params`); what must not differ is that BOTH refuse.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
async fn an_unknown_search_field_reports_invalid_params_on_both_transports() {
    let (_state, http, rpc) = fixture().await;

    let request = Request::post(format!("/indexes/{INDEX}/search"))
        .header("content-type", "application/json")
        .body(Body::from(r#"{"text":"authenticate","path_prefx":"src/"}"#))
        .expect("build the request");
    let (status, _body) = http_raw(&http, request).await;
    assert!(
        status.is_client_error(),
        "a misspelled filter must not be silently ignored, got {status}"
    );

    let err = rpc_err(
        &rpc,
        queries::METHOD_QUERY,
        serde_json::json!({
            "index_id": INDEX, "body": { "text": TERM, "path_prefx": "src/" },
        }),
    )
    .await;
    assert_eq!(err.code, CODE_INVALID_PARAMS, "{err}");
    assert!(err.message.contains("path_prefx"), "{err}");
}

/// Why: these six methods are the routes axum admission-limits (#2845). A
/// socket that admitted callers the HTTP door had already refused would leave
/// the daemon serving twice the configured concurrency while both doors
/// reported obeying it. Proved by holding the ONE permit the fixture's limiter
/// has and asserting both doors then refuse.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
async fn queries_are_refused_when_the_shared_limiter_is_saturated() {
    let (state, http, rpc) = guarded_fixture().await;

    // Hold the only slot for the duration of both requests.
    let held = crate::service::concurrency::admit(&state.query_limiter)
        .await
        .expect("the first caller is admitted");

    let request = Request::post(format!("/indexes/{INDEX}/search"))
        .header("content-type", "application/json")
        .body(Body::from(format!(r#"{{"text":"{TERM}"}}"#)))
        .expect("build the request");
    let (status, body) = http_raw(&http, request).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(body.contains("server_busy"), "{body}");

    let err = rpc_err(
        &rpc,
        queries::METHOD_QUERY,
        serde_json::json!({ "index_id": INDEX, "body": { "text": TERM } }),
    )
    .await;
    assert_eq!(
        err.code, CODE_UNAVAILABLE,
        "a busy daemon clears on its own, so the retryable 503 class is the one: {err}"
    );
    assert!(err.message.contains("server_busy"), "{err}");

    drop(held);
}

/// Why: these six are also the routes axum bounds with the interactive query
/// deadline (#907). Without it a socket search against a stalled index hangs
/// with nothing to cancel it, while the HTTP route for the identical query
/// answers 408. Proved by holding the index's write lock, which is a real stall
/// rather than an injected one — every query path takes that read lock.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
async fn a_query_that_outlasts_the_deadline_reports_the_same_refusal_on_both_transports() {
    let (state, http, rpc) = guarded_fixture().await;
    let handle = state
        .registry
        .get(&IndexId::new(INDEX))
        .expect("the planted index");
    let blocked = handle.indexer.write().await;

    let request = Request::post(format!("/indexes/{INDEX}/search"))
        .header("content-type", "application/json")
        .body(Body::from(format!(r#"{{"text":"{TERM}"}}"#)))
        .expect("build the request");
    let (status, body) = http_raw(&http, request).await;
    assert_eq!(status, StatusCode::REQUEST_TIMEOUT);
    assert!(body.contains("query_timeout"), "{body}");

    let err = rpc_err(
        &rpc,
        queries::METHOD_QUERY,
        serde_json::json!({ "index_id": INDEX, "body": { "text": TERM } }),
    )
    .await;
    assert_eq!(
        err.code, CODE_DEADLINE_EXCEEDED,
        "an expired deadline is the caller's own timeout, not an internal fault: {err}"
    );
    assert!(err.message.contains("query_timeout"), "{err}");

    drop(blocked);
}
