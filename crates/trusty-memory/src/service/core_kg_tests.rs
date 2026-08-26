//! The knowledge graph, the dream cycle, and the activity log.
//!
//! Why this file exists (#6286): these tests lived in `web::tests::kg_tests`,
//! `dream_sse_tests`, `activity_tests` and `prompt_tests`, and drove an
//! in-process axum router ADR-0032 retired. What they assert is
//! `MemoryService` behaviour — the clamps, the truncation signal, the
//! one-triple retraction, the prompt-cache rebuild — reached through routes
//! that only decoded a path and re-encoded a result.
//!
//! **Where an assertion is about a CLAMP it now runs against the folded
//! method, not the service.** `kg_graph_seed`'s `limit` and `kg_neighbors`'
//! `max_hops` and `direction` were clamped and parsed in the axum extractor
//! layer, and that code moved to `transport::methods::kg` verbatim. Testing
//! those through `MemoryService` would assert nothing, because the service
//! takes an already-clamped value.
//!
//! **Status codes became `ServiceError` variants and `ApiError` kinds**, the
//! same mapping `core_tests.rs` documents.
//!
//! Test: run with `cargo test -p trusty-memory service::core_kg_tests`.

use serde_json::{json, Value};
use trusty_common::memory_core::palace::PalaceId;
use trusty_common::memory_core::store::kg::Triple;

use crate::service::{CreatePalaceBody, KgAssertBody, MemoryService, ServiceError};
use crate::transport::methods::kg as folded;
use crate::{ActivitySource, AppState};

/// Build a fresh `AppState` rooted in an ephemeral tempdir.
fn test_state() -> AppState {
    trusty_common::memory_core::retrieval::seed_shared_embedder_with_mock();
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().to_path_buf();
    std::mem::forget(tmp);
    // SAFETY: every test in this process wants the same idempotent "1".
    unsafe {
        std::env::set_var("TRUSTY_SKIP_PALACE_ENFORCEMENT", "1");
    }
    let state = AppState::new(root);
    state.set_ready();
    state
}

fn service() -> (MemoryService, AppState) {
    let state = test_state();
    (MemoryService::new(state.clone()), state)
}

fn palace_body(name: &str) -> CreatePalaceBody {
    CreatePalaceBody {
        name: name.to_string(),
        description: None,
        cwd: None,
        force: false,
    }
}

/// Create a palace on disk without going through the service, for a fixture
/// that only needs the directory and a handle.
fn seed_palace(state: &AppState, name: &str) {
    let palace = trusty_common::memory_core::Palace {
        id: PalaceId::new(name),
        name: name.to_string(),
        description: None,
        created_at: chrono::Utc::now(),
        data_dir: state.data_root.join(name),
    };
    state
        .registry
        .create_palace(&state.data_root, palace)
        .expect("create palace");
}

// ---------------------------------------------------------------------------
// Graph reads
// ---------------------------------------------------------------------------

/// Why (#97): the graph view asks for the whole active triple set in one call
/// so the layout can run without paging, and reads the node/edge/community
/// counts for its legend. The payload shape is what breaks silently.
/// Test: itself.
#[tokio::test]
async fn kg_graph_returns_active_triples() {
    let (svc, _state) = service();
    svc.create_palace(palace_body("kg-graph"), ActivitySource::Http)
        .await
        .expect("create");

    svc.kg_assert(
        "kg-graph",
        KgAssertBody {
            subject: "alpha".to_string(),
            predicate: "is".to_string(),
            object: "thing".to_string(),
            confidence: None,
            provenance: None,
        },
    )
    .await
    .expect("kg_assert");

    let graph = svc.kg_graph("kg-graph").await.expect("kg_graph");
    assert!(
        graph
            .triples
            .iter()
            .any(|t| t.subject == "alpha" && t.predicate == "is" && t.object == "thing"),
        "the asserted triple must come back: {:?}",
        graph.triples
    );
    // The three counts the legend renders. They are computed over the whole
    // adjacency, which is why `kg_graph_signals_truncation` below exists.
    let _: u64 = graph.node_count as u64;
    let _: u64 = graph.edge_count as u64;
    let _: u64 = graph.community_count as u64;
}

