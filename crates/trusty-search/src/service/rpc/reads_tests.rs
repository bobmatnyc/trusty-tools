//! Socket-versus-HTTP parity for the read surface (#6285 slice 2).
//!
//! Why: the failure this rules out is the two transports answering differently
//! — a consumer migrated onto the socket by the retire slice seeing a different
//! index list, chunk page, graph, or refusal than the HTTP route it replaced.
//! Every test here drives the REAL axum router and the REAL RPC router against
//! ONE shared `SearchAppState`, so a `*_report` function that stopped being the
//! body both transports serve fails these rather than surfacing in a consumer.
//!
//! What: one `*_over_the_socket_matches_the_http_body` per route family, plus
//! the refusal classes — an unknown index, a cold-parked one, a KG-disabled
//! one, and a param the caller got wrong. The two 503 halves are both driven
//! end to end: a cold-parked index for the retryable code and a `skip_kg` index
//! for the permanent one, because the code is the ONLY thing a socket client can
//! branch on and reading one as the other sends a caller either to poll forever
//! or to give up on an index a search would restore (#6285 slice 3).
//!
//! **Host-sampled fields are excluded, and only those.** `graph`'s
//! `generated_at` is `Utc::now()` at answer time, so two reads microseconds
//! apart legitimately differ. Excluding host-sampled values is what #6358
//! established for the doctor parity test and what slice 1's
//! `health_over_the_socket_matches_the_http_body` already does. Every other
//! field is derived from state and must match byte for byte.
//!
//! Test: this file IS the test module for `super`.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use tower::ServiceExt;
use trusty_common::uds::server::{RpcError, RpcRouter, CODE_INVALID_PARAMS};

use crate::core::chunker::{ChunkType, RawChunk};
use crate::core::indexer::CodeIndexer;
use crate::core::registry::{IndexHandle, IndexId, IndexRegistry};
use crate::service::rpc::error::{CODE_NOT_FOUND, CODE_UNAVAILABLE, CODE_UNAVAILABLE_PERMANENT};
use crate::service::server::{build_router_on, SearchAppState};

use crate::service::rpc::reads;

/// The index every parity case reads.
const INDEX: &str = "parity-6285";

/// A second index, registered with the KG component off, for the one refusal
/// class that needs a live handle rather than a missing one.
const KG_OFF_INDEX: &str = "parity-6285-kg-off";

/// An id no store holds, for the not-found arm.
const MISSING: &str = "parity-6285-absent";

/// A third index, present ONLY in the cold store, for the transient-503 arm.
///
/// This is the state `restore_eager_entry` leaves an index in when its eager
/// restore times out — the default configuration, no env var involved (#4250)
/// — and the one `TRUSTY_MAX_RESIDENT_INDEXES` produces on purpose (#2161).
const COLD_INDEX: &str = "parity-6285-cold";

