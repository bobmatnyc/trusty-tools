//! Tests for the contributed-graph ingest + neighbors endpoints (ADR-0009).
//!
//! Covers: ingest → traversal round-trip, replace-per-producer on re-ingest,
//! survival across a derived-graph rebuild (the reindex seam), and the error
//! contract (404 unknown index, 400 validation, 503 no corpus).

use super::contrib_graph::{
    graph_neighbors_handler, ingest_graph_handler, IngestGraphRequest, NeighborsParams,
};
use super::state::SearchAppState;
use crate::core::corpus::contrib::{ContribEdge, ContribNode};
use crate::core::corpus::CorpusStore;
use crate::core::indexer::CodeIndexer;
use crate::core::registry::{IndexHandle, IndexId, IndexRegistry};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use std::sync::Arc;
use tokio::sync::RwLock;

fn node(id: &str, kind: &str) -> ContribNode {
    ContribNode {
        id: id.into(),
        kind: kind.into(),
    }
}

fn edge(from: &str, to: &str, kind: &str) -> ContribEdge {
    ContribEdge {
        from: from.into(),
        to: to.into(),
        kind: Some(kind.into()),
        tag: None,
        provenance: vec!["a.sql".into()],
        linked_server: None,
    }
}

fn request(producer: &str) -> IngestGraphRequest {
    IngestGraphRequest {
        schema: Some("navigatsql/kggraph@1".into()),
        producer: producer.into(),
        producer_version: Some("0.1.0".into()),
        git_sha: Some("abc123".into()),
        nodes: vec![
            node("m.Save", "csharp_method"),
            node("dbo.usp_x", "proc"),
            node("dbo.orders", "table"),
        ],
        edges: vec![
            edge("m.Save", "dbo.usp_x", "calls_proc"),
            edge("dbo.usp_x", "dbo.orders", "writes"),
        ],
    }
}

/// Add one index backed by a real temp redb corpus to `registry`, handing back
/// the corpus itself so a test can plant a fault in it (#5505).
fn register_index_with_corpus(
    registry: &IndexRegistry,
    id: &str,
) -> (tempfile::TempDir, Arc<CorpusStore>) {
    let dir = tempfile::tempdir().expect("tempdir");
    let corpus = Arc::new(CorpusStore::open(&dir.path().join("corpus.redb")).expect("open corpus"));
    let mut indexer = CodeIndexer::new(id, dir.path().to_str().expect("utf8 path"));
    indexer.set_corpus_store(Arc::clone(&corpus));
    registry.register(IndexHandle::bare(
        IndexId::new(id),
        Arc::new(RwLock::new(indexer)),
        dir.path().into(),
    ));
    (dir, corpus)
}

/// Registry with one index backed by a real temp redb corpus, plus its corpus.
fn state_and_corpus(id: &str) -> (tempfile::TempDir, Arc<CorpusStore>, SearchAppState) {
    let registry = IndexRegistry::new();
    let (dir, corpus) = register_index_with_corpus(&registry, id);
    (dir, corpus, SearchAppState::new(registry))
}

/// Registry with one index backed by a real temp redb corpus.
fn state_with_corpus(id: &str) -> (tempfile::TempDir, Arc<SearchAppState>) {
    let (dir, _corpus, state) = state_and_corpus(id);
    (dir, Arc::new(state))
}

fn neighbors_params(
    node: &str,
    direction: Option<&str>,
    edge_kinds: Option<&str>,
    max_hops: Option<usize>,
) -> NeighborsParams {
    NeighborsParams {
        node: node.into(),
        direction: direction.map(Into::into),
        edge_kinds: edge_kinds.map(Into::into),
        max_hops,
    }
}