/// Seed a palace with a hub-and-spoke KG.
///
/// Shape: `hub` has 3 outgoing edges (`a`, `b`, `c`) and 2 incoming (`s1`,
/// `s2`) → degree 5. `a→b` gives `a` and `b` degree 2 each. Every other node is
/// degree 1. Each subject needs distinct predicates because the adjacency keeps
/// at most one active edge per `(subject, predicate)`.
async fn seed_explore_palace(state: &AppState, name: &str) {
    seed_palace(state, name);
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
}

/// Why (#4670): the seed is the graph view's first paint. Without degree
/// ranking it would be no better than the arbitrary `valid_from`-ordered slice
/// it replaced, and it must report palace-wide totals so the header can say
/// "N of M shown".
/// What: seeds the 6-node fixture, asks for 3, and asserts the three
/// highest-degree nodes come back in degree order with only induced edges.
/// Test: itself.
#[tokio::test]
async fn kg_graph_seed_ranks_by_degree() {
    let (svc, state) = service();
    seed_explore_palace(&state, "kg-seed").await;

    let seed = svc
        .kg_graph_seed("kg-seed", 3)
        .await
        .expect("kg_graph_seed");
    let ids: Vec<&str> = seed.nodes.iter().map(|n| n.id.as_str()).collect();
    assert_eq!(ids, vec!["hub", "a", "b"], "seed must rank by degree desc");
    assert_eq!(seed.nodes[0].degree, 5);
    assert_eq!(seed.nodes[0].out_degree, 3);
    assert_eq!(seed.nodes[0].in_degree, 2);

    // Only the induced edges over {hub, a, b} — hub→c and s*→hub excluded.
    assert_eq!(seed.triples.len(), 3);
    assert_eq!(seed.returned_node_count, 3);
    assert_eq!(seed.returned_triple_count, 3);
    // Palace-wide truth alongside the slice.
    assert_eq!(seed.node_count, 6);
    assert_eq!(seed.edge_count, 6);
    assert!(seed.truncated);
    assert_eq!(seed.limit, 3);
}

/// Why (#4670): the seed limit is what keeps the client's O(n²) layout
/// tractable. A client asking for 100_000 must be clamped, and `limit=0` must
/// not be read as "unbounded". The clamp lives in
/// `transport::methods::kg::kg_graph_seed` — it was in the axum extractor layer
/// before — so this drives the folded method rather than the service.
/// Test: itself.
#[tokio::test]
async fn kg_graph_seed_clamps_limit() {
    let (_svc, state) = service();
    seed_explore_palace(&state, "kg-seed-clamp").await;

    let seed = |limit: Option<u64>| {
        let state = state.clone();
        async move {
            let mut params = json!({ "palace_id": "kg-seed-clamp" });
            if let Some(limit) = limit {
                params["limit"] = json!(limit);
            }
            folded::kg_graph_seed(
                &state,
                serde_json::from_value(params).expect("params decode"),
            )
            .await
            .expect("kg_graph_seed")
        }
    };

    let hi = seed(Some(100_000)).await;
    assert_eq!(hi["limit"], 200, "limit must clamp to MAX_KG_SEED_LIMIT");

    let lo = seed(Some(0)).await;
    assert_eq!(
        lo["limit"], 1,
        "limit=0 must clamp to 1, not mean unbounded"
    );
    assert_eq!(lo["nodes"].as_array().expect("nodes").len(), 1);

    let def = seed(None).await;
    assert_eq!(def["limit"], 75, "default seed limit");
    // Fewer nodes exist than the default asks for — that is not truncation.
    assert_eq!(def["truncated"], false);
}

