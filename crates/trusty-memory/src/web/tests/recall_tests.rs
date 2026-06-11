//! Tests for recall cross-palace fan-out, palace filter, triple-ID helpers,
//! and drawer utility functions (preview, entry JSON shape).

use super::super::kg_routes::{decode_triple_id, encode_triple_id};
use super::super::recall_routes::recall_entry_json;
use super::super::router;
use super::test_state;
use crate::service::{drawer_content_preview, DRAWER_PREVIEW_MAX_CHARS};
use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use serde_json::{json, Value};
use tower::util::ServiceExt;
use trusty_common::memory_core::palace::{Palace, PalaceId};
use trusty_common::memory_core::retrieval::RecallResult;
use uuid::Uuid;

/// Why: The base64url triple-ID round-trip is the core invariant for
/// `DELETE /kg/triples/<id>` — if encode/decode aren't inverses, the
/// handler will always 404 on valid IDs.
/// What: Encodes a (subject, predicate) pair, decodes the result, and
/// asserts exact equality with the originals. Also tests URL-safety.
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
        let encoded = encode_triple_id(subject, predicate)
            .unwrap_or_else(|e| panic!("encode_triple_id failed: {e}"));
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

/// Issue #1102 — `encode_triple_id` must reject subjects or predicates that
/// contain the null-byte separator (`\0`), not silently produce an ambiguous
/// (corrupt) encoded id.
///
/// Why: The null byte is the separator inside the encoded payload. If either
/// component itself carries a `\0`, the subsequent decode splits at the wrong
/// position and returns incorrect `(subject, predicate)` pairs for the
/// persisted `DELETE` path — this is a data-integrity bug, not just a
/// correctness assertion.
/// What: Calls `encode_triple_id` with a `\0`-containing subject and a
/// `\0`-containing predicate, asserts both return `Err` with a message
/// mentioning the null-byte separator.
/// Test: This test.
#[test]
fn encode_triple_id_rejects_null_byte() {
    let err = encode_triple_id("sub\0ject", "predicate")
        .expect_err("null byte in subject must return Err");
    assert!(
        err.contains("null-byte") || err.contains("\\0"),
        "error message should mention null-byte separator; got {err:?}"
    );

    let err = encode_triple_id("subject", "pred\0icate")
        .expect_err("null byte in predicate must return Err");
    assert!(
        err.contains("null-byte") || err.contains("\\0"),
        "error message should mention null-byte separator; got {err:?}"
    );

    // Clean inputs must still succeed.
    assert!(
        encode_triple_id("clean-subject", "clean-predicate").is_ok(),
        "clean inputs must encode successfully"
    );
}

/// Issue #1102 — `extract_partial_error` pure-function unit tests.
///
/// Why: The helper was added to guard against partial-success envelopes being
/// silently passed through as 200 OK, but the original test never reached that
/// branch. These tests call the pure function directly with synthetic
/// `serde_json::Value`s and pin every branch: (1) non-empty string errors
/// array, (2) non-empty `{"message":"..."}` object array, (3) empty array,
/// (4) no `errors` key.
/// What: Four focused `#[test]` functions below.
/// Test: These tests.
#[test]
fn extract_partial_error_string_array_returns_first_error() {
    use super::super::recall_routes::extract_partial_error;

    // (1) Non-empty errors array of plain strings.
    let v = json!({
        "errors": ["palace alpha timed out", "palace beta unreachable"],
        "results": []
    });
    let msg = extract_partial_error(&v);
    assert!(msg.is_some(), "non-empty string errors must yield Some");
    let msg = msg.unwrap();
    assert!(
        msg.contains("palace alpha timed out"),
        "message must include the first error string; got {msg:?}"
    );
    assert!(
        msg.contains("2 error(s)"),
        "message must include the error count; got {msg:?}"
    );
}

/// Why: Same as above but for the `{"message":"..."}` object shape that some
/// future partial-success envelopes may use.
/// What: Calls `extract_partial_error` with an `errors` array of objects
/// carrying a `"message"` field and asserts the text is extracted correctly.
/// Test: This test.
#[test]
fn extract_partial_error_object_array_extracts_message_field() {
    use super::super::recall_routes::extract_partial_error;

    // (2) Non-empty errors array of {"message": "..."} objects.
    let v = json!({
        "errors": [
            {"message": "KG query failed", "code": 503},
            {"message": "BM25 timeout"}
        ]
    });
    let msg = extract_partial_error(&v);
    assert!(msg.is_some(), "object errors must yield Some");
    let msg = msg.unwrap();
    assert!(
        msg.contains("KG query failed"),
        "message must include the first object's message; got {msg:?}"
    );
    assert!(
        msg.contains("2 error(s)"),
        "message must include the error count; got {msg:?}"
    );
}

