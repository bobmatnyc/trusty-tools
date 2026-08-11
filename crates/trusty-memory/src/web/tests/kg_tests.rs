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

// ---------------------------------------------------------------------------
// #4670 — progressive graph loading + honest truncation signalling
// ---------------------------------------------------------------------------

/// Seed a palace with a hub-and-spoke KG and return the router + palace id.
///
/// Shape: `hub` has 3 outgoing edges (`a`, `b`, `c`) and 2 incoming (`s1`,
/// `s2`) → degree 5. `a→b` gives `a` and `b` degree 2 each. Every other node
/// is degree 1. Each subject needs distinct predicates because the adjacency
/// keeps at most one active edge per `(subject, predicate)`.
async fn seed_explore_palace(state: &crate::AppState, name: &str) -> axum::Router {
    let app = router().with_state(state.clone());
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/palaces")
                .header("content-type", "application/json")
                .body(Body::from(json!({ "name": name }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let handle = state
        .registry
        .open_palace(&state.data_root, &PalaceId::new(name))
        .expect("open palace");
    let now = chrono::Utc::now();
    for (s, p, o) in [
        ("hub", "p1", "a"),
        ("hub", "p2", "b"),
        ("hub", "p3", "c"),
        ("s1", "pa", "hub"),
        ("s2", "pb", "hub"),
        ("a", "q1", "b"),
    ] {
        handle
            .kg
            .assert(Triple {
                subject: s.into(),
                predicate: p.into(),
                object: o.into(),
                valid_from: now,
                valid_to: None,
                confidence: 1.0,
                provenance: None,
            })
            .await
            .expect("kg.assert");
    }
    app
}

/// Why (issue #4670): the seed endpoint is the graph view's first paint. If it
/// did not rank by degree it would be no better than the arbitrary
/// `valid_from`-ordered slice it replaces. It must also report the palace-wide
/// totals so the header can say "N of M shown".
/// What: seeds a 6-node fixture, requests `limit=3`, and asserts the returned
/// nodes are the three highest-degree ones in degree order, that only induced
/// edges come back, and that `node_count`/`truncated` describe the whole palace.
/// Test: this test.
#[tokio::test]
async fn kg_graph_seed_ranks_by_degree() {
    let state = test_state();
    let app = seed_explore_palace(&state, "kg-seed").await;

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/palaces/kg-seed/kg/graph/seed?limit=3")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = to_bytes(resp.into_body(), 65_536).await.unwrap();
    let v: Value = serde_json::from_slice(&bytes).unwrap();

    let ids: Vec<&str> = v["nodes"]
        .as_array()
        .expect("nodes")
        .iter()
        .map(|n| n["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec!["hub", "a", "b"], "seed must rank by degree desc");
    assert_eq!(v["nodes"][0]["degree"], 5);
    assert_eq!(v["nodes"][0]["out_degree"], 3);
    assert_eq!(v["nodes"][0]["in_degree"], 2);

    // Only the induced edges over {hub, a, b} — hub→c and s*→hub excluded.
    assert_eq!(v["triples"].as_array().unwrap().len(), 3);
    assert_eq!(v["returned_node_count"], 3);
    assert_eq!(v["returned_triple_count"], 3);
    // Palace-wide truth alongside the slice.
    assert_eq!(v["node_count"], 6);
    assert_eq!(v["edge_count"], 6);
    assert_eq!(v["truncated"], true);
    assert_eq!(v["limit"], 3);
}

/// Why (issue #4670): the seed limit is what keeps the client's O(n²) layout
/// tractable. A client asking for 100_000 must be clamped server-side, and
/// `limit=0` must not be read as "unbounded".
/// What: requests `limit=100000` and `limit=0`, asserting the echoed `limit`
/// is clamped to the 200 ceiling and the 1 floor respectively, and that the
/// default (no param) is 75.
/// Test: this test.
#[tokio::test]
async fn kg_graph_seed_clamps_limit() {
    let state = test_state();
    let app = seed_explore_palace(&state, "kg-seed-clamp").await;

    let get = |uri: &'static str, app: axum::Router| async move {
        let resp = app
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = to_bytes(resp.into_body(), 65_536).await.unwrap();
        serde_json::from_slice::<Value>(&bytes).unwrap()
    };

    let hi = get(
        "/api/v1/palaces/kg-seed-clamp/kg/graph/seed?limit=100000",
        app.clone(),
    )
    .await;
    assert_eq!(hi["limit"], 200, "limit must clamp to MAX_KG_SEED_LIMIT");

    let lo = get(
        "/api/v1/palaces/kg-seed-clamp/kg/graph/seed?limit=0",
        app.clone(),
    )
    .await;
    assert_eq!(
        lo["limit"], 1,
        "limit=0 must clamp to 1, not mean unbounded"
    );
    assert_eq!(lo["nodes"].as_array().unwrap().len(), 1);

    let def = get("/api/v1/palaces/kg-seed-clamp/kg/graph/seed", app).await;
    assert_eq!(def["limit"], 75, "default seed limit");
    // Fewer nodes exist than the default asks for — that is not truncation.
    assert_eq!(def["truncated"], false);
}

/// Why (issue #4670): this is the regression guard for the capability that did
/// not exist before. `kg_query` / `GET /kg?subject=X` is a subject prefix scan
/// and never reads the object side, so "what points at this node" was
/// unanswerable over HTTP. `direction=in` must return exactly those edges.
/// What: expands `hub` with `direction=in&max_hops=1` and asserts only the two
/// inbound edges come back, then repeats with `out` and `both` to confirm the
/// three directions are genuinely distinct.
/// Test: this test.
#[tokio::test]
async fn kg_neighbors_returns_incoming_edges() {
    let state = test_state();
    let app = seed_explore_palace(&state, "kg-nbr").await;

    let fetch = |uri: String, app: axum::Router| async move {
        let resp = app
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = to_bytes(resp.into_body(), 65_536).await.unwrap();
        serde_json::from_slice::<Value>(&bytes).unwrap()
    };

    let inbound = fetch(
        "/api/v1/palaces/kg-nbr/kg/graph/neighbors?node=hub&direction=in&max_hops=1".into(),
        app.clone(),
    )
    .await;
    assert_eq!(inbound["direction"], "in");
    assert_eq!(inbound["origin"], "hub");
    let tr = inbound["triples"].as_array().unwrap();
    assert_eq!(tr.len(), 2, "hub has exactly 2 inbound edges");
    for t in tr {
        assert_eq!(t["object"], "hub", "direction=in must yield edges INTO hub");
    }
    let ids: std::collections::HashSet<&str> = inbound["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["hub", "s1", "s2"].into_iter().collect());
    // Origin is first so the client can anchor newly-added nodes on it.
    assert_eq!(inbound["nodes"][0]["id"], "hub");
    // Degree is graph-wide, not fragment-wide.
    assert_eq!(inbound["nodes"][0]["degree"], 5);

    let outbound = fetch(
        "/api/v1/palaces/kg-nbr/kg/graph/neighbors?node=hub&direction=out".into(),
        app.clone(),
    )
    .await;
    assert_eq!(outbound["triples"].as_array().unwrap().len(), 3);
    for t in outbound["triples"].as_array().unwrap() {
        assert_eq!(t["subject"], "hub");
    }

    // `both` is the default and must be the de-duplicated union.
    let both = fetch(
        "/api/v1/palaces/kg-nbr/kg/graph/neighbors?node=hub".into(),
        app,
    )
    .await;
    assert_eq!(both["direction"], "both");
    assert_eq!(both["triples"].as_array().unwrap().len(), 5);
    assert_eq!(both["returned_node_count"], 6);
}

/// Why (issue #4670): `max_hops` is the only thing stopping a click on a hub
/// from pulling the whole palace back. It must be clamped to `[1, 4]` — the
/// same window `trusty-search`'s `graph_neighbors_handler` uses.
/// What: asserts `max_hops=99` is echoed back as 4, `max_hops=0` as 1, and
/// that a 2-hop outbound walk really does reach further than a 1-hop one.
/// Test: this test.
#[tokio::test]
async fn kg_neighbors_clamps_max_hops() {
    let state = test_state();
    let app = seed_explore_palace(&state, "kg-nbr-hops").await;

    let fetch = |uri: String, app: axum::Router| async move {
        let resp = app
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = to_bytes(resp.into_body(), 65_536).await.unwrap();
        serde_json::from_slice::<Value>(&bytes).unwrap()
    };

    let hi = fetch(
        "/api/v1/palaces/kg-nbr-hops/kg/graph/neighbors?node=hub&max_hops=99".into(),
        app.clone(),
    )
    .await;
    assert_eq!(hi["max_hops"], 4, "max_hops must clamp to 4");

    let lo = fetch(
        "/api/v1/palaces/kg-nbr-hops/kg/graph/neighbors?node=hub&max_hops=0".into(),
        app.clone(),
    )
    .await;
    assert_eq!(
        lo["max_hops"], 1,
        "max_hops=0 must clamp to 1, not expand nothing"
    );
    assert!(lo["returned_triple_count"].as_u64().unwrap() > 0);

    let one = fetch(
        "/api/v1/palaces/kg-nbr-hops/kg/graph/neighbors?node=hub&direction=out&max_hops=1".into(),
        app.clone(),
    )
    .await;
    let two = fetch(
        "/api/v1/palaces/kg-nbr-hops/kg/graph/neighbors?node=hub&direction=out&max_hops=2".into(),
        app,
    )
    .await;
    assert_eq!(one["returned_triple_count"], 3);
    assert_eq!(two["returned_triple_count"], 4, "2 hops discovers a→b");
}

/// Why: an unparseable `direction` must be a 400, not a silent fallback to
/// `both` that renders edges the caller did not ask for.
/// What: asserts `direction=sideways` returns 400 and an unknown node returns
/// 200 with an empty expansion (a missing node is a normal UI state, not an
/// error banner).
/// Test: this test.
#[tokio::test]
async fn kg_neighbors_rejects_bad_direction() {
    let state = test_state();
    let app = seed_explore_palace(&state, "kg-nbr-bad").await;

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/palaces/kg-nbr-bad/kg/graph/neighbors?node=hub&direction=sideways")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/palaces/kg-nbr-bad/kg/graph/neighbors?node=ghost")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = to_bytes(resp.into_body(), 16_384).await.unwrap();
    let v: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v["returned_node_count"], 0);
    assert_eq!(v["returned_triple_count"], 0);
}

/// Why (issue #4670): THE correctness bug. `kg_graph` caps `triples` at
/// `KG_GRAPH_MAX_TRIPLES` while `node_count`/`edge_count` are computed over the
/// full adjacency, so the UI rendered 5,000 triples under a "9,311 nodes"
/// badge with nothing in the payload saying it was a partial view — and since
/// `list_active` orders by `valid_from` DESC, the dropped triples were silently
/// the oldest. The payload must now make truncation machine-detectable.
/// What: drives `kg_graph_with_cap` directly (the cap is a parameter precisely
/// so this branch is provable without seeding 5,001 triples) and asserts both
/// sides: capped below the palace size → `truncated: true` with the true
/// `active_triple_count`; capped above → `truncated: false`.
/// Test: this test.
#[tokio::test]
async fn kg_graph_signals_truncation() {
    let state = test_state();
    let _app = seed_explore_palace(&state, "kg-trunc").await;
    let svc = crate::service::MemoryService::new(state);

    let capped = svc
        .kg_graph_with_cap("kg-trunc", 3)
        .await
        .expect("kg_graph");
    assert_eq!(capped.triples.len(), 3);
    assert_eq!(capped.returned_triple_count, 3);
    assert_eq!(
        capped.active_triple_count, 6,
        "active count must reflect the whole palace, not the window"
    );
    assert!(
        capped.truncated,
        "a payload carrying 3 of 6 triples must say so"
    );
    // The counts that used to be silently inconsistent with `triples`.
    assert_eq!(capped.node_count, 6);
    assert_eq!(capped.edge_count, 6);

    let whole = svc
        .kg_graph_with_cap("kg-trunc", 1000)
        .await
        .expect("kg_graph");
    assert_eq!(whole.returned_triple_count, 6);
    assert_eq!(whole.active_triple_count, 6);
    assert!(
        !whole.truncated,
        "a complete payload must not claim truncation"
    );
}

// ---------------------------------------------------------------------------
// DELETE /api/v1/palaces/{id}/kg/triples/{triple_id}
// ---------------------------------------------------------------------------

/// Create a palace named `name` and assert `(subject, predicate, object)` for
/// every object given, so a delete test starts from siblings at one pair.
async fn seed_pair(
    app: &axum::Router,
    name: &str,
    subject: &str,
    predicate: &str,
    objects: &[&str],
) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/palaces")
                .header("content-type", "application/json")
                .body(Body::from(json!({ "name": name }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "create palace {name}");

    for object in objects {
        let body = json!({
            "subject": subject,
            "predicate": predicate,
            "object": object,
        })
        .to_string();
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/v1/palaces/{name}/kg"))
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT, "assert {object}");
    }
}

/// Return every active triple in `name` as `(subject, predicate, object)`.
async fn active_triples(app: &axum::Router, name: &str) -> Vec<(String, String, String)> {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/v1/palaces/{name}/kg/all?limit=50&offset=0"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = to_bytes(resp.into_body(), 16_384).await.unwrap();
    let v: Value = serde_json::from_slice(&bytes).unwrap();
    v.as_array()
        .expect("triples must be array")
        .iter()
        .map(|t| {
            (
                t["subject"].as_str().unwrap_or_default().to_string(),
                t["predicate"].as_str().unwrap_or_default().to_string(),
                t["object"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

/// Why: THE bug. The triple id encoded only `(subject, predicate)` and the
/// service called the pair-level `KnowledgeGraph::retract`, so deleting "one
/// triple" over the HTTP surface closed every object at that pair — an
/// `alpha is thing-a` delete also took `alpha is thing-b`, which the caller
/// never named and the 404 message could not even express. Against the
/// pre-fix code this test reports `left: 0, right: 1` at the surviving-sibling
/// assertion.
/// What: seeds two objects at one `(subject, predicate)` pair, DELETEs the id
/// for exactly one of them, and asserts `204` plus that the sibling is still
/// the one and only active triple. Then deletes the sibling too, to show the
/// pair can still be emptied one row at a time.
/// Test: this test.
#[tokio::test]
async fn kg_delete_triple_closes_one_object_and_keeps_siblings() {
    use super::super::kg_routes::encode_triple_id;

    let state = test_state();
    let app = router().with_state(state);
    seed_pair(&app, "kg-del", "alpha", "is", &["thing-a", "thing-b"]).await;

    let triple_id = encode_triple_id("alpha", "is", "thing-a").expect("encode triple id");
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/v1/palaces/kg-del/kg/triples/{triple_id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    assert_eq!(
        active_triples(&app, "kg-del").await,
        vec![("alpha".to_string(), "is".to_string(), "thing-b".to_string())],
        "deleting one object must leave its sibling at the same pair active"
    );

    // The pair can still be emptied — one named row at a time.
    let sibling_id = encode_triple_id("alpha", "is", "thing-b").expect("encode triple id");
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/v1/palaces/kg-del/kg/triples/{sibling_id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    assert!(active_triples(&app, "kg-del").await.is_empty());
}

/// Why: a delete that names an object nobody asserted must not be reported as
/// a success, and — since retraction is idempotent — repeating a delete that
/// already landed must answer the same way rather than closing something else.
/// What: seeds one object, deletes a different object at the same pair
/// (expect 404 with the object in the message, and the seeded row untouched),
/// then deletes the real one twice: 204 then 404.
/// Test: this test.
#[tokio::test]
async fn kg_delete_triple_returns_404_for_missing() {
    use super::super::kg_routes::encode_triple_id;

    let state = test_state();
    let app = router().with_state(state);
    seed_pair(&app, "kg-miss", "alpha", "is", &["thing-a"]).await;

    let wrong_object = encode_triple_id("alpha", "is", "thing-zzz").expect("encode triple id");
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/v1/palaces/kg-miss/kg/triples/{wrong_object}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let bytes = to_bytes(resp.into_body(), 4096).await.unwrap();
    let body = String::from_utf8_lossy(&bytes);
    assert!(
        body.contains("thing-zzz"),
        "the 404 must name the object it looked for; got {body}"
    );
    assert_eq!(
        active_triples(&app, "kg-miss").await.len(),
        1,
        "a miss must not close the sibling that does exist"
    );

    let real = encode_triple_id("alpha", "is", "thing-a").expect("encode triple id");
    for expected in [StatusCode::NO_CONTENT, StatusCode::NOT_FOUND] {
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/api/v1/palaces/kg-miss/kg/triples/{real}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), expected, "repeat delete must be idempotent");
    }
}

/// Why: an id in the old two-field form cannot name a triple, and answering
/// it with 404 would read as "already deleted" to a caller whose request was
/// never understood. It must fail closed and say what the id now needs.
/// What: hand-encodes `subject\0predicate`, DELETEs with it, and asserts 400
/// with the new format in the message and both seeded objects still active.
/// Test: this test.
#[tokio::test]
async fn kg_delete_triple_rejects_a_legacy_pair_id() {
    use base64::Engine as _;

    let state = test_state();
    let app = router().with_state(state);
    seed_pair(&app, "kg-legacy", "alpha", "is", &["thing-a", "thing-b"]).await;

    let legacy_id = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b"alpha\0is");
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/v1/palaces/kg-legacy/kg/triples/{legacy_id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let bytes = to_bytes(resp.into_body(), 4096).await.unwrap();
    let body = String::from_utf8_lossy(&bytes);
    assert!(
        body.contains("object"),
        "the 400 must say the id needs an object; got {body}"
    );
    assert_eq!(
        active_triples(&app, "kg-legacy").await.len(),
        2,
        "a rejected legacy id must close nothing"
    );
}