/// Why (#4670): this is the regression guard for the capability that did not
/// exist before. `kg_query` is a subject prefix scan and never reads the object
/// side, so "what points at this node" was unanswerable. `direction=in` must
/// return exactly those edges, and the three directions must be genuinely
/// distinct.
/// Test: itself.
#[tokio::test]
async fn kg_neighbors_returns_incoming_edges() {
    let (_svc, state) = service();
    seed_explore_palace(&state, "kg-nbr").await;

    let expand = |direction: &'static str, max_hops: u64| {
        let state = state.clone();
        async move {
            folded::kg_graph_neighbors(
                &state,
                serde_json::from_value(json!({
                    "palace_id": "kg-nbr",
                    "node": "hub",
                    "direction": direction,
                    "max_hops": max_hops,
                }))
                .expect("params decode"),
            )
            .await
            .expect("kg_graph_neighbors")
        }
    };

    let inbound = expand("in", 1).await;
    assert_eq!(inbound["direction"], "in");
    assert_eq!(inbound["origin"], "hub");
    let tr = inbound["triples"].as_array().expect("triples");
    assert_eq!(tr.len(), 2, "hub has exactly 2 inbound edges");
    for t in tr {
        assert_eq!(t["object"], "hub", "direction=in must yield edges INTO hub");
    }
    let ids: std::collections::HashSet<&str> = inbound["nodes"]
        .as_array()
        .expect("nodes")
        .iter()
        .map(|n| n["id"].as_str().expect("id"))
        .collect();
    assert_eq!(ids, ["hub", "s1", "s2"].into_iter().collect());
    // Origin is first so the client can anchor newly-added nodes on it.
    assert_eq!(inbound["nodes"][0]["id"], "hub");
    // Degree is graph-wide, not fragment-wide.
    assert_eq!(inbound["nodes"][0]["degree"], 5);

    let outbound = expand("out", 1).await;
    assert_eq!(outbound["triples"].as_array().expect("triples").len(), 3);
    for t in outbound["triples"].as_array().expect("triples") {
        assert_eq!(t["subject"], "hub");
    }

    // `both` is the default and must be the de-duplicated union.
    let both = folded::kg_graph_neighbors(
        &state,
        serde_json::from_value(json!({ "palace_id": "kg-nbr", "node": "hub" }))
            .expect("params decode"),
    )
    .await
    .expect("kg_graph_neighbors");
    assert_eq!(both["direction"], "both");
    assert_eq!(both["triples"].as_array().expect("triples").len(), 5);
    assert_eq!(both["returned_node_count"], 6);
}

/// Why (#4670): `max_hops` is the only thing stopping a click on a hub from
/// pulling the whole palace back. It must clamp to `[1, 4]` — the same window
/// trusty-search's neighbors endpoint uses.
/// Test: itself.
#[tokio::test]
async fn kg_neighbors_clamps_max_hops() {
    let (_svc, state) = service();
    seed_explore_palace(&state, "kg-nbr-hops").await;

    let expand = |direction: Option<&'static str>, max_hops: u64| {
        let state = state.clone();
        async move {
            let mut params = json!({
                "palace_id": "kg-nbr-hops",
                "node": "hub",
                "max_hops": max_hops,
            });
            if let Some(direction) = direction {
                params["direction"] = json!(direction);
            }
            folded::kg_graph_neighbors(
                &state,
                serde_json::from_value(params).expect("params decode"),
            )
            .await
            .expect("kg_graph_neighbors")
        }
    };

    let hi = expand(None, 99).await;
    assert_eq!(hi["max_hops"], 4, "max_hops must clamp to 4");

    let lo = expand(None, 0).await;
    assert_eq!(
        lo["max_hops"], 1,
        "max_hops=0 must clamp to 1, not expand nothing"
    );
    assert!(lo["returned_triple_count"].as_u64().expect("count") > 0);

    let one = expand(Some("out"), 1).await;
    let two = expand(Some("out"), 2).await;
    assert_eq!(one["returned_triple_count"], 3);
    assert_eq!(two["returned_triple_count"], 4, "2 hops discovers a→b");
}

