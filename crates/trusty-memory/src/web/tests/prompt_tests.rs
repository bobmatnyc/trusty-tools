//! Tests for prompt-context, add-alias, list-prompt-facts, remove-prompt-fact.

use super::super::router;
use super::test_state;
use crate::AppState;
use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use serde_json::{json, Value};
use tower::util::ServiceExt;
use trusty_common::memory_core::palace::PalaceId;
use trusty_common::memory_core::store::kg::Triple;

/// Why (issue #42): `GET /api/v1/kg/prompt-context` must serve the
/// formatted Markdown block from the in-memory cache (or a placeholder
/// when empty). Mirrors the MCP `get_prompt_context` tool but over HTTP.
#[tokio::test]
async fn prompt_context_endpoint_returns_formatted_block() {
    let state = test_state();

    // Empty cache returns the placeholder text.
    let app = router().with_state(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/kg/prompt-context")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = to_bytes(resp.into_body(), 4096).await.unwrap();
    let text = String::from_utf8(bytes.to_vec()).unwrap();
    assert_eq!(text, "No prompt facts stored yet.");

    // Populate the cache and re-fetch.
    {
        let mut guard = state.prompt_context_cache.write().await;
        let triples = vec![(
            "tga".to_string(),
            "is_alias_for".to_string(),
            "trusty-git-analytics".to_string(),
        )];
        let formatted = crate::prompt_facts::build_prompt_context(&triples);
        *guard = crate::prompt_facts::PromptFactsCache { triples, formatted };
    }
    let app = router().with_state(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/kg/prompt-context")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = to_bytes(resp.into_body(), 4096).await.unwrap();
    let text = String::from_utf8(bytes.to_vec()).unwrap();
    assert!(text.contains("tga → trusty-git-analytics"), "got: {text}");
}

/// Why (issue #42): `POST /api/v1/kg/aliases` must assert the alias as
/// an `is_alias_for` triple AND refresh the prompt cache so subsequent
/// reads see the new alias.
#[tokio::test]
async fn add_alias_endpoint_asserts_triple_and_refreshes_cache() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().to_path_buf();
    std::mem::forget(tmp);
    let state = AppState::new(root).with_default_palace(Some("aliases".to_string()));
    let palace = trusty_common::memory_core::Palace {
        id: PalaceId::new("aliases"),
        name: "aliases".to_string(),
        description: None,
        created_at: chrono::Utc::now(),
        data_dir: state.data_root.join("aliases"),
    };
    state
        .registry
        .create_palace(&state.data_root, palace)
        .expect("create palace");

    let body = json!({"short": "tm", "full": "trusty-memory"});
    let app = router().with_state(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/kg/aliases")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = to_bytes(resp.into_body(), 4096).await.unwrap();
    let v: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v["subject"], "tm");
    assert_eq!(v["object"], "trusty-memory");

    // The prompt cache must reflect the new alias.
    let guard = state.prompt_context_cache.read().await;
    assert!(
        guard.formatted.contains("tm → trusty-memory"),
        "cache missing alias; got: {}",
        guard.formatted
    );
}

/// Why (issue #42): `GET /api/v1/kg/prompt-facts` returns the structured
/// JSON array of every hot-predicate triple across the registry (so a
/// dashboard can render its own table).
#[tokio::test]
async fn list_prompt_facts_endpoint_returns_hot_triples() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().to_path_buf();
    std::mem::forget(tmp);
    let state = AppState::new(root).with_default_palace(Some("listfacts".to_string()));
    let palace = trusty_common::memory_core::Palace {
        id: PalaceId::new("listfacts"),
        name: "listfacts".to_string(),
        description: None,
        created_at: chrono::Utc::now(),
        data_dir: state.data_root.join("listfacts"),
    };
    let handle = state
        .registry
        .create_palace(&state.data_root, palace)
        .expect("create palace");

    // Insert one hot triple and one non-hot triple; only the hot one
    // should surface.
    handle
        .kg
        .assert(Triple {
            subject: "ts".to_string(),
            predicate: "is_alias_for".to_string(),
            object: "trusty-search".to_string(),
            valid_from: chrono::Utc::now(),
            valid_to: None,
            confidence: 1.0,
            provenance: None,
        })
        .await
        .expect("assert alias");
    handle
        .kg
        .assert(Triple {
            subject: "alice".to_string(),
            predicate: "works_at".to_string(),
            object: "Acme".to_string(),
            valid_from: chrono::Utc::now(),
            valid_to: None,
            confidence: 1.0,
            provenance: None,
        })
        .await
        .expect("assert works_at");

    let app = router().with_state(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/kg/prompt-facts")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = to_bytes(resp.into_body(), 4096).await.unwrap();
    let v: Value = serde_json::from_slice(&bytes).unwrap();
    let arr = v.as_array().expect("array");
    assert!(
        arr.iter().any(|r| r["subject"] == "ts"
            && r["predicate"] == "is_alias_for"
            && r["object"] == "trusty-search"),
        "missing ts alias; got {arr:?}"
    );
    // The non-hot `works_at` triple must not be present.
    assert!(
        !arr.iter().any(|r| r["predicate"] == "works_at"),
        "non-hot triple leaked into prompt facts: {arr:?}"
    );
}

/// Why (issue #42): `DELETE /api/v1/kg/prompt-facts` must retract the
/// interval and refresh the cache; the next list call must omit it.
#[tokio::test]
async fn remove_prompt_fact_endpoint_soft_deletes_and_refreshes_cache() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().to_path_buf();
    std::mem::forget(tmp);
    let state = AppState::new(root).with_default_palace(Some("rmfacts".to_string()));
    let palace = trusty_common::memory_core::Palace {
        id: PalaceId::new("rmfacts"),
        name: "rmfacts".to_string(),
        description: None,
        created_at: chrono::Utc::now(),
        data_dir: state.data_root.join("rmfacts"),
    };
    let handle = state
        .registry
        .create_palace(&state.data_root, palace)
        .expect("create palace");

    handle
        .kg
        .assert(Triple {
            subject: "ta".to_string(),
            predicate: "is_alias_for".to_string(),
            object: "trusty-analyze".to_string(),
            valid_from: chrono::Utc::now(),
            valid_to: None,
            confidence: 1.0,
            provenance: None,
        })
        .await
        .expect("assert alias");
    // Prime the cache so we can observe the removal effect.
    crate::prompt_facts::rebuild_prompt_cache(&state)
        .await
        .expect("rebuild prompt cache");

    let app = router().with_state(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/v1/kg/prompt-facts?subject=ta&predicate=is_alias_for")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = to_bytes(resp.into_body(), 4096).await.unwrap();
    let v: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v["removed"], true);
    assert!(v["closed"].as_u64().unwrap_or(0) >= 1);

    // Cache must no longer contain the alias.
    {
        let guard = state.prompt_context_cache.read().await;
        assert!(
            !guard.formatted.contains("ta → trusty-analyze"),
            "alias still in cache after delete: {}",
            guard.formatted
        );
    }

    // Removing a non-existent fact returns removed=false.
    let app = router().with_state(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/v1/kg/prompt-facts?subject=nope&predicate=is_alias_for")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = to_bytes(resp.into_body(), 4096).await.unwrap();
    let v: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v["removed"], false);
}

