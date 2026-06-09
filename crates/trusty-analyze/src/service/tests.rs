//! Integration tests for the analyzer HTTP service — health, SSE, facts, SCIP, diagnostics.
//!
//! Why: Extracted from the original `service/mod.rs` tests block. Split into
//! two files at the 500-line cap: this file covers health, SSE, facts CRUD,
//! SCIP ingest, diagnostics, and index proxy tests. Review/webhook/deep-analysis
//! tests live in `service/tests_review.rs`.
//!
//! What: Each test boots the router with a stub `TrustySearchClient` pointing
//! at port 1 (nothing listening), so any test that reaches trusty-search
//! receives a 502.
//!
//! Test: `cargo test -p trusty-analyze` runs all tests in this module.

use std::collections::HashMap;

use axum::body::{to_bytes, Body};
use axum::http::StatusCode;
use axum::http::{Method, Request};
use tempfile::TempDir;
use tower::ServiceExt;

use crate::core::{FactStore, TrustySearchClient};
use crate::service::events::{AnalyzerAppState, AnalyzerEvent};
use crate::service::routes::build_router;
use axum::Router;

pub(crate) fn make_state() -> (AnalyzerAppState, TempDir) {
    let tmp = TempDir::new().unwrap();
    let facts = FactStore::open(&tmp.path().join("facts.redb")).unwrap();
    let search = TrustySearchClient::new("http://127.0.0.1:1");
    (AnalyzerAppState::new(search, facts), tmp)
}

pub(crate) async fn json_get(app: Router, uri: &str) -> (StatusCode, serde_json::Value) {
    let resp = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), 1024 * 1024).await.unwrap();
    let value = if bytes.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, value)
}

#[tokio::test]
async fn health_degraded_when_search_unreachable() {
    // The stub search client points at port 1 (nothing listening).
    // Expect: 503 SERVICE_UNAVAILABLE, status == "degraded",
    // search_reachable == false.
    let (state, _tmp) = make_state();
    let app = build_router(state);
    let (status, body) = json_get(app, "/health").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["status"], "degraded");
    assert_eq!(body["search_reachable"], false);
}

#[tokio::test]
async fn health_response_includes_version() {
    let (state, _tmp) = make_state();
    let app = build_router(state);
    let (_status, body) = json_get(app, "/health").await;
    // Version is always present regardless of search reachability.
    assert!(body["version"].is_string());
    assert!(!body["version"].as_str().unwrap().is_empty());
}

