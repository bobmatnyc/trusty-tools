//! Tests for knowledge graph endpoints: gaps, subjects, all-triples, graph.

use super::super::router;
use super::test_state;
use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use serde_json::{json, Value};
use tower::util::ServiceExt;
use trusty_common::memory_core::palace::PalaceId;
use trusty_common::memory_core::store::kg::Triple;

/// Why: Issue #53 — when the dream cycle has not yet run for a palace,
/// `/api/v1/kg/gaps` must return an empty array (200 OK), not 404 or
/// 500. The cache miss is a meaningful, non-error state.
/// What: Creates a palace, queries `/api/v1/kg/gaps?palace=...`, asserts
/// the response is `200` with body `[]`.
/// Test: this test itself.
#[tokio::test]
async fn kg_gaps_endpoint_returns_empty_when_uncached() {
    let state = test_state();
    let palace = trusty_common::memory_core::Palace {
        id: PalaceId::new("gaps-empty"),
        name: "gaps-empty".to_string(),
        description: None,
        created_at: chrono::Utc::now(),
        data_dir: state.data_root.join("gaps-empty"),
    };
    state
        .registry
        .create_palace(&state.data_root, palace)
        .expect("create palace");

    let app = router().with_state(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/kg/gaps?palace=gaps-empty")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = to_bytes(resp.into_body(), 4096).await.unwrap();
    let v: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v.as_array().expect("array").len(), 0);
}

/// Why: Issue #53 — when the cache *has* been populated (by the dream
/// cycle in production, or by direct seeding here), the endpoint must
/// return each gap with the four wire fields.
/// What: Seeds the registry cache via `set_gaps` directly, then GETs
/// `/api/v1/kg/gaps?palace=...` and asserts the JSON shape.
/// Test: this test itself.
#[tokio::test]
async fn kg_gaps_endpoint_returns_cached_gaps() {
    use trusty_common::memory_core::community::KnowledgeGap;

    let state = test_state();
    let palace = trusty_common::memory_core::Palace {
        id: PalaceId::new("gaps-seed"),
        name: "gaps-seed".to_string(),
        description: None,
        created_at: chrono::Utc::now(),
        data_dir: state.data_root.join("gaps-seed"),
    };
    state
        .registry
        .create_palace(&state.data_root, palace)
        .expect("create palace");

    state.registry.set_gaps(
        PalaceId::new("gaps-seed"),
        vec![KnowledgeGap {
            entities: vec!["foo".to_string(), "bar".to_string(), "baz".to_string()],
            internal_density: 0.15,
            external_bridges: 2,
            suggested_exploration: "Explore connections between foo and related concepts"
                .to_string(),
        }],
    );

    let app = router().with_state(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/kg/gaps?palace=gaps-seed")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = to_bytes(resp.into_body(), 4096).await.unwrap();
    let v: Value = serde_json::from_slice(&bytes).unwrap();
    let arr = v.as_array().expect("array");
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["entities"].as_array().unwrap().len(), 3);
    assert_eq!(arr[0]["external_bridges"], 2);
    assert!(arr[0]["suggested_exploration"]
        .as_str()
        .unwrap()
        .contains("foo"));
}

