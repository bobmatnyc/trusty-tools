//! Tests for health, graph, global search, and routing-mode handlers.
use super::routing::RoutingMode;
use super::status::GraphQueryParams;
use super::*;
use crate::core::registry::IndexId;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;

/// Why: `/health` is consumed by external probes (open-mpm,
/// `ensure_daemon_running`) — the contract `{ status, version, indexes,
/// uptime_secs }` must remain stable.
/// What: Builds an AppState with N registered indexes and asserts the
/// HealthResponse JSON shape and counts.
/// Test: covers issue #34's acceptance (indexes counter + uptime_secs).
#[tokio::test]
async fn health_handler_reports_indexes_and_uptime() {
    use crate::core::{
        indexer::CodeIndexer,
        registry::{IndexHandle, IndexId, IndexRegistry},
    };
    use std::sync::Arc;
    use tokio::sync::RwLock;

    let registry = IndexRegistry::new();
    let id = IndexId::new("health-test");
    registry.register(IndexHandle::bare(
        id.clone(),
        Arc::new(RwLock::new(CodeIndexer::new(
            "health-test",
            "/tmp/health-test",
        ))),
        "/tmp/health-test".into(),
    ));
    let state = Arc::new(SearchAppState::new(registry));
    let Json(resp) = health_handler(State(state)).await;
    assert_eq!(resp.status, "ok");
    assert_eq!(resp.version, env!("CARGO_PKG_VERSION"));
    assert_eq!(resp.indexes, 1);
    // uptime_secs is u64 — always >= 0 by type; just exercise the path.
    let _ = resp.uptime_secs;
    // No embedder attached in this test. With the deferred-init flow,
    // a fresh `SearchAppState::new()` reports "initializing" (the
    // background task hasn't installed an embedder yet) rather than
    // "unavailable". "unavailable" is reserved for the post-failure
    // case where the init task explicitly errored.
    assert_eq!(resp.embedder, "initializing");
}

/// Issue #128 — `GET /indexes/{id}/graph` exports the full SymbolGraph.
/// With a registered index holding inter-calling functions, the response
/// must carry node/edge lists, a `stats` block, a `generated_at` stamp,
/// and a 1-hour `Cache-Control` header.
#[tokio::test]
async fn graph_handler_exports_nodes_and_edges() {
    use crate::core::{
        indexer::CodeIndexer,
        registry::{IndexHandle, IndexId, IndexRegistry},
    };
    use std::sync::Arc;
    use tokio::sync::RwLock;

    let registry = IndexRegistry::new();
    let id = IndexId::new("graph-test");
    let indexer = CodeIndexer::new("graph-test", "/tmp/graph-test");
    // Two functions where `caller` calls `callee` — yields one node per
    // function and one CallsFunction edge.
    indexer
        .index_file(
            "graph-test/lib.rs",
            "fn callee() {}\nfn caller() { callee(); }\n",
        )
        .await
        .expect("index_file ok");
    registry.register(IndexHandle::bare(
        id.clone(),
        Arc::new(RwLock::new(indexer)),
        "/tmp/graph-test".into(),
    ));
    let state = Arc::new(SearchAppState::new(registry));

    let response = graph_handler(
        State(state),
        Path("graph-test".to_string()),
        Query(GraphQueryParams::default()),
    )
    .await
    .expect("handler ok");

    // 1-hour cache header is present.
    assert_eq!(
        response
            .headers()
            .get(axum::http::header::CACHE_CONTROL)
            .and_then(|v| v.to_str().ok()),
        Some("max-age=3600"),
    );

    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body bytes");
    let value: serde_json::Value = serde_json::from_slice(&bytes).expect("json body");

    let nodes = value["nodes"].as_array().expect("nodes array");
    assert_eq!(nodes.len(), 2, "two function symbols expected");
    for node in nodes {
        assert_eq!(node["type"].as_str(), Some("Symbol"));
        assert!(node["id"].is_string());
        assert!(node["label"].is_string());
        assert!(node["metadata"]["file"].is_string());
    }

    let edges = value["edges"].as_array().expect("edges array");
    assert_eq!(edges.len(), 1, "one CallsFunction edge expected");
    assert_eq!(edges[0]["source"].as_str(), Some("caller"));
    assert_eq!(edges[0]["target"].as_str(), Some("callee"));
    assert_eq!(edges[0]["type"].as_str(), Some("CallsFunction"));
    assert!(edges[0]["weight"].as_f64().is_some());

    assert_eq!(value["stats"]["node_count"].as_u64(), Some(2));
    assert_eq!(value["stats"]["edge_count"].as_u64(), Some(1));
    assert!(value["generated_at"].is_string());
}