/// Why: an unparseable `direction` must be refused, not silently fall back to
/// `both` and render edges the caller did not ask for. An unknown NODE is the
/// opposite case — a normal UI state, not an error banner.
/// Test: itself.
#[tokio::test]
async fn kg_neighbors_rejects_bad_direction() {
    let (_svc, state) = service();
    seed_explore_palace(&state, "kg-nbr-bad").await;

    let refused = folded::kg_graph_neighbors(
        &state,
        serde_json::from_value(json!({
            "palace_id": "kg-nbr-bad",
            "node": "hub",
            "direction": "sideways",
        }))
        .expect("params decode"),
    )
    .await
    .expect_err("an unknown direction must be refused");
    assert_eq!(refused.kind, crate::transport::ErrorKind::BadRequest);
    assert!(
        refused.message.contains("sideways"),
        "the refusal must name what was sent: {}",
        refused.message
    );

    let ghost = folded::kg_graph_neighbors(
        &state,
        serde_json::from_value(json!({ "palace_id": "kg-nbr-bad", "node": "ghost" }))
            .expect("params decode"),
    )
    .await
    .expect("an unknown node is an empty expansion, not a failure");
    assert_eq!(ghost["returned_node_count"], 0);
    assert_eq!(ghost["returned_triple_count"], 0);
}

/// Why (#4670): THE correctness bug. `kg_graph` caps `triples` while
/// `node_count`/`edge_count` are computed over the full adjacency, so the UI
/// rendered 5,000 triples under a "9,311 nodes" badge with nothing saying it
/// was partial — and since `list_active` orders by `valid_from` DESC, the
/// dropped triples were silently the oldest.
/// What: drives `kg_graph_with_cap` directly — the cap is a parameter precisely
/// so this branch is provable without seeding 5,001 triples.
/// Test: itself.
#[tokio::test]
async fn kg_graph_signals_truncation() {
    let (svc, state) = service();
    seed_explore_palace(&state, "kg-trunc").await;

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
// Retracting one triple
// ---------------------------------------------------------------------------

/// Create `name` and assert `(subject, predicate, object)` for every object
/// given, so a delete test starts from siblings at one pair.
async fn seed_pair(
    svc: &MemoryService,
    name: &str,
    subject: &str,
    predicate: &str,
    objects: &[&str],
) {
    svc.create_palace(palace_body(name), ActivitySource::Http)
        .await
        .unwrap_or_else(|e| panic!("create palace {name}: {e}"));
    for object in objects {
        svc.kg_assert(
            name,
            KgAssertBody {
                subject: subject.to_string(),
                predicate: predicate.to_string(),
                object: (*object).to_string(),
                confidence: None,
                provenance: None,
            },
        )
        .await
        .unwrap_or_else(|e| panic!("assert {object}: {e}"));
    }
}

/// Every active triple in `name` as `(subject, predicate, object)`.
async fn active_triples(svc: &MemoryService, name: &str) -> Vec<(String, String, String)> {
    svc.kg_list_all(name, 50, 0)
        .await
        .expect("kg_list_all")
        .into_iter()
        .map(|t| (t.subject, t.predicate, t.object))
        .collect()
}

/// Why: THE bug. The triple id encoded only `(subject, predicate)` and the
/// service called the pair-level retract, so deleting "one triple" closed every
/// object at that pair — an `alpha is thing-a` delete also took
/// `alpha is thing-b`, which the caller never named and the refusal could not
/// even express. Against the pre-fix code this reports `left: 0, right: 1` at
/// the surviving-sibling assertion.
/// Test: itself.
#[tokio::test]
async fn kg_delete_triple_closes_one_object_and_keeps_siblings() {
    let (svc, state) = service();
    seed_pair(&svc, "kg-del", "alpha", "is", &["thing-a", "thing-b"]).await;

    delete_triple(&state, "kg-del", "alpha", "is", "thing-a")
        .await
        .expect("deleting one named object must succeed");

    assert_eq!(
        active_triples(&svc, "kg-del").await,
        vec![("alpha".to_string(), "is".to_string(), "thing-b".to_string())],
        "deleting one object must leave its sibling at the same pair active"
    );

    // The pair can still be emptied — one named row at a time.
    delete_triple(&state, "kg-del", "alpha", "is", "thing-b")
        .await
        .expect("the sibling deletes too");
    assert!(active_triples(&svc, "kg-del").await.is_empty());
}

/// Delete one triple through the folded method, by the id a caller builds.
async fn delete_triple(
    state: &AppState,
    palace: &str,
    subject: &str,
    predicate: &str,
    object: &str,
) -> Result<serde_json::Value, crate::transport::ApiError> {
    let triple_id = folded::encode_triple_id(subject, predicate, object).expect("encode triple id");
    folded::kg_delete_triple(
        state,
        serde_json::from_value(json!({ "palace_id": palace, "triple_id": triple_id }))
            .expect("params decode"),
    )
    .await
}

/// Why: a delete naming an object nobody asserted must not report success, and
/// — since retraction is idempotent — repeating a delete that already landed
/// must answer the same way rather than closing something else.
/// Test: itself.
#[tokio::test]
async fn kg_delete_triple_returns_404_for_missing() {
    let (svc, state) = service();
    seed_pair(&svc, "kg-miss", "alpha", "is", &["thing-a"]).await;

    let missed = delete_triple(&state, "kg-miss", "alpha", "is", "thing-zzz")
        .await
        .expect_err("an object nobody asserted must be refused");
    assert_eq!(missed.kind, crate::transport::ErrorKind::NotFound);
    assert!(
        missed.message.contains("thing-zzz"),
        "the refusal must name the object it looked for; got {}",
        missed.message
    );
    assert_eq!(
        active_triples(&svc, "kg-miss").await.len(),
        1,
        "a miss must not close the sibling that does exist"
    );

    delete_triple(&state, "kg-miss", "alpha", "is", "thing-a")
        .await
        .expect("the real object deletes");
    let repeat = delete_triple(&state, "kg-miss", "alpha", "is", "thing-a")
        .await
        .expect_err("repeat delete must be idempotent, not a second success");
    assert_eq!(repeat.kind, crate::transport::ErrorKind::NotFound);
}

/// Why: an id in the old two-field form cannot name a triple, and answering it
/// with "not found" would read as "already deleted" to a caller whose request
/// was never understood. It must fail closed and say what the id now needs.
/// Test: itself.
#[tokio::test]
async fn kg_delete_triple_rejects_a_legacy_pair_id() {
    use base64::Engine as _;

    let (svc, state) = service();
    seed_pair(&svc, "kg-legacy", "alpha", "is", &["thing-a", "thing-b"]).await;

    let legacy_id = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b"alpha\0is");
    let refused = folded::kg_delete_triple(
        &state,
        serde_json::from_value(json!({ "palace_id": "kg-legacy", "triple_id": legacy_id }))
            .expect("params decode"),
    )
    .await
    .expect_err("a pair id cannot name a triple");
    assert_eq!(refused.kind, crate::transport::ErrorKind::BadRequest);
    assert!(
        refused.message.contains("object"),
        "the refusal must say the id needs an object; got {}",
        refused.message
    );
    assert_eq!(
        active_triples(&svc, "kg-legacy").await.len(),
        2,
        "a rejected legacy id must close nothing"
    );
}