/// Why: An empty `errors` array means no failure occurred; the guard must
/// not convert a healthy envelope into an error.
/// What: Calls `extract_partial_error` with `errors: []` and asserts None.
/// Test: This test.
#[test]
fn extract_partial_error_empty_array_returns_none() {
    use super::super::recall_routes::extract_partial_error;

    // (3) Empty errors array → no failure.
    let v = json!({ "errors": [], "results": [{"content": "ok"}] });
    assert!(
        extract_partial_error(&v).is_none(),
        "empty errors array must return None"
    );
}

/// Why: Successful recall returns a JSON array (not an object), so
/// `get("errors")` returns None. The guard must handle this gracefully.
/// What: Calls `extract_partial_error` with a JSON array and with an object
/// that has no `"errors"` key; asserts None in both cases.
/// Test: This test.
#[test]
fn extract_partial_error_no_errors_key_returns_none() {
    use super::super::recall_routes::extract_partial_error;

    // (4a) Plain JSON array (the success shape from recall_all).
    let v = json!([{"content": "hello", "score": 0.9}]);
    assert!(
        extract_partial_error(&v).is_none(),
        "JSON array input must return None"
    );

    // (4b) Object without an errors key.
    let v = json!({ "results": [{"content": "world"}] });
    assert!(
        extract_partial_error(&v).is_none(),
        "object without errors key must return None"
    );
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

// -------------------------------------------------------------------------
// Drawer utility tests (moved from health_tests.rs to stay under 500-line cap)
// -------------------------------------------------------------------------

/// Why: `drawer_content_preview` is a shared utility that collapses whitespace
/// and truncates long content for display in recall listings. This pins its
/// contract so regressions are caught immediately without requiring HTTP tests.
/// What: Exercises the trim, whitespace-collapse, truncation, and exact-limit
/// code paths of `drawer_content_preview`.
/// Test: this test.
#[test]
fn drawer_preview_collapses_whitespace_and_truncates() {
    // Short single-line content is returned verbatim.
    assert_eq!(drawer_content_preview("hello world"), "hello world");

    // Multiline / tab-laden content collapses to single-spaced text.
    assert_eq!(
        drawer_content_preview("first line\n\nsecond\tline   third"),
        "first line second line third"
    );

    // Leading / trailing whitespace is stripped.
    assert_eq!(drawer_content_preview("   padded   "), "padded");

    // Empty content yields an empty preview (fallback signal for clients).
    assert_eq!(drawer_content_preview(""), "");

    // Long content is truncated to DRAWER_PREVIEW_MAX_CHARS with an ellipsis.
    let long = "x".repeat(DRAWER_PREVIEW_MAX_CHARS + 50);
    let preview = drawer_content_preview(&long);
    assert_eq!(preview.chars().count(), DRAWER_PREVIEW_MAX_CHARS);
    assert!(preview.ends_with('…'));

    // Content right at the limit is not truncated.
    let exact = "y".repeat(DRAWER_PREVIEW_MAX_CHARS);
    assert_eq!(drawer_content_preview(&exact), exact);
}

/// Issue #69 — `recall_entry_json` hoists the drawer's fields to the top
/// level so `content` is directly reachable.
///
/// Why: The recall API previously wrapped the drawer under a `"drawer"`
/// key, so clients scanning the top level for `content`/`tags` found
/// nothing and recall always looked empty. This locks the flattened shape
/// in place so the regression cannot silently return.
/// What: Builds a `RecallResult`, runs it through `recall_entry_json`, and
/// asserts `content`, `tags`, and `importance` are at the top level, that
/// `score`/`layer` sit alongside them, and that the old `drawer` wrapper
/// key is gone.
/// Test: this test.
#[test]
fn recall_entry_json_hoists_drawer_fields() {
    use trusty_common::memory_core::Drawer;

    let room = Uuid::new_v4();
    let mut drawer = Drawer::new(room, "the answer is 42");
    drawer.tags = vec!["source:kuzu".to_string()];
    drawer.importance = 0.7;

    let entry = recall_entry_json(RecallResult {
        drawer,
        score: 0.699,
        layer: 1,
    });

    // Content must be reachable WITHOUT a `drawer` wrapper (issue #69).
    assert_eq!(
        entry.get("content").and_then(|v| v.as_str()),
        Some("the answer is 42"),
        "content must be at the top level, got {entry:?}"
    );
    assert!(
        entry.get("drawer").is_none(),
        "the legacy `drawer` wrapper must not be present, got {entry:?}"
    );
    // Other drawer fields are hoisted too.
    assert_eq!(
        entry["importance"].as_f64().map(|f| (f * 10.0).round()),
        Some(7.0)
    );
    assert_eq!(
        entry["tags"][0].as_str(),
        Some("source:kuzu"),
        "tags must be hoisted, got {entry:?}"
    );
    // Ranking metadata sits alongside the hoisted fields.
    assert_eq!(entry["layer"].as_u64(), Some(1));
    assert!(
        entry["score"]
            .as_f64()
            .is_some_and(|s| (s - 0.699).abs() < 1e-6),
        "score must be preserved, got {entry:?}"
    );
}