/// Issue #128 — unknown index id returns 404 from `graph_handler`.
#[tokio::test]
async fn graph_handler_unknown_index_returns_404() {
    use crate::core::registry::IndexRegistry;
    let state = Arc::new(SearchAppState::new(IndexRegistry::new()));
    let err = graph_handler(
        State(state),
        Path("does-not-exist".to_string()),
        Query(GraphQueryParams::default()),
    )
    .await
    .expect_err("missing index must 404");
    assert_eq!(err, StatusCode::NOT_FOUND);
}

/// Issue #128 — `edge_types` filter drops edges of other kinds.
#[tokio::test]
async fn graph_handler_filters_by_edge_type() {
    use crate::core::{
        indexer::CodeIndexer,
        registry::{IndexHandle, IndexId, IndexRegistry},
    };
    use std::sync::Arc;
    use tokio::sync::RwLock;

    let registry = IndexRegistry::new();
    let id = IndexId::new("graph-filter");
    let indexer = CodeIndexer::new("graph-filter", "/tmp/graph-filter");
    indexer
        .index_file(
            "graph-filter/lib.rs",
            "fn callee() {}\nfn caller() { callee(); }\n",
        )
        .await
        .expect("index_file ok");
    registry.register(IndexHandle::bare(
        id.clone(),
        Arc::new(RwLock::new(indexer)),
        "/tmp/graph-filter".into(),
    ));
    let state = Arc::new(SearchAppState::new(registry));

    // Filter to Implements only — the lone CallsFunction edge must drop.
    let response = graph_handler(
        State(state),
        Path("graph-filter".to_string()),
        Query(GraphQueryParams {
            types: None,
            edge_types: Some("Implements".to_string()),
            min_weight: None,
        }),
    )
    .await
    .expect("handler ok");
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body bytes");
    let value: serde_json::Value = serde_json::from_slice(&bytes).expect("json body");
    assert!(
        value["edges"].as_array().expect("edges").is_empty(),
        "CallsFunction edge must be filtered out",
    );
    // Nodes are unaffected by an edge-type filter.
    assert_eq!(value["nodes"].as_array().expect("nodes").len(), 2);
}

/// Issue #10 — `POST /search` fan-out: with two registered indexes each
/// holding a single file, the global search must return results tagged
/// with the correct `index_id` and the response must list both indexes
/// as searched. BM25-only path (no embedder) keeps the test hermetic.
#[tokio::test]
async fn global_search_fans_out_and_merges() {
    use crate::core::{
        indexer::CodeIndexer,
        registry::{IndexHandle, IndexId, IndexRegistry},
    };
    use std::sync::Arc;
    use tokio::sync::RwLock;

    let registry = IndexRegistry::new();
    for name in ["proj-a", "proj-b"] {
        let id = IndexId::new(name);
        let indexer = CodeIndexer::new(name, format!("/tmp/{name}"));
        // Seed one file per index with content matching the query "alpha".
        indexer
            .index_file(
                &format!("{name}/lib.rs"),
                &format!("fn alpha_{name}() {{ println!(\"alpha hit\"); }}"),
            )
            .await
            .expect("index_file ok");
        registry.register(IndexHandle::bare(
            id.clone(),
            Arc::new(RwLock::new(indexer)),
            format!("/tmp/{name}").into(),
        ));
    }

    let state = Arc::new(SearchAppState::new(registry));
    let Json(value) = global_search_handler(
        State(state),
        Json(GlobalSearchRequest {
            query: "alpha".into(),
            top_k: 10,
            full_content: false,
            indexes: None,
            routing: None,
            routing_n: None,
            routing_threshold: None,
        }),
    )
    .await
    .expect("handler ok");

    let total = value["total_indexes"].as_u64().expect("total_indexes");
    assert_eq!(total, 2, "both indexes counted");

    let searched: Vec<String> = value["indexes_searched"]
        .as_array()
        .expect("indexes_searched array")
        .iter()
        .filter_map(|v| v.as_str().map(str::to_owned))
        .collect();
    assert_eq!(searched.len(), 2);
    assert!(searched.contains(&"proj-a".to_string()));
    assert!(searched.contains(&"proj-b".to_string()));

    let results = value["results"].as_array().expect("results array");
    assert!(!results.is_empty(), "expected at least one hit");
    // Every result must carry an index_id tagged with one of the two
    // registered indexes.
    let mut from_a = false;
    let mut from_b = false;
    for r in results {
        let idx = r["index_id"]
            .as_str()
            .expect("each result must be tagged with index_id");
        assert!(
            idx == "proj-a" || idx == "proj-b",
            "unexpected index_id: {idx}"
        );
        from_a |= idx == "proj-a";
        from_b |= idx == "proj-b";
    }
    // Both indexes share the same query term "alpha", so RRF should
    // surface at least one hit from each.
    assert!(from_a, "expected a result tagged with proj-a");
    assert!(from_b, "expected a result tagged with proj-b");
}