#[tokio::test]
async fn ingest_then_neighbors_round_trip() {
    let (_dir, state) = state_with_corpus("contrib-rt");

    let Json(resp) = ingest_graph_handler(
        State(Arc::clone(&state)),
        Path("contrib-rt".into()),
        Json(request("navigatsql")),
    )
    .await
    .expect("ingest ok");
    assert!(!resp.replaced);
    assert_eq!(resp.nodes_received, 3);
    assert_eq!(resp.edges_received, 2);
    assert_eq!(resp.graph_nodes, 3);
    assert_eq!(resp.graph_edges, 2);
    assert_eq!(resp.unknown_edge_tags_dropped, 0);

    // "What writes dbo.orders?" — inbound, 2 hops reaches the C# method.
    let Json(body) = graph_neighbors_handler(
        State(Arc::clone(&state)),
        Path("contrib-rt".into()),
        Query(neighbors_params("dbo.orders", Some("in"), None, Some(2))),
    )
    .await
    .expect("neighbors ok");
    assert_eq!(body["count"], 2);
    assert_eq!(body["node_kind"], "table");
    let symbols: Vec<&str> = body["neighbors"]
        .as_array()
        .expect("array")
        .iter()
        .map(|n| n["symbol"].as_str().expect("symbol"))
        .collect();
    assert!(symbols.contains(&"dbo.usp_x"));
    assert!(symbols.contains(&"m.Save"));

    // Kind filter: only Writes edges out of the proc.
    let Json(body) = graph_neighbors_handler(
        State(Arc::clone(&state)),
        Path("contrib-rt".into()),
        Query(neighbors_params(
            "dbo.usp_x",
            Some("out"),
            Some("writes"),
            Some(1),
        )),
    )
    .await
    .expect("neighbors ok");
    assert_eq!(body["count"], 1);
    assert_eq!(body["neighbors"][0]["symbol"], "dbo.orders");
    assert_eq!(body["neighbors"][0]["node_kind"], "table");
    assert_eq!(body["neighbors"][0]["edge"], "Writes");
}

#[tokio::test]
async fn ingest_twice_replaces_per_producer() {
    let (_dir, state) = state_with_corpus("contrib-replace");

    let Json(first) = ingest_graph_handler(
        State(Arc::clone(&state)),
        Path("contrib-replace".into()),
        Json(request("navigatsql")),
    )
    .await
    .expect("ingest v1 ok");
    assert!(!first.replaced);

    // v2 drops the method node/edge: graph must shrink, not accrete.
    let v2 = IngestGraphRequest {
        nodes: vec![node("dbo.usp_x", "proc"), node("dbo.orders", "table")],
        edges: vec![edge("dbo.usp_x", "dbo.orders", "writes")],
        ..request("navigatsql")
    };
    let Json(second) = ingest_graph_handler(
        State(Arc::clone(&state)),
        Path("contrib-replace".into()),
        Json(v2),
    )
    .await
    .expect("ingest v2 ok");
    assert!(second.replaced);
    assert_eq!(second.graph_nodes, 2, "v1's m.Save must be gone");
    assert_eq!(second.graph_edges, 1);
}

#[tokio::test]
async fn ingest_survives_derived_rebuild() {
    let (_dir, state) = state_with_corpus("contrib-rebuild");
    let _ = ingest_graph_handler(
        State(Arc::clone(&state)),
        Path("contrib-rebuild".into()),
        Json(request("navigatsql")),
    )
    .await
    .expect("ingest ok");

    // Simulate the reindex seam: a fresh derived rebuild must re-merge the
    // stored contribution rather than evicting it from the serving graph.
    let handle = state
        .registry
        .get(&IndexId::new("contrib-rebuild"))
        .expect("handle");
    {
        let indexer = handle.indexer.read().await;
        indexer.rebuild_symbol_graph_now().await;
    }

    let Json(body) = graph_neighbors_handler(
        State(Arc::clone(&state)),
        Path("contrib-rebuild".into()),
        Query(neighbors_params("dbo.orders", Some("in"), None, Some(2))),
    )
    .await
    .expect("neighbors ok");
    assert_eq!(body["count"], 2, "contribution must survive a rebuild");
}