/// Why: the other delete tests all run under the predicate `is`, which
/// `is_hot_predicate` rejects, so the prompt-cache rebuild in
/// `MemoryService::kg_retract_triple` never runs in them. Nothing else stops a
/// later edit inverting the condition or dropping the call while every existing
/// test stays green, and the hazard is the one the method's own doc names: a
/// retracted Tier S fact keeps being injected into every session's prompt.
/// What: writes an alias under the hot predicate `is_alias_for`, confirms the
/// prompt context serves it, retracts that triple, and confirms it is gone. The
/// cache is only re-read, never re-derived, so the second read can change only
/// if the delete rebuilt it.
/// Test: itself.
#[tokio::test]
async fn kg_delete_triple_rebuilds_prompt_cache_for_hot_predicate() {
    let (svc, state) = service();
    svc.create_palace(palace_body("kg-hot"), ActivitySource::Http)
        .await
        .expect("create palace kg-hot");

    crate::tools::dispatch_tool(
        &state,
        "add_alias",
        json!({
            "palace": "kg-hot",
            "short": "tmhot",
            "full": "trusty-memory-hot-fact",
        }),
    )
    .await
    .expect("seed the hot alias");

    let before = state.prompt_context_cache.read().await.formatted.clone();
    assert!(
        before.contains("tmhot → trusty-memory-hot-fact"),
        "the hot fact must be cached before the delete; got: {before}"
    );

    delete_triple(
        &state,
        "kg-hot",
        "tmhot",
        "is_alias_for",
        "trusty-memory-hot-fact",
    )
    .await
    .expect("retract the hot triple");

    let after = state.prompt_context_cache.read().await.formatted.clone();
    assert!(
        !after.contains("trusty-memory-hot-fact"),
        "a retracted hot fact must not still be injected into the prompt; got: {after}"
    );
}

