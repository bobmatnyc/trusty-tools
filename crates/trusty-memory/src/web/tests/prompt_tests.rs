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