#[tokio::test]
async fn ingest_unknown_index_404() {
    let (_dir, state) = state_with_corpus("contrib-404");
    let err = ingest_graph_handler(
        State(state),
        Path("nope".into()),
        Json(request("navigatsql")),
    )
    .await
    .expect_err("must fail");
    assert_eq!(err.0, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn ingest_empty_producer_400() {
    let (_dir, state) = state_with_corpus("contrib-400");
    let err = ingest_graph_handler(
        State(state),
        Path("contrib-400".into()),
        Json(request("  ")),
    )
    .await
    .expect_err("must fail");
    assert_eq!(err.0, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn ingest_without_corpus_503() {
    // Bare indexer with no durable corpus store attached.
    let registry = IndexRegistry::new();
    registry.register(IndexHandle::bare(
        IndexId::new("no-corpus"),
        Arc::new(RwLock::new(CodeIndexer::new("no-corpus", "/tmp/no-corpus"))),
        "/tmp/no-corpus".into(),
    ));
    let state = Arc::new(SearchAppState::new(registry));
    let err = ingest_graph_handler(
        State(state),
        Path("no-corpus".into()),
        Json(request("navigatsql")),
    )
    .await
    .expect_err("must fail");
    assert_eq!(err.0, StatusCode::SERVICE_UNAVAILABLE);
}

/// Why: #5505 — the ingest persisted the contribution, the merge into the
/// serving graph failed, and the endpoint still answered `200 replaced: true`
/// with graph totals that excluded the contribution. The caller was told an
/// ingest succeeded that no query could see.
/// Test: this test. Fails on the pre-fix commit with the `Ok` arm's panic
/// (`200`, `graph_nodes = 0`).
#[tokio::test(flavor = "multi_thread")]
async fn ingest_reports_503_when_the_contributed_overlay_cannot_be_merged() {
    let (_dir, corpus, state) = state_and_corpus("contrib-merge-fail");
    let state = Arc::new(state);
    // A row from an earlier producer that no longer deserializes: this
    // ingest's own write still succeeds, the load that follows cannot.
    crate::core::corpus::test_support::corrupt_contrib_row(&corpus, "stale-producer")
        .expect("plant corrupt contrib row");

    let (code, Json(body)) = match ingest_graph_handler(
        State(Arc::clone(&state)),
        Path("contrib-merge-fail".into()),
        Json(request("navigatsql")),
    )
    .await
    {
        Err(e) => e,
        Ok(Json(resp)) => panic!(
            "#5505: ingest reported success (replaced={}, graph_nodes={}, graph_edges={}) \
             while the contribution is absent from the serving graph",
            resp.replaced, resp.graph_nodes, resp.graph_edges
        ),
    };
    assert_eq!(code, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["error"], "contrib_not_merged");
    assert_eq!(body["index_id"], "contrib-merge-fail");
    assert_eq!(body["retryable"], true);
    assert_eq!(
        body["persisted"], true,
        "the contribution IS durable — the caller must not be told to treat it as lost"
    );

    // Corroboration: the verdict matches what a query can actually see.
    let Json(neighbors) = graph_neighbors_handler(
        State(state),
        Path("contrib-merge-fail".into()),
        Query(neighbors_params("dbo.orders", Some("in"), None, Some(2))),
    )
    .await
    .expect("neighbors ok");
    assert_eq!(
        neighbors["count"], 0,
        "the contribution really is unqueryable — a 200 would have been a lie"
    );
}

/// Why: #5505's lesser arm at the endpoint — a derived-KG persist failure does
/// NOT make the contribution unqueryable, so the ingest still succeeds. What
/// was wrong is that the caller never heard about it.
/// Test: this test.
#[tokio::test(flavor = "multi_thread")]
async fn ingest_succeeds_but_flags_a_derived_kg_persist_failure() {
    let (_dir, corpus, state) = state_and_corpus("contrib-persist-degraded");
    crate::core::corpus::test_support::break_kg_nodes_table(&corpus).expect("break kg_nodes");

    let Json(resp) = ingest_graph_handler(
        State(Arc::new(state)),
        Path("contrib-persist-degraded".into()),
        Json(request("navigatsql")),
    )
    .await
    .expect("the contribution is queryable — this is not a 503");

    assert_eq!(resp.graph_nodes, 3, "the merge still ran");
    let reason = resp
        .derived_graph_persist_degraded
        .expect("a silent persist failure is what #5505 is about");
    assert!(
        reason.contains("kg persist failed"),
        "reason must name the failure, got {reason:?}"
    );
}

/// Why: the response-contract change is on an HTTP surface, so it owes
/// evidence over the wire — routing, middleware, status line and serialized
/// body — not only at the handler function (#5505).
/// What: serves the real daemon router on a loopback socket and drives
/// `POST /indexes/{id}/graph` with a real HTTP client against two indexes: one
/// healthy, one whose contributed overlay cannot be loaded.
/// Test: this test.
#[tokio::test(flavor = "multi_thread")]
async fn ingest_route_answers_503_over_http_when_the_merge_fails() {
    let registry = IndexRegistry::new();
    let (_ok_dir, _ok_corpus) = register_index_with_corpus(&registry, "live-ok");
    let (_bad_dir, bad_corpus) = register_index_with_corpus(&registry, "live-broken");
    crate::core::corpus::test_support::corrupt_contrib_row(&bad_corpus, "stale-producer")
        .expect("plant corrupt contrib row");

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback");
    let addr = listener.local_addr().expect("local addr");
    let app = super::build_router(SearchAppState::new(registry));
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    let payload = serde_json::json!({
        "schema": "navigatsql/kggraph@1",
        "producer": "navigatsql",
        "nodes": [
            { "id": "dbo.usp_x", "kind": "proc" },
            { "id": "dbo.orders", "kind": "table" },
        ],
        "edges": [{ "from": "dbo.usp_x", "to": "dbo.orders", "kind": "writes" }],
    });
    let client = reqwest::Client::new();
    let post = |id: &str| {
        client
            .post(format!("http://{addr}/indexes/{id}/graph"))
            .json(&payload)
            .send()
    };

    // Healthy index: unchanged contract — 200, totals that include the
    // contribution, and no degraded field.
    let resp = post("live-ok").await.expect("healthy ingest response");
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert_eq!(body["graph_nodes"], 2);
    assert_eq!(body["graph_edges"], 1);
    assert!(body.get("derived_graph_persist_degraded").is_none());

    // Unmergeable index: 503, not a 200 whose totals exclude the contribution.
    let resp = post("live-broken").await.expect("degraded ingest response");
    assert_eq!(
        resp.status(),
        503,
        "POST /indexes/{{id}}/graph must not answer 200 for an unmerged contribution"
    );
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert_eq!(body["error"], "contrib_not_merged");
    assert_eq!(body["index_id"], "live-broken");
    assert_eq!(body["retryable"], true);
    assert_eq!(body["persisted"], true);
}

#[tokio::test]
async fn neighbors_rejects_unknown_edge_kind() {
    let (_dir, state) = state_with_corpus("contrib-badkind");
    let err = graph_neighbors_handler(
        State(state),
        Path("contrib-badkind".into()),
        Query(neighbors_params("x", None, Some("bogus_kind"), None)),
    )
    .await
    .expect_err("must fail");
    assert_eq!(err.0, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn neighbors_unknown_node_is_empty() {
    let (_dir, state) = state_with_corpus("contrib-empty");
    let Json(body) = graph_neighbors_handler(
        State(state),
        Path("contrib-empty".into()),
        Query(neighbors_params("does.not.exist", None, None, None)),
    )
    .await
    .expect("ok");
    assert_eq!(body["count"], 0);
}