// ---------------------------------------------------------------------------
// kg_assert and the prompt cache (#5524)
// ---------------------------------------------------------------------------

/// A state whose default palace is `name`, with that palace on disk.
fn state_with_palace(name: &str) -> AppState {
    trusty_common::memory_core::retrieval::seed_shared_embedder_with_mock();
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().to_path_buf();
    std::mem::forget(tmp);
    // SAFETY: every test in this process wants the same idempotent "1".
    unsafe {
        std::env::set_var("TRUSTY_SKIP_PALACE_ENFORCEMENT", "1");
    }
    let state = AppState::new(root).with_default_palace(Some(name.to_string()));
    state.set_ready();
    seed_palace(&state, name);
    state
}

/// Regression for [#5524](https://github.com/bobmatnyc/trusty-tools/issues/5524):
/// a hot fact asserted through `MemoryService::kg_assert` must be visible in the
/// prompt context, not merely stored.
///
/// Before the fix `kg_assert` returned without touching the prompt cache, so
/// this assertion on `formatted` found an empty string — the fact was in the KG
/// and invisible to every later turn.
/// Test: itself.
#[tokio::test]
async fn http_kg_assert_endpoint_refreshes_prompt_cache() {
    let state = state_with_palace("httpassert");
    let svc = MemoryService::new(state.clone());

    svc.kg_assert(
        "httpassert",
        KgAssertBody {
            subject: "trusty-tools".to_string(),
            predicate: "has_convention".to_string(),
            object: "thiserror for libraries, anyhow for binaries".to_string(),
            confidence: None,
            provenance: None,
        },
    )
    .await
    .expect("kg_assert");

    let guard = state.prompt_context_cache.read().await;
    assert!(
        guard
            .formatted
            .contains("thiserror for libraries, anyhow for binaries"),
        "kg_assert did not reach the prompt cache; got: {:?}",
        guard.formatted
    );
}

/// The error arm of the same path: a Tier S refusal must leave both the graph
/// and the prompt cache untouched.
///
/// Why this is here and not only in `kg_write::tests`: consolidation moved the
/// gate behind a shared function, and what could regress is the variant mapping
/// at the caller — which only a caller-level test sees.
/// Test: itself.
#[tokio::test]
async fn http_kg_assert_endpoint_rejects_over_long_tier_s_object() {
    let state = state_with_palace("httpreject");
    let svc = MemoryService::new(state.clone());
    let over_long = "x".repeat(crate::prompt_facts::TIER_S_MAX_OBJECT_CHARS + 1);

    let refused = svc
        .kg_assert(
            "httpreject",
            KgAssertBody {
                subject: "trusty-tools".to_string(),
                predicate: "has_convention".to_string(),
                object: over_long,
                confidence: None,
                provenance: None,
            },
        )
        .await
        .expect_err("an over-long Tier S object must be refused");
    assert!(
        matches!(refused, ServiceError::BadRequest(_)),
        "expected BadRequest, got {refused:?}"
    );

    let handle = state
        .registry
        .get(&PalaceId::new("httpreject"))
        .expect("palace handle");
    let stored = handle.kg.query_active("trusty-tools").await.expect("query");
    assert!(
        stored.is_empty(),
        "refused write reached storage: {stored:?}"
    );
    assert!(
        state.prompt_context_cache.read().await.triples.is_empty(),
        "refused write reached the prompt cache"
    );
}

