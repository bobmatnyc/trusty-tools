//! Unit tests for the `AnalyzeClient` trait and its response types.
//!
//! Why: split from `analyze_client.rs` to keep that file under the 500-line cap
//! (issue #610).  All tests exercise the parse helpers and the trait shape; no
//! running daemon is required.
//! What: covers `AnalyzeHealthResponse`, `AnalyzeIndexInfo`, `ComplexityHotspot`,
//! `Smell`, and `AnalyzeClientError`.
//!
//! #6287 removed the three `HttpAnalyzeClient` construction tests and the three
//! that drove its transport against a dead port, because the type they
//! constructed is gone (see the note in `analyze_client.rs`). The BEHAVIOUR they
//! covered — graceful degradation on a probe that fails, and hotspots/smells
//! that never block a review — is covered on the implementation that survives,
//! by `subprocess_analyze_client::tests::{subprocess_client_has_analysis_returns_
//! false_on_error, subprocess_client_hotspots_returns_empty,
//! subprocess_client_smells_returns_empty}`. The REV-441 `/quality` invariant
//! moved here rather than going with them.
//!
//! Test: each function is a self-contained unit test.

use super::*;

#[test]
fn analyze_client_trait_object_compiles() {
    fn _accepts_dyn(_c: &dyn AnalyzeClient) {}
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

/// The spec REV-441 invariant: the readiness probe NEVER calls `/quality`.
///
/// Why: the O(corpus) `/quality` endpoint always times out at 5 s and made the
/// sidecar appear perpetually unavailable (lesson §12.3). #6287 deleted
/// `HttpAnalyzeClient`, which is where this guard used to read; the invariant
/// did not go with it, because `SubprocessAnalyzeClient` is now the only
/// implementation that probes anything and it is the one a regression would
/// land in.
/// What: scans the live implementation's source for a string literal naming the
/// `/quality` path in non-comment code. Reading the whole file rather than one
/// function body is stricter than the version it replaces — that one balanced
/// braces to isolate `has_analysis`, and a probe helper moved one function over
/// would have escaped it.
/// Test: this is the test.
#[test]
fn two_step_probe_never_calls_quality() {
    let source = include_str!("subprocess_analyze_client/client.rs");

    // The sentinel is the path fragment followed by a quote or a query marker,
    // which distinguishes a URL literal from prose that talks ABOUT the
    // endpoint. Comment lines are excluded for the same reason.
    let quality_url = source
        .lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .find(|l| l.contains("/quality\"") || l.contains("/quality?"));

    assert!(
        quality_url.is_none(),
        "the readiness probe must NEVER construct a URL to /quality \
         (spec REV-441, lesson §12.3): {quality_url:?}"
    );

    // Guards against the test silently passing if the file were emptied or
    // renamed out from under `include_str!`.
    assert!(
        source.contains("async fn has_analysis"),
        "could not locate has_analysis in subprocess_analyze_client/client.rs — \
         this test no longer reads the implementation it guards"
    );
}