/// Issue #10 — `POST /search` with no indexes registered must return an
/// empty result set (not 500). This guards the empty-registry edge case
/// the fan-out path checks before spawning per-index futures.
#[tokio::test]
async fn global_search_empty_registry_returns_empty_results() {
    use crate::core::registry::IndexRegistry;
    let state = Arc::new(SearchAppState::new(IndexRegistry::new()));
    let Json(value) = global_search_handler(
        State(state),
        Json(GlobalSearchRequest {
            query: "anything".into(),
            top_k: 5,
            full_content: false,
            indexes: None,
            routing: None,
            routing_n: None,
            routing_threshold: None,
        }),
    )
    .await
    .expect("handler ok");
    assert_eq!(value["total_indexes"].as_u64(), Some(0));
    assert!(value["results"].as_array().unwrap().is_empty());
    assert!(value["indexes_searched"].as_array().unwrap().is_empty());
}

/// Issue #110 — `POST /search` with explicit `indexes: [...]` must only
/// fan out to the named indexes; results from indexes outside the
/// allow-list must not appear, even when they match the query.
#[tokio::test]
async fn global_search_restricts_to_named_indexes() {
    use crate::core::{
        indexer::CodeIndexer,
        registry::{IndexHandle, IndexId, IndexRegistry},
    };
    use std::sync::Arc;
    use tokio::sync::RwLock;

    let registry = IndexRegistry::new();
    for name in ["proj-a", "proj-b", "proj-c"] {
        let id = IndexId::new(name);
        let indexer = CodeIndexer::new(name, format!("/tmp/{name}"));
        indexer
            .index_file(
                &format!("{name}/lib.rs"),
                &format!("fn alpha_{name}() {{ println!(\"alpha hit\"); }}"),
            )
            .await
            .expect("index_file ok");
        registry.register(IndexHandle::bare(
            id.clone(),
            Arc::new(RwLock::new(indexer)),
            format!("/tmp/{name}").into(),
        ));
    }
    let state = Arc::new(SearchAppState::new(registry));
    let Json(value) = global_search_handler(
        State(state),
        Json(GlobalSearchRequest {
            query: "alpha".into(),
            top_k: 10,
            full_content: false,
            indexes: Some(vec!["proj-a".into(), "proj-c".into()]),
            routing: None,
            routing_n: None,
            routing_threshold: None,
        }),
    )
    .await
    .expect("handler ok");

    // total_indexes reflects the *filtered* set we actually fanned out to.
    assert_eq!(value["total_indexes"].as_u64(), Some(2));

    let searched: std::collections::HashSet<String> = value["indexes_searched"]
        .as_array()
        .expect("array")
        .iter()
        .filter_map(|v| v.as_str().map(str::to_owned))
        .collect();
    assert!(searched.contains("proj-a"));
    assert!(searched.contains("proj-c"));
    assert!(!searched.contains("proj-b"), "proj-b must be excluded");

    for r in value["results"].as_array().unwrap() {
        let idx = r["index_id"].as_str().unwrap();
        assert_ne!(idx, "proj-b", "no result may come from excluded index");
    }
}

/// Issue #112: `RoutingMode::All` keeps every index and surfaces the
/// cosine-similarity weight verbatim. Indexes without a weight entry
/// fall back to 1.0.
#[test]
fn routing_mode_all_preserves_every_index_with_weights() {
    let ids = vec![IndexId::new("a"), IndexId::new("b"), IndexId::new("c")];
    let weights: std::collections::HashMap<IndexId, f32> = [
        (IndexId::new("a"), 0.9_f32),
        (IndexId::new("b"), 0.2),
        // "c" deliberately absent → falls back to 1.0
    ]
    .into_iter()
    .collect();

    let (active, map) = RoutingMode::All.apply(&ids, &weights);
    assert_eq!(active.len(), 3, "all routing keeps every index");
    assert!((map.get(&IndexId::new("a")).copied().unwrap() - 0.9).abs() < 1e-6);
    assert!((map.get(&IndexId::new("b")).copied().unwrap() - 0.2).abs() < 1e-6);
    assert!((map.get(&IndexId::new("c")).copied().unwrap() - 1.0).abs() < 1e-6);
}