/// Why (#42): the prompt-facts read returns every hot-predicate triple across
/// the registry, so a dashboard can render its own table — and, since #4890, so
/// `trusty-memory doctor`'s Tier S re-affirmation check can decode it into
/// `Vec<TierSFact>`. A dropped or renamed `affirmed_at` would leave that check
/// permanently reporting "could not determine" against a healthy daemon.
/// Test: itself.
#[tokio::test]
async fn list_prompt_facts_endpoint_returns_hot_triples() {
    let state = state_with_palace("listfacts");
    let handle = state
        .registry
        .get(&PalaceId::new("listfacts"))
        .expect("palace handle");

    // One hot triple and one non-hot; only the hot one may surface.
    for (subject, predicate, object) in [
        ("ts", "is_alias_for", "trusty-search"),
        ("alice", "works_at", "Acme"),
    ] {
        handle
            .kg
            .assert(Triple {
                subject: subject.into(),
                predicate: predicate.into(),
                object: object.into(),
                valid_from: chrono::Utc::now(),
                valid_to: None,
                confidence: 1.0,
                provenance: None,
            })
            .await
            .unwrap_or_else(|e| panic!("assert {predicate}: {e}"));
    }

    let listed =
        crate::tools::dispatch_tool(&state, crate::prompt_facts::PROMPT_FACTS_METHOD, json!({}))
            .await
            .expect("list_prompt_facts");
    let rows = listed["facts"].as_array().expect("a `facts` array").clone();
    assert!(
        rows.iter().any(|r| r["subject"] == "ts"
            && r["predicate"] == "is_alias_for"
            && r["object"] == "trusty-search"),
        "missing the ts alias; got {rows:?}"
    );
    assert!(
        !rows.iter().any(|r| r["predicate"] == "works_at"),
        "a non-hot triple leaked into the prompt facts: {rows:?}"
    );

    // #4890: decoded the way the doctor decodes it — including the `facts`
    // unwrap — so the shape that check depends on is pinned rather than
    // assumed. The retired REST route answered a BARE array, so this unwrap is
    // the one thing #6286 changed about the contract.
    let facts: Vec<crate::prompt_facts::TierSFact> = serde_json::from_value(Value::Array(rows))
        .expect("rows must decode as the doctor decodes them");
    let alias = facts
        .iter()
        .find(|f| f.subject == "ts")
        .expect("ts alias present");
    assert!(
        (chrono::Utc::now() - alias.affirmed_at).num_seconds() < 60,
        "affirmed_at must be the write time, got {}",
        alias.affirmed_at,
    );
}

// ---------------------------------------------------------------------------
// Dream cycle
// ---------------------------------------------------------------------------

/// Why: the aggregate must SUM per-palace counters and surface the most recent
/// `last_run_at`. A regression returning only the first palace's stats would
/// silently break the global dream panel.
/// Test: itself.
#[tokio::test]
async fn dream_status_aggregates_across_palaces() {
    use trusty_common::memory_core::dream::{DreamStats, PersistedDreamStats};

    let (svc, state) = service();
    for (id, stats, ts) in [
        (
            "palace-a",
            DreamStats {
                merged: 1,
                pruned: 2,
                compacted: 3,
                closets_updated: 4,
                duration_ms: 100,
                ..DreamStats::default()
            },
            chrono::Utc::now() - chrono::Duration::seconds(60),
        ),
        (
            "palace-b",
            DreamStats {
                merged: 10,
                pruned: 20,
                compacted: 30,
                closets_updated: 40,
                duration_ms: 200,
                ..DreamStats::default()
            },
            chrono::Utc::now(),
        ),
    ] {
        seed_palace(&state, id);
        PersistedDreamStats {
            last_run_at: ts,
            stats,
        }
        .save(&state.data_root.join(id))
        .expect("save dream stats");
    }

    let later = chrono::Utc::now();
    let payload =
        serde_json::to_value(svc.dream_status_aggregate().await).expect("dream status serialises");

    assert_eq!(payload["merged"], 11);
    assert_eq!(payload["pruned"], 22);
    assert_eq!(payload["compacted"], 33);
    assert_eq!(payload["closets_updated"], 44);
    assert_eq!(payload["duration_ms"], 300);

    let last = payload["last_run_at"]
        .as_str()
        .expect("last_run_at is a string");
    let parsed: chrono::DateTime<chrono::Utc> =
        last.parse().expect("last_run_at parses as RFC3339");
    assert!(
        parsed <= later,
        "last_run_at ({parsed}) should not exceed wall clock ({later})"
    );
    // Must have picked palace-b's newer stamp, not palace-a's older one.
    let cutoff = chrono::Utc::now() - chrono::Duration::seconds(30);
    assert!(
        parsed >= cutoff,
        "expected the newer (palace-b) timestamp; got {parsed}"
    );
}

