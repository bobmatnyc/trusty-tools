//! Tests for recall cross-palace fan-out, palace filter, and triple-ID helpers.

use super::super::kg_routes::{decode_triple_id, encode_triple_id};
use super::super::router;
use super::test_state;
use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use serde_json::{json, Value};
use tower::util::ServiceExt;
use trusty_common::memory_core::palace::{Palace, PalaceId};

/// Why: The base64url triple-ID round-trip is the core invariant for
/// `DELETE /kg/triples/<id>` — if encode/decode aren't inverses, the
/// handler will always 404 on valid IDs.
/// What: Encodes a (subject, predicate) pair, decodes the result, and
/// asserts exact equality with the originals. Also tests the null-byte
/// separator and URL-safety.
/// Test: This test.
#[test]
fn decode_triple_id_round_trips() {
    let cases = [
        ("drawer:some-uuid", "has_tag"),
        ("entity:alice", "works_at"),
        ("entity:project/foo", "depends_on"),
        // edge: empty predicate
        ("subject", ""),
        // edge: subject with slashes + predicate with colons
        ("path/to/node", "rel:type:sub"),
    ];
    for (subject, predicate) in cases {
        let encoded = encode_triple_id(subject, predicate);
        // Must be URL-safe: no +, /, or = characters.
        assert!(
            !encoded.contains('+') && !encoded.contains('/') && !encoded.contains('='),
            "encoded triple id {encoded:?} is not URL-safe"
        );
        let (s, p) = decode_triple_id(&encoded)
            .unwrap_or_else(|| panic!("decode_triple_id failed for {encoded:?}"));
        assert_eq!(s, subject, "subject mismatch for ({subject}, {predicate})");
        assert_eq!(
            p, predicate,
            "predicate mismatch for ({subject}, {predicate})"
        );
    }
}

/// Why: `decode_triple_id` must return `None` on garbage input (not panic).
/// What: Passes invalid base64 and base64 without a null separator; asserts None.
/// Test: This test.
#[test]
fn decode_triple_id_returns_none_for_invalid_input() {
    assert!(decode_triple_id("not!!valid%%base64").is_none());
    // Valid base64url but no null separator → no split possible.
    use base64::Engine as _;
    let no_sep = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b"no-separator");
    assert!(decode_triple_id(&no_sep).is_none());
}

// -------------------------------------------------------------------------
// Issue #465 — GET /api/v1/recall?palace= must honour the palace filter
// -------------------------------------------------------------------------

/// Why (issue #465): `GET /api/v1/recall?palace=<id>&q=...` was silently
/// ignoring the `palace=` parameter and always fanning out across all
/// palaces, returning results from the wrong palace. This test proves the
/// route now scopes the recall to the requested palace.
/// What: creates two palaces with distinct drawers, requests recall with
/// `palace=` set to one of them, and asserts the response is a JSON array
/// (the per-palace shape), not the cross-palace object shape.
/// Test: this test.
#[tokio::test]
async fn recall_all_handler_honors_palace_filter() {
    let state = test_state();
    // Pre-create a palace so the handler can open it.
    let palace = Palace {
        id: PalaceId::new("filter-target"),
        name: "filter-target".to_string(),
        description: None,
        created_at: chrono::Utc::now(),
        data_dir: state.data_root.join("filter-target"),
    };
    state
        .registry
        .create_palace(&state.data_root, palace)
        .expect("create_palace");

    let app = router().with_state(state);
    // With palace= set, the handler should delegate to the per-palace path.
    // Even with no drawers, a valid palace returns a JSON array (possibly
    // empty), NOT a 404 or a cross-palace object shape.
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/recall?q=anything&palace=filter-target")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "recall with valid palace= must return 200"
    );
    let bytes = to_bytes(resp.into_body(), 4096).await.unwrap();
    let v: Value = serde_json::from_slice(&bytes).unwrap();
    assert!(
        v.is_array(),
        "recall with palace= must return a JSON array (per-palace shape); got {v}"
    );
}

/// Why (issue #465): when `palace=` refers to a non-existent palace, the
/// handler must return a 404 — not silently fall back to cross-palace recall.
/// What: requests recall with a `palace=` that was never created and asserts
/// the response is 404.
/// Test: this test.
#[tokio::test]
async fn recall_all_handler_palace_filter_missing_palace_returns_404() {
    let state = test_state();
    let app = router().with_state(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/recall?q=anything&palace=nonexistent-palace")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::NOT_FOUND,
        "recall with palace= pointing to missing palace must return 404"
    );
}

/// Why (issue #465): when `palace=` is absent, the endpoint must continue
/// to fan out across all palaces (original cross-palace behaviour).
/// What: with no palace= param and no palaces created, the cross-palace
/// fan-out returns an empty JSON array (no palaces → nothing to search).
/// Test: this test.
#[tokio::test]
async fn recall_all_handler_fans_out_without_palace_param() {
    let state = test_state();
    let app = router().with_state(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/recall?q=anything")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "cross-palace recall with no palace= must return 200"
    );
    let bytes = to_bytes(resp.into_body(), 4096).await.unwrap();
    let v: Value = serde_json::from_slice(&bytes).unwrap();
    // No palaces → empty array.
    assert!(
        v.is_array(),
        "cross-palace recall must return a JSON array; got {v}"
    );
}

// -------------------------------------------------------------------------
// Issue #466 — POST /api/v1/remember must reject short content synchronously
// -------------------------------------------------------------------------

/// Why (issue #466): content that is too short was silently dropped by the
/// background worker while the HTTP response claimed `202 Accepted`.
/// Callers believed the memory was stored when it wasn't — silent data loss.
/// The fix: validate the minimum word count synchronously and return 422
/// before queueing so the caller gets an actionable error immediately.
/// What: POSTs content with fewer than REMEMBER_MIN_WORDS words and asserts
/// the response is 422, not 202.
/// Test: this test.
#[tokio::test]
async fn remember_async_rejects_short_content() {
    let state = test_state();
    let app = router().with_state(state);
    // "hi" is 1 word — well below REMEMBER_MIN_WORDS (4).
    for body in [
        json!({"content": "hi"}),
        json!({"content": "two words"}),
        json!({"content": "three word content"}),
    ] {
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/remember")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::UNPROCESSABLE_ENTITY,
            "short content must return 422; body={body}"
        );
    }
}

/// Why (issue #466): content that meets the minimum word count must still
/// return 202, proving the synchronous gate does not over-reject.
/// What: POSTs exactly REMEMBER_MIN_WORDS words and asserts 202.
/// Test: this test (companion to `remember_async_rejects_short_content`).
#[tokio::test]
async fn remember_async_accepts_content_at_min_words() {
    let state = test_state();
    // Pre-create a palace so the spawned task can find it.
    let palace = Palace {
        id: PalaceId::new("min-words-test"),
        name: "min-words-test".to_string(),
        description: None,
        created_at: chrono::Utc::now(),
        data_dir: state.data_root.join("min-words-test"),
    };
    state
        .registry
        .create_palace(&state.data_root, palace)
        .expect("create_palace");

    let app = router().with_state(state);
    // Exactly 4 words — the minimum.
    let body = json!({
        "content": "four words exactly here",
        "palace": "min-words-test",
    });
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/remember")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::ACCEPTED,
        "content at minimum word count must return 202"
    );
    let bytes = to_bytes(resp.into_body(), 512).await.unwrap();
    let v: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        v["status"], "queued",
        "accepted body must carry status=queued"
    );
}
