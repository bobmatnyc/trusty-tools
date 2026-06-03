//! Unit tests for `SearchClient` types and `HttpSearchClient`.
//!
//! Why: split from `search_client.rs` to keep that file under the 500-line cap
//! (issue #610).  All tests exercise the parse helpers and URL construction; no
//! running daemon is required.
//! What: covers `IndexInfo`, `SearchResult`, `SearchRequest`, `SearchResponse`,
//! `ListIndexesResponse` (envelope regression #672), `SearchClientError`, and
//! `HttpSearchClient` construction.
//! Test: each function is a self-contained unit test.

use super::{
    HttpSearchClient, ListIndexesResponse, SearchClient, SearchClientError, SearchRequest,
    SearchResponse, SearchResult,
};
use crate::integrations::search_client::IndexInfo;

#[test]
fn search_client_trait_object_compiles() {
    // This test just needs to compile; the coercion proves SearchClient is
    // object-safe.
    fn _accepts_dyn(_c: &dyn super::SearchClient) {}
}

#[test]
fn http_search_client_url_is_configurable() {
    let client = HttpSearchClient::new("http://127.0.0.1:7878");
    assert_eq!(client.base_url(), "http://127.0.0.1:7878");
}

#[test]
fn http_search_client_strips_trailing_slash() {
    let client = HttpSearchClient::new("http://127.0.0.1:7878/");
    // Trailing slash must be removed to prevent double-slash paths.
    assert_eq!(client.base_url(), "http://127.0.0.1:7878");
}

#[test]
fn http_search_client_from_config() {
    let mut config = crate::config::ReviewConfig::load(None);
    config.search_url = "http://localhost:9999".to_string();
    let client = HttpSearchClient::from_config(&config);
    assert_eq!(client.base_url(), "http://localhost:9999");
}

#[test]
fn index_info_deserialises() {
    let json = r#"{"id":"main","name":"trusty-tools","root_path":"/home/user/trusty-tools"}"#;
    let info: IndexInfo = serde_json::from_str(json).unwrap();
    assert_eq!(info.id, "main");
    assert_eq!(info.name.as_deref(), Some("trusty-tools"));
}

#[test]
fn search_result_deserialises() {
    let json = r#"{
        "file": "src/lib.rs",
        "snippet": "pub fn authenticate() {",
        "score": 0.92,
        "start_line": 42,
        "end_line": 58
    }"#;
    let result: SearchResult = serde_json::from_str(json).unwrap();
    assert_eq!(result.file, "src/lib.rs");
    assert_eq!(result.snippet.as_deref(), Some("pub fn authenticate() {"));
    assert!((result.score - 0.92_f32).abs() < 1e-5);
    assert_eq!(result.start_line, Some(42));
    assert_eq!(result.end_line, Some(58));
}

#[test]
fn search_result_missing_optional_fields() {
    let json = r#"{"file":"src/main.rs"}"#;
    let result: SearchResult = serde_json::from_str(json).unwrap();
    assert_eq!(result.file, "src/main.rs");
    assert!(result.snippet.is_none());
    assert!((result.score - 0.0_f32).abs() < 1e-10);
}

/// Verify `SearchRequest` serialises with the correct `text` field name.
///
/// Why: trusty-search's `SearchQuery` expects `text` (not `query`); the
/// wrong field name causes a 422 "missing field `text`" and disables context
/// retrieval for every review.  This regression test pins the wire name.
/// What: serialises a `SearchRequest` and asserts the JSON key is `"text"`,
/// not `"query"`.
/// Test: this test itself; no network.
#[test]
fn search_request_body_uses_text_field() {
    let req = SearchRequest {
        text: "fn authenticate".to_string(),
        top_k: Some(10),
    };
    let json = serde_json::to_string(&req).unwrap();
    // The wire field MUST be "text" — trusty-search rejects "query" with 422.
    assert!(
        json.contains("\"text\""),
        "SearchRequest must use 'text' field name, got: {json}"
    );
    assert!(
        !json.contains("\"query\""),
        "SearchRequest must NOT use 'query' field name, got: {json}"
    );
    assert!(json.contains("fn authenticate"));
    assert!(json.contains("10"));
}