// ---------------------------------------------------------------------------
// #4888 — Tier S admission control over HTTP (ADR-0028 D2 / D8)
//
// The MCP tools are not the only way to write a hot predicate. These two
// endpoints can each create one, so a gate that covered only the MCP surface
// would read as protection while leaving the surface writable.
// ---------------------------------------------------------------------------

/// Build a palace named `name` and fill Tier S to the cap with conventions.
async fn state_with_full_tier_s(name: &str) -> AppState {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().to_path_buf();
    std::mem::forget(tmp);
    let state = AppState::new(root).with_default_palace(Some(name.to_string()));
    let palace = trusty_common::memory_core::Palace {
        id: PalaceId::new(name),
        name: name.to_string(),
        description: None,
        created_at: chrono::Utc::now(),
        data_dir: state.data_root.join(name),
    };
    let handle = state
        .registry
        .create_palace(&state.data_root, palace)
        .expect("create palace");
    for i in 0..crate::prompt_facts::TIER_S_MAX_FACTS {
        handle
            .kg
            .assert(Triple {
                subject: format!("rule-{i}"),
                predicate: "has_convention".to_string(),
                object: format!("standing rule number {i}"),
                valid_from: chrono::Utc::now(),
                valid_to: None,
                confidence: 1.0,
                provenance: None,
            })
            .await
            .expect("seed convention");
    }
    state
}