/// Why: the dashboard's "Run now" button must never fail the UI, so even a
/// registry with nothing to consolidate has to answer the full payload shape.
/// Deeper assertions are skipped deliberately: a real dream cycle loads the
/// ONNX embedder, and this test stays fast and embedder-free.
/// Test: itself.
#[tokio::test]
async fn dream_run_aggregates_stats() {
    let (svc, state) = service();
    seed_palace(&state, "dream-run-test");

    let payload =
        serde_json::to_value(svc.dream_run().await.expect("dream_run")).expect("serialises");
    for key in [
        "merged",
        "pruned",
        "compacted",
        "closets_updated",
        "duration_ms",
    ] {
        assert!(
            payload.get(key).is_some(),
            "missing key {key} in the dream_run payload: {payload}"
        );
        assert!(
            payload[key].is_u64() || payload[key].is_i64(),
            "{key} should be an integer, got {}",
            payload[key]
        );
    }
    assert!(
        payload["last_run_at"].is_string(),
        "last_run_at must be set by dream_run; got {payload}"
    );
}

// ---------------------------------------------------------------------------
// Activity log
// ---------------------------------------------------------------------------

/// Why (#96): the activity page seeds the console feed on mount, so the
/// persisted log has to come back in newest-first order with its source labels
/// and a structured payload. Without it the pane rendered empty until the next
/// live event.
/// Test: itself.
#[tokio::test]
async fn activity_endpoint_lists_recent_emits() {
    use crate::DaemonEvent;

    let (_svc, state) = service();
    // Three drawer events (one MCP, two HTTP) and one palace_created.
    state.emit(DaemonEvent::PalaceCreated {
        id: "alpha".into(),
        name: "alpha".into(),
        source: ActivitySource::Http,
    });
    state.emit(DaemonEvent::DrawerAdded {
        palace_id: "alpha".into(),
        palace_name: "alpha".into(),
        drawer_count: 1,
        timestamp: chrono::Utc::now(),
        content_preview: "hello".into(),
        source: ActivitySource::Mcp,
    });
    state.emit(DaemonEvent::DrawerAdded {
        palace_id: "beta".into(),
        palace_name: "beta".into(),
        drawer_count: 1,
        timestamp: chrono::Utc::now(),
        content_preview: "hi there".into(),
        source: ActivitySource::Http,
    });
    state.emit(DaemonEvent::DrawerDeleted {
        palace_id: "alpha".into(),
        drawer_count: 0,
        source: ActivitySource::Http,
    });
    // #232: emits fire-and-forget the redb write on the blocking pool, so wait
    // for them to settle before reading back.
    state.flush_activity_writes().await;

    let page = crate::transport::methods::activity::activity(
        &state,
        serde_json::from_value(json!({ "limit": 10 })).expect("params decode"),
    )
    .await
    .expect("activity");

    assert_eq!(page["limit"], 10);
    assert_eq!(page["offset"], 0);
    assert_eq!(page["total"], 4);
    let entries = page["entries"].as_array().expect("entries array");
    assert_eq!(entries.len(), 4);
    // Newest-first: drawer_deleted was the last event pushed.
    assert_eq!(entries[0]["event_type"], "drawer_deleted");
    assert_eq!(entries[3]["event_type"], "palace_created");
    // Sources made it onto the wire as lowercase strings.
    let sources: Vec<&str> = entries
        .iter()
        .filter_map(|e| e["source"].as_str())
        .collect();
    assert!(sources.contains(&"http"));
    assert!(sources.contains(&"mcp"));
    // Payload is structured JSON, not an escaped string.
    assert!(entries[0]["payload"].is_object());
}