#[tokio::test]
async fn sse_subscriber_receives_emitted_event() {
    // Why: confirms the broadcast wiring is correct end-to-end —
    // subscribe via state.events, emit an event, and verify the
    // receiver gets the same payload.
    let (state, _tmp) = make_state();
    let mut rx = state.events.subscribe();
    state.emit(AnalyzerEvent::FactUpserted {
        subject: "fn auth".into(),
        predicate: "uses".into(),
    });
    let evt = rx
        .recv()
        .await
        .expect("subscriber should receive emitted event");
    match evt {
        AnalyzerEvent::FactUpserted { subject, predicate } => {
            assert_eq!(subject, "fn auth");
            assert_eq!(predicate, "uses");
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

#[tokio::test]
async fn sse_route_returns_event_stream_content_type() {
    // Why: routes should advertise text/event-stream so browsers /
    // clients negotiate the SSE protocol correctly.
    let (state, _tmp) = make_state();
    let app = build_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/sse")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let ct = resp
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(ct.starts_with("text/event-stream"), "got {ct}");
}

#[test]
fn run_diagnostics_blocking_skips_unknown_languages() {
    // Why: a file with no recognized extension must not crash the
    // diagnostics pipeline; it should simply be skipped.
    let mut by_file = HashMap::new();
    by_file.insert("notes.txt".to_string(), "hello world".to_string());
    let diags =
        crate::service::diagnostics_dispatch::run_diagnostics_blocking(by_file, None, None, None);
    assert!(diags.is_empty());
}

#[test]
fn run_diagnostics_blocking_respects_language_filter() {
    // A Rust file filtered to `python` yields nothing even if clippy is
    // installed, because the language filter excludes it.
    let mut by_file = HashMap::new();
    by_file.insert("main.rs".to_string(), "fn main() {}".to_string());
    let diags = crate::service::diagnostics_dispatch::run_diagnostics_blocking(
        by_file,
        Some("python".to_string()),
        None,
        None,
    );
    assert!(diags.is_empty());
}

/// Why: project-scoped tools (e.g. Roslyn) must be completely skipped — not
/// just return empty — when `root_path` is `None`. Previously the test
/// asserted `let _ = diags;` (only checked no panic). This version enforces
/// the contract by injecting a `FakeProjectScopedTool` that records every
/// `run_project` call and asserting the call count is zero.
/// What: builds a `ToolRegistry` containing only `FakeProjectScopedTool`
/// registered under `"csharp"`, passes a `.cs` file with `root_path = None`,
/// and asserts: (a) result is `Ok(vec![])`, (b) `run_project` was never
/// invoked.
/// Test: this test itself.
#[test]
fn run_diagnostics_blocking_project_scoped_skips_when_no_root() {
    use crate::core::tool_registry::ToolRegistry;
    use crate::core::tools::{StaticTool, ToolDiagnostic};
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};

    // A fake project-scoped tool that counts how many times run_project is
    // called, mirroring the FakeAliasedTool pattern in tool_registry tests.
    #[derive(Clone)]
    struct FakeProjectScopedTool {
        call_count: Arc<Mutex<u32>>,
    }
    impl StaticTool for FakeProjectScopedTool {
        fn name(&self) -> &str {
            "fake-project-scoped"
        }
        fn language(&self) -> &str {
            "csharp"
        }
        fn is_available(&self) -> bool {
            true
        }
        fn is_project_scoped(&self) -> bool {
            true
        }
        fn run(&self, _file: &Path, _content: &str) -> anyhow::Result<Vec<ToolDiagnostic>> {
            Ok(Vec::new())
        }
        fn run_project(&self, _files: &[PathBuf]) -> anyhow::Result<Vec<ToolDiagnostic>> {
            *self.call_count.lock().unwrap() += 1;
            Ok(Vec::new())
        }
    }

    let counter = Arc::new(Mutex::new(0u32));
    let tool = FakeProjectScopedTool {
        call_count: Arc::clone(&counter),
    };

    // Build a registry with only our fake tool, bypassing global discovery.
    let registry = ToolRegistry::from_tools_for_test(vec![Arc::new(tool)]);

    let mut by_file = std::collections::HashMap::new();
    by_file.insert("src/Foo.cs".to_string(), "class Foo {}".to_string());

    let diags = crate::service::diagnostics_dispatch::run_diagnostics_blocking_with_registry(
        by_file, None, // language_filter
        None, // tool_filter
        None, // root_path — the None case we are testing
        &registry,
    );

    // Contract: result is empty (no diagnostics produced without a root path).
    assert!(
        diags.is_empty(),
        "expected no diagnostics when root_path is None, got: {diags:?}"
    );
    // Contract: run_project was never called.
    let calls = *counter.lock().unwrap();
    assert_eq!(
        calls, 0,
        "run_project must not be called when root_path is None, was called {calls} times"
    );
}

#[tokio::test]
async fn diagnostics_endpoint_surfaces_search_failure_as_502() {
    // The stub search client is unreachable, so fetching the corpus fails
    // and the endpoint must return a 502 rather than panic.
    let (state, _tmp) = make_state();
    let app = build_router(state);
    let (status, _body) = json_get(app, "/indexes/demo/diagnostics").await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
}

#[tokio::test]
async fn upsert_then_list_facts_round_trip() {
    let (state, _tmp) = make_state();
    let app = build_router(state);

    let body = serde_json::json!({
        "subject": "fn search",
        "predicate": "implements",
        "object": "trait Searcher",
        "index_id": "test"
    });
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/facts")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let (status, listing) = json_get(app, "/facts").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(listing["count"], 1);
}

#[tokio::test]
async fn scip_ingest_accepts_valid_index_and_stores_overlay() {
    use protobuf::{EnumOrUnknown, Message};
    use scip::types::{
        symbol_information::Kind as ScipKind, Document, Index, Occurrence, SymbolInformation,
    };

    let (state, _tmp) = make_state();
    let overlays = state.scip_overlays.clone();
    let app = build_router(state);

    // Build a one-symbol SCIP index.
    let mut sym = SymbolInformation::new();
    sym.symbol = "rust . . hello().".into();
    sym.kind = EnumOrUnknown::new(ScipKind::Function);
    sym.display_name = "hello".into();
    let mut occ = Occurrence::new();
    occ.symbol = sym.symbol.clone();
    occ.symbol_roles = 0x1;
    occ.range = vec![1, 0, 5];
    let mut doc = Document::new();
    doc.relative_path = "src/lib.rs".into();
    doc.language = "rust".into();
    doc.symbols.push(sym);
    doc.occurrences.push(occ);
    let mut index = Index::new();
    index.documents.push(doc);
    let bytes = index.write_to_bytes().expect("encode scip index");

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/indexes/myidx/scip")
                .header("content-type", "application/octet-stream")
                .body(Body::from(bytes))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = to_bytes(resp.into_body(), 1 << 20).await.unwrap();
    let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(parsed["index_id"], "myidx");
    assert_eq!(parsed["documents"], 1);
    assert_eq!(parsed["kg_nodes"], 1);

    // The overlay should be persisted in state.
    let overlays = overlays.read().await;
    let g = overlays.get("myidx").expect("overlay stored");
    assert_eq!(g.node_count(), 1);
    assert_eq!(g.nodes[0].name, "hello");
}

#[tokio::test]
async fn scip_ingest_rejects_garbage_bytes() {
    let (state, _tmp) = make_state();
    let app = build_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/indexes/x/scip")
                .header("content-type", "application/octet-stream")
                .body(Body::from(vec![0xFF, 0xFF, 0xFF, 0xFF]))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn list_indexes_proxies_failure_to_502() {
    // Search daemon at port 1 won't answer — proxy should surface 502.
    let (state, _tmp) = make_state();
    let app = build_router(state);
    let (status, _) = json_get(app, "/indexes").await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
}
