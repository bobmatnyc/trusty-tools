//! Unit tests for `AnalyzeClient` types and `HttpAnalyzeClient`.
//!
//! Why: split from `analyze_client.rs` to keep that file under the 500-line cap
//! (issue #610).  All tests exercise the parse helpers, URL construction, and
//! the two-step probe behaviour; no running daemon is required.
//! What: covers `AnalyzeHealthResponse`, `AnalyzeIndexInfo`, `ComplexityHotspot`,
//! `Smell`, `AnalyzeClientError`, and `HttpAnalyzeClient` construction.
//! Test: each function is a self-contained unit test.

use super::*;

#[test]
fn analyze_client_trait_object_compiles() {
    fn _accepts_dyn(_c: &dyn AnalyzeClient) {}
}

#[test]
fn http_analyze_client_url_is_configurable() {
    let client = HttpAnalyzeClient::new("http://127.0.0.1:7879").expect("TLS init should succeed");
    assert_eq!(client.base_url(), "http://127.0.0.1:7879");
}

#[test]
fn http_analyze_client_strips_trailing_slash() {
    let client = HttpAnalyzeClient::new("http://127.0.0.1:7879/").expect("TLS init should succeed");
    assert_eq!(client.base_url(), "http://127.0.0.1:7879");
}

#[test]
fn http_analyze_client_from_config() {
    let mut config = crate::config::ReviewConfig::load(None);
    config.analyzer_url = "http://localhost:8888".to_string();
    let client = HttpAnalyzeClient::from_config(&config).expect("TLS init should succeed");
    assert_eq!(client.base_url(), "http://localhost:8888");
}

#[test]
fn analyze_health_response_is_healthy() {
    let resp = AnalyzeHealthResponse {
        status: "ok".to_string(),
        search_reachable: true,
    };
    assert!(resp.is_healthy());
}

#[test]
fn analyze_health_response_not_ok() {
    let resp = AnalyzeHealthResponse {
        status: "starting".to_string(),
        search_reachable: false,
    };
    assert!(!resp.is_healthy());
}

#[test]
fn analyze_health_search_not_reachable() {
    // status == "ok" but search_reachable == false → not healthy.
    let resp = AnalyzeHealthResponse {
        status: "ok".to_string(),
        search_reachable: false,
    };
    assert!(
        !resp.is_healthy(),
        "is_healthy must be false when search_reachable is false"
    );
}

#[test]
fn analyze_health_response_deserialises() {
    let json = r#"{"status":"ok","search_reachable":true}"#;
    let resp: AnalyzeHealthResponse = serde_json::from_str(json).unwrap();
    assert!(resp.is_healthy());
}

#[test]
fn analyze_index_info_deserialises() {
    let json = r#"{"id":"main"}"#;
    let info: AnalyzeIndexInfo = serde_json::from_str(json).unwrap();
    assert_eq!(info.id, "main");
}

#[test]
fn hotspot_deserialises() {
    let json = r#"{
        "file": "src/service/mod.rs",
        "function_name": "handle_webhook",
        "cyclomatic": 18,
        "cognitive": 22
    }"#;
    let h: ComplexityHotspot = serde_json::from_str(json).unwrap();
    assert_eq!(h.file, "src/service/mod.rs");
    assert_eq!(h.function_name.as_deref(), Some("handle_webhook"));
    assert_eq!(h.cyclomatic, 18);
}

#[test]
fn smell_deserialises() {
    let json = r#"{"file":"src/main.rs","category":"long_method","severity":"high","line":42}"#;
    let s: Smell = serde_json::from_str(json).unwrap();
    assert_eq!(s.file, "src/main.rs");
    assert_eq!(s.category, "long_method");
    assert_eq!(s.line, Some(42));
}

#[test]
fn analyze_error_display() {
    let err = AnalyzeClientError::Transport("connection refused".to_string());
    assert!(err.to_string().contains("connection refused"));

    let err = AnalyzeClientError::Unavailable("timeout".to_string());
    assert!(err.to_string().contains("timeout"));
}

/// Documents the spec REV-441 invariant: has_analysis NEVER calls /quality.
///
/// Why: the O(corpus) /quality endpoint always times out at 5s and made
/// the sidecar appear perpetually unavailable (lesson §12.3).
/// What: this is a documentation test — the actual enforcement is in the
/// implementation above which calls only /health and /indexes.
/// Test: read `has_analysis` above to verify no call to /quality is present.
#[test]
fn two_step_probe_never_calls_quality() {
    // Search the has_analysis implementation for any URL string that would
    // route to the /quality endpoint.  We locate the has_analysis fn body in
    // the source and scan for string literals containing "/quality".
    //
    // Strategy: find lines that form a URL path to /quality in non-comment
    // code.  The sentinel we look for is a format string or string literal
    // containing `/quality"` (closing quote distinguishes the path literal from
    // documentation strings that talk *about* the endpoint).
    let source = include_str!("analyze_client.rs");

    // Locate the `has_analysis` function body by looking for lines between
    // `async fn has_analysis` and the next top-level `async fn`.
    let in_has_analysis: Vec<&str> = {
        let mut capturing = false;
        let mut brace_depth: i32 = 0;
        let mut lines = Vec::new();
        for line in source.lines() {
            let trimmed = line.trim_start();
            if !capturing && trimmed.contains("async fn has_analysis") {
                capturing = true;
            }
            if capturing {
                lines.push(line);
                brace_depth += line.chars().filter(|&c| c == '{').count() as i32;
                brace_depth -= line.chars().filter(|&c| c == '}').count() as i32;
                if brace_depth <= 0 && lines.len() > 1 {
                    break;
                }
            }
        }
        lines
    };

    // Within the has_analysis body, look for non-comment lines that contain
    // the string literal path `/quality"` (path fragment followed by a quote),
    // which would indicate a URL string targeting the quality endpoint.
    let quality_url_in_body = in_has_analysis
        .iter()
        .filter(|l| !l.trim_start().starts_with("//"))
        .any(|l| l.contains("/quality\"") || l.contains("/quality?"));

    assert!(
        !quality_url_in_body,
        "has_analysis must NEVER construct a URL to /quality (spec REV-441, lesson §12.3)"
    );

    // Also verify we actually found the function body (guards against the test
    // silently passing if the function was renamed).
    assert!(
        !in_has_analysis.is_empty(),
        "could not locate has_analysis fn body in analyze_client.rs — test is broken"
    );
}

#[tokio::test]
async fn two_step_probe_returns_false_on_transport_error() {
    // Port 1 is always refused; has_analysis must return false (not panic).
    let client = HttpAnalyzeClient::new("http://127.0.0.1:1").expect("TLS init should succeed");
    let result = client.has_analysis("main").await;
    assert!(
        !result,
        "has_analysis must return false on transport error, not panic"
    );
}

#[tokio::test]
async fn complexity_hotspots_transport_error_propagates() {
    let client = HttpAnalyzeClient::new("http://127.0.0.1:1").expect("TLS init should succeed");
    let result = client.complexity_hotspots("main", Some(5)).await;
    assert!(
        result.is_err(),
        "transport error must surface as Err from complexity_hotspots"
    );
}

#[tokio::test]
async fn smells_transport_error_propagates() {
    let client = HttpAnalyzeClient::new("http://127.0.0.1:1").expect("TLS init should succeed");
    let result = client.smells("main").await;
    assert!(
        result.is_err(),
        "transport error must surface as Err from smells"
    );
}