#[test]
fn search_request_omits_none_top_k() {
    let req = SearchRequest {
        text: "async fn".to_string(),
        top_k: None,
    };
    let json = serde_json::to_string(&req).unwrap();
    assert!(!json.contains("top_k"));
}

#[test]
fn search_response_deserialises() {
    let json = r#"{"results":[{"file":"a.rs","score":0.5},{"file":"b.rs","score":0.3}]}"#;
    let resp: SearchResponse = serde_json::from_str(json).unwrap();
    assert_eq!(resp.results.len(), 2);
    assert_eq!(resp.results[0].file, "a.rs");
}

/// Regression guard for the `{"indexes":[...]}` envelope bug (issue #672).
///
/// Why: `list_indexes` previously tried to deserialise `Vec<IndexInfo>`
/// directly from the daemon body.  The daemon always returns an object
/// envelope `{"indexes":[...]}`, which causes
/// `invalid type: map, expected a sequence` and silently falls back to
/// `"main"`.  This test pins the correct parse path so that regression is
/// caught immediately without a running daemon.
/// What: deserialises the real daemon envelope JSON through
/// `ListIndexesResponse` and asserts `id` and `root_path` round-trip
/// correctly.
/// Test: this test; no network required.
#[test]
fn list_indexes_parses_daemon_envelope() {
    // This is the EXACT shape the trusty-search daemon returns.
    let body = r#"{"indexes":[{"id":"trusty-tools","root_path":"/Volumes/SSD1/Projects/trusty-tools","size_bytes":123}]}"#;
    let envelope: ListIndexesResponse =
        serde_json::from_str(body).expect("must parse the daemon envelope without error");
    assert_eq!(
        envelope.indexes.len(),
        1,
        "must yield exactly one IndexInfo"
    );
    let info = &envelope.indexes[0];
    assert_eq!(info.id, "trusty-tools");
    assert_eq!(
        info.root_path.as_deref(),
        Some("/Volumes/SSD1/Projects/trusty-tools"),
        "root_path must survive the envelope unwrap"
    );
}

/// Regression guard: a bare array (legacy shape) must fail gracefully, not panic.
///
/// Why: documents that the parse expects the envelope shape; if the daemon
/// ever regresses to a bare array we get a clear `Parse` error, not a panic.
/// What: feeds a bare JSON array to `ListIndexesResponse`, asserts it errors.
/// Test: this test.
#[test]
fn list_indexes_bare_array_is_rejected() {
    let body = r#"[{"id":"trusty-tools","root_path":"/foo","size_bytes":0}]"#;
    let result: Result<ListIndexesResponse, _> = serde_json::from_str(body);
    assert!(
        result.is_err(),
        "bare array must not parse as ListIndexesResponse — envelope is required"
    );
}

#[test]
fn search_error_display() {
    let err = SearchClientError::Transport("connection refused".to_string());
    assert!(err.to_string().contains("connection refused"));

    let err = SearchClientError::Api {
        status: 503,
        body: "overloaded".to_string(),
    };
    let s = err.to_string();
    assert!(s.contains("503"));
    assert!(s.contains("overloaded"));
}

#[tokio::test]
async fn health_check_transport_error_on_unreachable() {
    // Port 1 is always refused; this verifies graceful transport error handling.
    let client = HttpSearchClient::new("http://127.0.0.1:1");
    let result = client.health().await;
    assert!(
        result.is_err(),
        "unreachable host must return an error, not panic"
    );
    match result.unwrap_err() {
        SearchClientError::Unavailable(_) => {}
        SearchClientError::Transport(_) => {}
        other => panic!("expected Unavailable or Transport, got {other:?}"),
    }
}

#[tokio::test]
async fn search_transport_error_on_unreachable() {
    let client = HttpSearchClient::new("http://127.0.0.1:1");
    let result = client.search("main", "fn auth", Some(5)).await;
    assert!(
        result.is_err(),
        "unreachable host must return an error, not panic"
    );
}