/// Why (#4888): `POST /api/v1/kg/aliases` writes `is_alias_for` directly, so
/// it consumes a Tier S slot and must refuse the write past the cap with an
/// actionable 400 rather than silently growing the always-injected surface.
#[tokio::test]
async fn add_alias_endpoint_enforces_tier_s_cap() {
    let state = state_with_full_tier_s("httpcap").await;

    let body = json!({"short": "tga", "full": "trusty-git-analytics"});
    let app = router().with_state(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/kg/aliases")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let bytes = to_bytes(resp.into_body(), 8192).await.unwrap();
    let text = String::from_utf8(bytes.to_vec()).unwrap();
    assert!(text.contains("Tier S is full"), "got: {text}");
    assert!(text.contains("remove_prompt_fact"), "got: {text}");

    // Fail-closed: the alias is absent from the surface.
    let facts = crate::prompt_facts::gather_hot_triples(&state)
        .await
        .expect("gather");
    assert_eq!(facts.len(), crate::prompt_facts::TIER_S_MAX_FACTS);
    assert!(!facts.iter().any(|(s, _, _)| s == "tga"), "{facts:?}");
}

/// Why (#4888): `POST /api/v1/palaces/{id}/kg` takes an arbitrary predicate,
/// so it can write a hot one. This is the path `trusty-mpm`'s provisioner
/// uses to seed its identity `is_fact`, which makes it a real bypass rather
/// than a hypothetical one. The form constraint must hold here too.
#[tokio::test]
async fn kg_assert_endpoint_enforces_tier_s_form_constraint() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().to_path_buf();
    std::mem::forget(tmp);
    let state = AppState::new(root);
    let palace = trusty_common::memory_core::Palace {
        id: PalaceId::new("httpform"),
        name: "httpform".to_string(),
        description: None,
        created_at: chrono::Utc::now(),
        data_dir: state.data_root.join("httpform"),
    };
    state
        .registry
        .create_palace(&state.data_root, palace)
        .expect("create palace");

    let body = json!({
        "subject": "trusty-mpm",
        "predicate": "is_fact",
        "object": "z".repeat(crate::prompt_facts::TIER_S_MAX_OBJECT_CHARS + 1),
    });
    let app = router().with_state(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/palaces/httpform/kg")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let bytes = to_bytes(resp.into_body(), 8192).await.unwrap();
    let text = String::from_utf8(bytes.to_vec()).unwrap();
    assert!(text.contains("81 characters"), "got: {text}");
    assert!(text.contains("limit is 80"), "got: {text}");

    let facts = crate::prompt_facts::gather_hot_triples(&state)
        .await
        .expect("gather");
    assert!(facts.is_empty(), "rejected write must not land: {facts:?}");
}

/// Why (#4888): the HTTP KG endpoint must stay usable for ordinary
/// (non-hot) triples regardless of Tier S occupancy — the gate is scoped to
/// the always-injected surface, not to the knowledge graph as a whole.
#[tokio::test]
async fn kg_assert_endpoint_allows_cold_predicates_at_cap() {
    let state = state_with_full_tier_s("httpcold").await;

    let body = json!({
        "subject": "alice",
        "predicate": "works_at",
        "object": "z".repeat(crate::prompt_facts::TIER_S_MAX_OBJECT_CHARS + 40),
    });
    let app = router().with_state(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/palaces/httpcold/kg")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
}