/// Why: The KG Explorer UI calls `/api/v1/palaces/{id}/kg/subjects` to
/// populate the left panel; the endpoint must return distinct active
/// subjects as a JSON string array.
/// What: Creates a palace, asserts two triples via the existing kg endpoint,
/// then GETs the subjects route and asserts the shape.
/// Test: this test itself.
#[tokio::test]
async fn kg_list_subjects_returns_distinct() {
    let state = test_state();
    let app = router().with_state(state.clone());

    // Create palace.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/palaces")
                .header("content-type", "application/json")
                .body(Body::from(json!({"name": "kg-list"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // Assert two triples on distinct subjects.
    for subj in ["alpha", "beta"] {
        let body = json!({
            "subject": subj,
            "predicate": "is",
            "object": "thing",
        })
        .to_string();
        let r = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/palaces/kg-list/kg")
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::NO_CONTENT);
    }

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/palaces/kg-list/kg/subjects?limit=10")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = to_bytes(resp.into_body(), 4096).await.unwrap();
    let v: Value = serde_json::from_slice(&bytes).unwrap();
    let arr = v.as_array().expect("subjects must be array");
    let subjects: Vec<String> = arr
        .iter()
        .filter_map(|x| x.as_str().map(String::from))
        .collect();
    assert_eq!(subjects, vec!["alpha".to_string(), "beta".to_string()]);
}

/// Why: KG Explorer's "All" mode pages through every active triple via
/// `/api/v1/palaces/{id}/kg/all`; the endpoint must return a JSON array of
/// `Triple` rows ordered by `valid_from` DESC.
/// What: Creates a palace, asserts a triple, then GETs the all route and
/// asserts the response is an array with the expected shape.
/// Test: this test itself.
#[tokio::test]
async fn kg_list_all_returns_paginated_triples() {
    let state = test_state();
    let app = router().with_state(state.clone());

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/palaces")
                .header("content-type", "application/json")
                .body(Body::from(json!({"name": "kg-all"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = json!({
        "subject": "alpha",
        "predicate": "is",
        "object": "thing",
    })
    .to_string();
    let r = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/palaces/kg-all/kg")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::NO_CONTENT);

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/palaces/kg-all/kg/all?limit=10&offset=0")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = to_bytes(resp.into_body(), 4096).await.unwrap();
    let v: Value = serde_json::from_slice(&bytes).unwrap();
    let arr = v.as_array().expect("triples must be array");
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["subject"], "alpha");
    assert_eq!(arr[0]["predicate"], "is");
    assert_eq!(arr[0]["object"], "thing");
}

/// Why (issue #97): The visual graph view fetches the entire active
/// triple set in one call so d3-force can lay it out without paging.
/// The endpoint must return the triple list plus the node/edge/
/// community counts that drive the legend.
/// What: Creates a palace, asserts a single triple, and confirms `GET
/// /api/v1/palaces/{id}/kg/graph` returns `{ triples, node_count,
/// edge_count, community_count }` with the right shape.
/// Test: This test.
#[tokio::test]
async fn kg_graph_returns_active_triples() {
    let state = test_state();
    let app = router().with_state(state.clone());

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/palaces")
                .header("content-type", "application/json")
                .body(Body::from(json!({"name": "kg-graph"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = json!({
        "subject": "alpha",
        "predicate": "is",
        "object": "thing",
    })
    .to_string();
    let r = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/palaces/kg-graph/kg")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::NO_CONTENT);

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/palaces/kg-graph/kg/graph")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = to_bytes(resp.into_body(), 16_384).await.unwrap();
    let v: Value = serde_json::from_slice(&bytes).unwrap();
    let triples = v["triples"].as_array().expect("triples array");
    assert!(triples
        .iter()
        .any(|t| t["subject"] == "alpha" && t["predicate"] == "is" && t["object"] == "thing"));
    assert!(v["node_count"].as_u64().is_some());
    assert!(v["edge_count"].as_u64().is_some());
    assert!(v["community_count"].as_u64().is_some());
}

/// Why (issue #97): The visual graph view's stated perf budget is
/// "<1s for palaces with <500 triples". Seed 500 triples, time one
/// `/kg/graph` round-trip, and assert the result stays well under that
/// budget. The assertion uses a generous 10x ceiling so flaky CI
/// hardware doesn't false-positive while still catching catastrophic
/// regressions.
/// What: Creates a palace, asserts 500 triples directly through the
/// `KnowledgeGraph` handle (skipping the HTTP overhead of 500 separate
/// `POST /kg` calls), then runs one `GET /kg/graph` and prints the
/// elapsed time to stderr.
/// Test: This test.
#[tokio::test]
async fn kg_graph_meets_perf_budget_for_500_triples() {
    let state = test_state();
    let app = router().with_state(state.clone());

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/palaces")
                .header("content-type", "application/json")
                .body(Body::from(json!({"name": "kg-perf"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let pid = trusty_common::memory_core::palace::PalaceId::new("kg-perf");
    let handle = state
        .registry
        .open_palace(&state.data_root, &pid)
        .expect("open palace");
    let now = chrono::Utc::now();
    for s in 0..10 {
        for o in 0..50 {
            handle
                .kg
                .assert(Triple {
                    subject: format!("s{s}"),
                    predicate: format!("p{o}"),
                    object: format!("o{o}"),
                    valid_from: now,
                    valid_to: None,
                    confidence: 1.0,
                    provenance: Some("perf-test".to_string()),
                })
                .await
                .expect("kg.assert");
        }
    }

    let started = std::time::Instant::now();
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/palaces/kg-perf/kg/graph")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let elapsed = started.elapsed();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = to_bytes(resp.into_body(), 1_000_000).await.unwrap();
    let v: Value = serde_json::from_slice(&bytes).unwrap();
    let n = v["triples"].as_array().map(|a| a.len()).unwrap_or(0);
    assert_eq!(n, 500, "expected 500 triples in payload");
    assert!(
        elapsed.as_secs_f64() < 10.0,
        "graph endpoint should serve 500 triples in well under 10s; took {elapsed:?}"
    );
    eprintln!(
        "[perf] kg_graph endpoint served 500 triples in {:.3}ms",
        elapsed.as_secs_f64() * 1000.0
    );
}