/// Issue #112: `RoutingMode::TopN` keeps only the N highest-similarity
/// indexes (ranked desc) and zeroes weights to 1.0 — selection has
/// already absorbed relevance.
#[test]
fn routing_mode_top_n_keeps_only_highest_similarity() {
    let ids = vec![IndexId::new("low"), IndexId::new("hi"), IndexId::new("mid")];
    let weights: std::collections::HashMap<IndexId, f32> = [
        (IndexId::new("low"), 0.1_f32),
        (IndexId::new("hi"), 0.95),
        (IndexId::new("mid"), 0.5),
    ]
    .into_iter()
    .collect();

    let (active, map) = RoutingMode::TopN(2).apply(&ids, &weights);
    assert_eq!(active.len(), 2);
    let active_set: std::collections::HashSet<&str> =
        active.iter().map(|id| id.0.as_str()).collect();
    assert!(active_set.contains("hi"));
    assert!(active_set.contains("mid"));
    assert!(!active_set.contains("low"));
    // Selected entries normalised to weight 1.0.
    assert!((map.get(&IndexId::new("hi")).copied().unwrap() - 1.0).abs() < 1e-6);
    assert!((map.get(&IndexId::new("mid")).copied().unwrap() - 1.0).abs() < 1e-6);
    assert!(!map.contains_key(&IndexId::new("low")));
}

/// Issue #112: `RoutingMode::Threshold` drops anything strictly below
/// the threshold and keeps entries at/above it.
#[test]
fn routing_mode_threshold_drops_below_cutoff() {
    let ids = vec![IndexId::new("a"), IndexId::new("b"), IndexId::new("c")];
    let weights: std::collections::HashMap<IndexId, f32> = [
        (IndexId::new("a"), 0.1_f32),
        (IndexId::new("b"), 0.5),
        (IndexId::new("c"), 0.8),
    ]
    .into_iter()
    .collect();

    let (active, map) = RoutingMode::Threshold(0.4).apply(&ids, &weights);
    let active_set: std::collections::HashSet<&str> =
        active.iter().map(|id| id.0.as_str()).collect();
    assert!(!active_set.contains("a"), "0.1 < 0.4 must drop");
    assert!(active_set.contains("b"), "0.5 >= 0.4 must keep");
    assert!(active_set.contains("c"));
    assert!(!map.contains_key(&IndexId::new("a")));
}

/// Indexes missing a weight entry default to neutral 1.0, so threshold
/// routing must not silently drop them — otherwise "no metadata"
/// becomes "no relevance" by accident.
#[test]
fn routing_threshold_keeps_neutral_indexes() {
    let ids = vec![IndexId::new("known"), IndexId::new("missing")];
    let weights: std::collections::HashMap<IndexId, f32> =
        [(IndexId::new("known"), 0.05_f32)].into_iter().collect();

    let (active, _map) = RoutingMode::Threshold(0.5).apply(&ids, &weights);
    let active_set: std::collections::HashSet<&str> =
        active.iter().map(|id| id.0.as_str()).collect();
    assert!(!active_set.contains("known"), "0.05 < 0.5 dropped");
    // Missing entries default to 1.0 → kept.
    assert!(
        active_set.contains("missing"),
        "indexes without a context embedding must use neutral 1.0 weight"
    );
}

/// Verify request → routing-mode resolution: missing or unknown values
/// fall back to `All`; explicit values pick the right strategy and
/// honour their `n` / `threshold` knobs.
#[test]
fn routing_mode_from_request_resolves_strategy() {
    let base = |routing: Option<&str>, n: Option<usize>, t: Option<f32>| -> GlobalSearchRequest {
        GlobalSearchRequest {
            query: "x".into(),
            top_k: 1,
            full_content: false,
            indexes: None,
            routing: routing.map(|s| s.to_string()),
            routing_n: n,
            routing_threshold: t,
        }
    };
    assert!(matches!(
        RoutingMode::from_request(&base(None, None, None)),
        RoutingMode::All
    ));
    assert!(matches!(
        RoutingMode::from_request(&base(Some("garbage"), None, None)),
        RoutingMode::All
    ));
    match RoutingMode::from_request(&base(Some("top_n"), Some(5), None)) {
        RoutingMode::TopN(n) => assert_eq!(n, 5),
        _ => panic!("expected TopN"),
    }
    match RoutingMode::from_request(&base(Some("top_n"), None, None)) {
        RoutingMode::TopN(n) => assert_eq!(n, RoutingMode::DEFAULT_TOP_N),
        _ => panic!("expected TopN default"),
    }
    match RoutingMode::from_request(&base(Some("threshold"), None, Some(0.7))) {
        RoutingMode::Threshold(t) => assert!((t - 0.7).abs() < 1e-6),
        _ => panic!("expected Threshold"),
    }
}