/// A minimal in-memory chunk. `function_name` is what
/// `call_chain::resolve_entry_point` falls back to when the symbol graph is
/// empty, so it is the field that makes a `search.call_chain` success reachable
/// without building a real graph.
fn raw(id: &str, function_name: Option<&str>) -> RawChunk {
    RawChunk {
        id: id.to_string(),
        file: "src/auth.rs".to_string(),
        start_line: 1,
        end_line: 3,
        content: "fn authenticate() {}".to_string(),
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

/// One state, both routers.
///
/// Why both are built from the SAME `Arc`: that is the property under test at
/// one level down — `build_router_on` and `reads::register` must read one
/// registry. Two `Arc::new` calls here would make every assertion below pass
/// against two independent daemons.
async fn fixture() -> (Arc<SearchAppState>, Router, RpcRouter) {
    let registry = IndexRegistry::new();

    let indexer = CodeIndexer::new(INDEX, "/nonexistent/parity-6285");
    for (id, name) in [
        ("src/auth.rs:1:3", Some("authenticate")),
        ("src/lib.rs:1:3", None),
        ("src/main.rs:1:3", None),
    ] {
        indexer
            .add_chunk(raw(id, name))
            .await
            .expect("plant a chunk");
    }
    registry.register(IndexHandle::bare(
        IndexId::new(INDEX),
        Arc::new(tokio::sync::RwLock::new(indexer)),
        "/nonexistent/parity-6285".into(),
    ));

    let mut kg_off = IndexHandle::bare(
        IndexId::new(KG_OFF_INDEX),
        Arc::new(tokio::sync::RwLock::new(CodeIndexer::new(
            KG_OFF_INDEX,
            "/nonexistent/parity-6285-kg-off",
        ))),
        "/nonexistent/parity-6285-kg-off".into(),
    );
    kg_off.skip_kg = true;
    registry.register(kg_off);

    let state = Arc::new(SearchAppState::new(registry));
    // Registered and built, but not resident — the third index exists only
    // here, so an index-scoped read finds it in the cold store rather than the
    // registry. It is invisible to `GET /indexes`, which lists the registry.
    state
        .cold_store
        .register_cold_entries(vec![crate::service::persistence::PersistedIndex {
            id: COLD_INDEX.to_string(),
            root_path: "/nonexistent/parity-6285-cold".into(),
            ..Default::default()
        }]);

    let http = build_router_on(
        Arc::clone(&state),
        trusty_common::server::SelfOrigins::default(),
    );
    let rpc = reads::register(RpcRouter::new(), &state);
    (state, http, rpc)
}

/// `GET uri` against the real router, as parsed JSON, asserting a 200.
async fn http_json(router: &Router, uri: &str) -> serde_json::Value {
    let (status, body) = http_raw(router, uri).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "GET {uri} answered {status}: {body}"
    );
    serde_json::from_str(&body)
        .unwrap_or_else(|e| panic!("GET {uri} body is not JSON: {e}: {body}"))
}

/// `GET uri` against the real router, as `(status, body-text)`.
async fn http_raw(router: &Router, uri: &str) -> (StatusCode, String) {
    let response = router
        .clone()
        .oneshot(
            Request::get(uri)
                .body(Body::empty())
                .expect("build the request"),
        )
        .await
        .expect("the router must answer");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read the body");
    (status, String::from_utf8_lossy(&bytes).into_owned())
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

/// Drop the fields both sides sample from the host at answer time (#6358).
fn without_host_sampled(mut body: serde_json::Value, fields: &[&str]) -> serde_json::Value {
    if let Some(obj) = body.as_object_mut() {
        for field in fields {
            obj.remove(*field);
        }
    }
    body
}

/// Why: the flat list is the response eleven crates read today, and
/// `?details=true` and `?format=tree` are three different arms of one handler —
/// a socket that served only the flat arm would look correct until a consumer
/// asked for details.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
async fn indexes_list_over_the_socket_matches_the_http_body() {
    let (_state, http, rpc) = fixture().await;

    let over_socket = rpc_ok(&rpc, reads::METHOD_INDEXES_LIST, serde_json::json!({})).await;
    assert_eq!(over_socket, http_json(&http, "/indexes").await);

    let over_socket = rpc_ok(
        &rpc,
        reads::METHOD_INDEXES_LIST,
        serde_json::json!({ "details": true }),
    )
    .await;
    assert_eq!(
        over_socket,
        http_json(&http, "/indexes?details=true").await,
        "the details arm must carry the same size_bytes and root_path"
    );

    let over_socket = rpc_ok(
        &rpc,
        reads::METHOD_INDEXES_LIST,
        serde_json::json!({ "format": "tree" }),
    )
    .await;
    assert_eq!(over_socket, http_json(&http, "/indexes?format=tree").await);
}

/// Why: `status` is the endpoint the MCP layer reads to decide whether an index
/// was ever built, so a divergence here is a wrong answer about the code rather
/// than a cosmetic one.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
async fn index_status_over_the_socket_matches_the_http_body() {
    let (_state, http, rpc) = fixture().await;

    let over_socket = rpc_ok(
        &rpc,
        reads::METHOD_INDEX_STATUS,
        serde_json::json!({ "index_id": INDEX }),
    )
    .await;
    let over_http = http_json(&http, &format!("/indexes/{INDEX}/status")).await;
    assert_eq!(over_socket["chunk_count"], serde_json::json!(3));
    assert_eq!(over_socket, over_http);
}

/// Why: the hygiene config is a typed view shared with the PATCH echo-back, so
/// this pins that the socket serialises that struct rather than a hand-built
/// object with the same field names.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
async fn index_config_over_the_socket_matches_the_http_body() {
    let (_state, http, rpc) = fixture().await;

    let over_socket = rpc_ok(
        &rpc,
        reads::METHOD_INDEX_CONFIG_GET,
        serde_json::json!({ "index_id": INDEX }),
    )
    .await;
    let over_http = http_json(&http, &format!("/indexes/{INDEX}/config")).await;
    assert_eq!(over_socket["kg"], serde_json::json!(true));
    assert_eq!(over_socket, over_http);
}

/// Why: both limits are process-global `AtomicU64` cells, so this is the one
/// read where a divergence could only come from a second projection of them.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
async fn config_over_the_socket_matches_the_http_body() {
    let (_state, http, rpc) = fixture().await;

    let over_socket = rpc_ok(&rpc, reads::METHOD_CONFIG_GET, serde_json::json!(null)).await;
    assert_eq!(over_socket, http_json(&http, "/config").await);
}

/// Why: the two pagination modes order rows differently on purpose, so a socket
/// that silently took the other mode would lose or duplicate rows for a bulk
/// consumer. Both modes are asserted, and the `limit` clamp with them.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
async fn chunks_over_the_socket_matches_the_http_body() {
    let (_state, http, rpc) = fixture().await;

    // Offset mode.
    let over_socket = rpc_ok(
        &rpc,
        reads::METHOD_CHUNKS_LIST,
        serde_json::json!({ "index_id": INDEX, "offset": 1, "limit": 2 }),
    )
    .await;
    let over_http = http_json(&http, &format!("/indexes/{INDEX}/chunks?offset=1&limit=2")).await;
    assert_eq!(over_socket["total"], serde_json::json!(3));
    assert!(
        over_socket["next_cursor"].is_null(),
        "offset mode never surfaces a cursor"
    );
    assert_eq!(over_socket, over_http);

    // Cursor mode — opted into by sending `after` at all, empty included.
    let over_socket = rpc_ok(
        &rpc,
        reads::METHOD_CHUNKS_LIST,
        serde_json::json!({ "index_id": INDEX, "after": "", "limit": 2 }),
    )
    .await;
    let over_http = http_json(&http, &format!("/indexes/{INDEX}/chunks?after=&limit=2")).await;
    assert_eq!(over_socket, over_http);

    // The clamp is echoed post-clamp so a client can detect it; both transports
    // must clamp to the same number.
    let over_socket = rpc_ok(
        &rpc,
        reads::METHOD_CHUNKS_LIST,
        serde_json::json!({ "index_id": INDEX, "limit": 9_999 }),
    )
    .await;
    assert_eq!(over_socket["limit"], serde_json::json!(1_000));
    assert_eq!(
        over_socket,
        http_json(&http, &format!("/indexes/{INDEX}/chunks?limit=9999")).await
    );
}

/// Why: the graph export applies three filters and drops edges whose endpoints
/// the type filter removed — the subtlest body on this surface, and the one a
/// second implementation would most plausibly get wrong.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
async fn graph_over_the_socket_matches_the_http_body() {
    let (_state, http, rpc) = fixture().await;
    const HOST_SAMPLED: [&str; 1] = ["generated_at"];

    let over_socket = rpc_ok(
        &rpc,
        reads::METHOD_GRAPH_GET,
        serde_json::json!({ "index_id": INDEX }),
    )
    .await;
    let over_http = http_json(&http, &format!("/indexes/{INDEX}/graph")).await;
    assert_eq!(
        without_host_sampled(over_socket, &HOST_SAMPLED),
        without_host_sampled(over_http, &HOST_SAMPLED)
    );

    // With every filter engaged, so the filter parsing is on both paths.
    let params = serde_json::json!({
        "index_id": INDEX, "types": "Symbol", "edge_types": "CallsFunction", "min_weight": 0.5,
    });
    let over_socket = rpc_ok(&rpc, reads::METHOD_GRAPH_GET, params).await;
    let over_http = http_json(
        &http,
        &format!("/indexes/{INDEX}/graph?types=Symbol&edge_types=CallsFunction&min_weight=0.5"),
    )
    .await;
    assert_eq!(
        without_host_sampled(over_socket, &HOST_SAMPLED),
        without_host_sampled(over_http, &HOST_SAMPLED)
    );
}

/// Why: `graph/stats` is the KG-health signal dashboards poll, and it carries
/// `unknown_edge_tags_dropped` — a skew indicator that reading as zero on one
/// transport would hide.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
async fn graph_stats_over_the_socket_matches_the_http_body() {
    let (_state, http, rpc) = fixture().await;

    let over_socket = rpc_ok(
        &rpc,
        reads::METHOD_GRAPH_STATS,
        serde_json::json!({ "index_id": INDEX }),
    )
    .await;
    assert_eq!(
        over_socket,
        http_json(&http, &format!("/indexes/{INDEX}/graph/stats")).await
    );
}

/// Why: the neighbours BFS parses a direction word and an edge-kind vocabulary,
/// and the clamp on `max_hops` is echoed in the body — three chances for the
/// two transports to accept different inputs.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
async fn graph_neighbors_over_the_socket_matches_the_http_body() {
    let (_state, http, rpc) = fixture().await;

    let over_socket = rpc_ok(
        &rpc,
        reads::METHOD_GRAPH_NEIGHBORS,
        serde_json::json!({ "index_id": INDEX, "node": "authenticate", "direction": "out", "max_hops": 9 }),
    )
    .await;
    let over_http = http_json(
        &http,
        &format!("/indexes/{INDEX}/graph/neighbors?node=authenticate&direction=out&max_hops=9"),
    )
    .await;
    assert_eq!(
        over_socket["max_hops"],
        serde_json::json!(4),
        "max_hops is clamped to 1..=4 and the clamped value is echoed"
    );
    assert_eq!(over_socket, over_http);
}

/// Drop the report's `# Generated: <rfc3339>` header line.
///
/// The call-chain renderer stamps the host clock at render time, so it is this
/// report's host-sampled field — the same exclusion `graph`'s `generated_at`
/// gets, in a text body rather than a JSON one. Every other line is derived
/// from the graph and the corpus and must match.
fn without_generated_line(report: &str) -> String {
    report
        .lines()
        .filter(|line| !line.starts_with("# Generated:"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Why: `call_chain` is the one read whose HTTP body is `text/plain`, so the
/// socket must carry the rendered report itself rather than a JSON wrapper
/// around it. This compares the frame's string against the HTTP text.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
async fn call_chain_over_the_socket_matches_the_http_body() {
    let (_state, http, rpc) = fixture().await;

    let over_socket = rpc_ok(
        &rpc,
        reads::METHOD_CALL_CHAIN,
        serde_json::json!({ "index_id": INDEX, "entry_point": "authenticate" }),
    )
    .await;
    let (status, over_http) = http_raw(
        &http,
        &format!("/indexes/{INDEX}/call_chain?entry_point=authenticate"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "the entry point must resolve");

    let over_socket = over_socket.as_str().expect("the frame carries a string");
    assert!(
        over_socket.contains("[ENTRY]"),
        "the socket must carry the rendered report, not an empty string: {over_socket}"
    );
    assert_eq!(
        without_generated_line(over_socket),
        without_generated_line(&over_http)
    );
}

/// Why: an id no store holds is the most-hit refusal on this surface, and the
/// retire slice's consumers will branch on the code alone. Every index-scoped
/// read must answer the SAME code for it — one that answered `internal_error`
/// would send a caller looking for a daemon fault.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
async fn an_unknown_index_reports_not_found_on_every_index_scoped_read() {
    let (_state, _http, rpc) = fixture().await;

    // Each entry carries the params that method needs BESIDES `index_id`, so a
    // refusal here is the index lookup rather than a decode failure.
    for (method, params) in [
        (
            reads::METHOD_INDEX_STATUS,
            serde_json::json!({ "index_id": MISSING }),
        ),
        (
            reads::METHOD_INDEX_CONFIG_GET,
            serde_json::json!({ "index_id": MISSING }),
        ),
        (
            reads::METHOD_CHUNKS_LIST,
            serde_json::json!({ "index_id": MISSING }),
        ),
        (
            reads::METHOD_GRAPH_GET,
            serde_json::json!({ "index_id": MISSING }),
        ),
        (
            reads::METHOD_GRAPH_STATS,
            serde_json::json!({ "index_id": MISSING }),
        ),
        (
            reads::METHOD_GRAPH_NEIGHBORS,
            serde_json::json!({ "index_id": MISSING, "node": "x" }),
        ),
        (
            reads::METHOD_CALL_CHAIN,
            serde_json::json!({ "index_id": MISSING, "entry_point": "x" }),
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

/// Why: the retryable half of the 503 split had no end-to-end case — slice 2
/// proved `CODE_UNAVAILABLE_PERMANENT` through `kg_unavailable` and left
/// [`CODE_UNAVAILABLE`] proved only by the unit table in `error_tests.rs`. That
/// is the half a consumer acts on: a cold-parked index is registered, built,
/// and one search away from serving, so a caller that read it as permanent
/// would give up on an index that was never lost. This drives the real
/// cold-store state through both transports.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
async fn a_cold_parked_index_reports_the_retryable_unavailable_code() {
    let (_state, http, rpc) = fixture().await;

    let (status, body) = http_raw(&http, &format!("/indexes/{COLD_INDEX}/status")).await;
    assert_eq!(
        status,
        StatusCode::SERVICE_UNAVAILABLE,
        "fixture precondition: HTTP refuses a non-resident index with 503"
    );
    assert!(
        body.contains("\"retryable\":true"),
        "fixture precondition: the refusal says waiting can succeed: {body}"
    );

    let err = rpc_err(
        &rpc,
        reads::METHOD_INDEX_STATUS,
        serde_json::json!({ "index_id": COLD_INDEX }),
    )
    .await;
    assert_eq!(
        err.code, CODE_UNAVAILABLE,
        "an index a search would reload must not read as permanently gone: {err}"
    );
    assert_ne!(
        err.code, CODE_NOT_FOUND,
        "and it must not read as an index that never existed (#4715): {err}"
    );
    assert!(
        err.message.contains("index_not_resident"),
        "the refusal must keep the class name HTTP reports: {err}"
    );
}

/// Why: `kg_unavailable` on a `skip_kg` index never clears by waiting — only a
/// config change does. `RpcError` carries no `retryable` field, so the
/// permanent code IS the contract, and a caller that read this as the retryable
/// 503 would poll forever.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
async fn call_chain_over_the_socket_reports_a_kg_disabled_index_as_unavailable() {
    let (_state, http, rpc) = fixture().await;

    let (status, _body) = http_raw(
        &http,
        &format!("/indexes/{KG_OFF_INDEX}/call_chain?entry_point=authenticate"),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::SERVICE_UNAVAILABLE,
        "fixture precondition: HTTP refuses a skip_kg index with 503"
    );

    let err = rpc_err(
        &rpc,
        reads::METHOD_CALL_CHAIN,
        serde_json::json!({ "index_id": KG_OFF_INDEX, "entry_point": "authenticate" }),
    )
    .await;
    assert_eq!(
        err.code, CODE_UNAVAILABLE_PERMANENT,
        "a lane the index was built without never clears by retrying: {err}"
    );
    assert!(
        err.message.contains("kg_unavailable"),
        "the refusal must keep the class name HTTP reports: {err}"
    );
}

/// Why: a param the caller got wrong is the caller's fault on both transports.
/// HTTP answers 400; the socket must answer `invalid_params` rather than
/// folding it into the internal-error bucket, which would tell a client to file
/// a bug instead of fixing its request.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
async fn a_bad_direction_reports_invalid_params_on_both_transports() {
    let (_state, http, rpc) = fixture().await;

    let (status, _body) = http_raw(
        &http,
        &format!("/indexes/{INDEX}/graph/neighbors?node=authenticate&direction=sideways"),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let err = rpc_err(
        &rpc,
        reads::METHOD_GRAPH_NEIGHBORS,
        serde_json::json!({ "index_id": INDEX, "node": "authenticate", "direction": "sideways" }),
    )
    .await;
    assert_eq!(err.code, CODE_INVALID_PARAMS, "{err}");
    assert!(err.message.contains("sideways"), "{err}");
}

/// Why: `index_id` is the path segment on HTTP, where it cannot be omitted. On
/// the socket it is a field, so it CAN be — and a method that silently read a
/// missing id as the empty string would answer "unknown index: " instead of
/// telling the caller what it left out.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
async fn an_index_scoped_method_without_an_index_id_reports_invalid_params() {
    let (_state, _http, rpc) = fixture().await;

    let err = rpc_err(&rpc, reads::METHOD_INDEX_STATUS, serde_json::json!({})).await;
    assert_eq!(err.code, CODE_INVALID_PARAMS, "{err}");
    assert!(
        err.message.contains("index_id"),
        "the refusal must name the missing field: {err}"
    );
}
