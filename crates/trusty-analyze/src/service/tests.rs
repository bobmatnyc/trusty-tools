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

use crate::core::{FactStore, ScipOverlayStore, TrustySearchClient};
use crate::service::events::{AnalyzerAppState, AnalyzerEvent};
use crate::service::routes::build_router;
use axum::Router;

pub(crate) fn make_state() -> (AnalyzerAppState, TempDir) {
    let tmp = TempDir::new().unwrap();
    let state = state_in(tmp.path());
    (state, tmp)
}

/// Build state whose redb stores live under `dir`.
///
/// Why (#5049): proving the SCIP overlay survives a restart needs two
/// `AnalyzerAppState`s built over the *same* data directory — the in-process
/// stand-in for stopping and restarting the daemon. `make_state` hides its
/// `TempDir`, so it cannot express that.
/// What: opens both redb stores under `dir` and returns state around a search
/// client pointed at port 1 (nothing listening), matching `make_state`.
/// Test: used by `scip_overlay_survives_state_rebuild`.
pub(crate) fn state_in(dir: &std::path::Path) -> AnalyzerAppState {
    state_in_with_search(dir, "http://127.0.0.1:1")
}

/// `state_in`, but pointed at an arbitrary trusty-search base URL.
///
/// Why (#5049): `/indexes/{id}/graph` fetches chunks before it ever reads the
/// overlay, so with the unreachable default client every graph request 502s
/// and the overlay-merge path is untestable. Tests that need `/graph` point
/// this at a local stub instead.
/// What: same two redb stores under `dir`, with `search_base` as the search
/// client's base URL.
/// Test: used by `graph_marks_scip_overlay_present_after_ingest` and
/// `graph_marks_scip_overlay_absent_without_ingest`.
pub(crate) fn state_in_with_search(dir: &std::path::Path, search_base: &str) -> AnalyzerAppState {
    let facts = FactStore::open(&dir.join("facts.redb")).unwrap();
    let overlays = ScipOverlayStore::open(&dir.join("scip_overlays.redb")).unwrap();
    let search = TrustySearchClient::new(search_base);
    AnalyzerAppState::new(search, facts, overlays)
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

/// Why (#3304): `POST /facts` is a destructive write; a malicious cross-origin
/// page must be rejected with `403` by the router-wide same-origin guard before
/// the handler runs (CSRF defence).
/// Test: this test.
#[tokio::test]
async fn write_route_rejects_cross_origin() {
    let (state, _tmp) = make_state();
    let app = build_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/facts")
                .header("origin", "http://evil.example.com")
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "cross-origin POST /facts must be rejected by the write guard"
    );
}

/// Why (#3304): the analyzer's own loopback-served UI and server-side callers
/// (the console proxy / `curl` / the GitHub webhook, which send NO `Origin`)
/// must keep driving writes — loopback and missing-Origin writes must NOT 403.
/// Test: this test.
#[tokio::test]
async fn write_route_allows_loopback_and_missing_origin() {
    let (state, _tmp) = make_state();
    let app = build_router(state);
    // Loopback origin → allowed.
    let loopback = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/facts")
                .header("origin", "http://127.0.0.1:7799")
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_ne!(
        loopback.status(),
        StatusCode::FORBIDDEN,
        "loopback-origin POST /facts must pass the guard"
    );
    // No Origin (server-side caller) → allowed.
    let missing = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/facts")
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_ne!(
        missing.status(),
        StatusCode::FORBIDDEN,
        "missing-Origin POST /facts (server-side caller) must pass the guard"
    );
}

/// Why (#3304): the guard is method-gated — a cross-origin GET read leaks no
/// destructive capability and must NOT be blocked.
/// Test: this test.
#[tokio::test]
async fn read_route_allows_cross_origin() {
    let (state, _tmp) = make_state();
    let app = build_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/health")
                .header("origin", "http://evil.example.com")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_ne!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "cross-origin GET /health must not be blocked by the write guard"
    );
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
    let report =
        crate::service::diagnostics_dispatch::run_diagnostics_blocking(by_file, None, None, None);
    assert!(report.diagnostics.is_empty());
    // tools_run must be empty when no language-matched tools ran.
    assert!(report.tools_run.is_empty());
}

#[test]
fn run_diagnostics_blocking_respects_language_filter() {
    // A Rust file filtered to `python` yields nothing even if clippy is
    // installed, because the language filter excludes it.
    let mut by_file = HashMap::new();
    by_file.insert("main.rs".to_string(), "fn main() {}".to_string());
    let report = crate::service::diagnostics_dispatch::run_diagnostics_blocking(
        by_file,
        Some("python".to_string()),
        None,
        None,
    );
    assert!(report.diagnostics.is_empty());
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

    let report = crate::service::diagnostics_dispatch::run_diagnostics_blocking_with_registry(
        by_file, None, // language_filter
        None, // tool_filter
        None, // root_path — the None case we are testing
        &registry,
    );

    // Contract: result is empty (no diagnostics produced without a root path).
    assert!(
        report.diagnostics.is_empty(),
        "expected no diagnostics when root_path is None, got: {:?}",
        report.diagnostics
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

/// Encode a SCIP `Index` protobuf carrying `symbol_names.len()` function
/// symbols in `src/lib.rs`. An empty slice yields a valid, symbol-free index.
///
/// Why (#5049): the empty-vs-absent distinction needs a SCIP payload that
/// legitimately produces zero KG nodes, so the ingest path can be driven with
/// both shapes from one helper.
/// What: builds one `Document` with one `SymbolInformation`/`Occurrence` pair
/// per name and returns the encoded bytes.
/// Test: used by `scip_ingest_accepts_valid_index_and_stores_overlay`,
/// `scip_overlay_survives_state_rebuild`,
/// `empty_scip_ingest_is_distinguishable_from_no_ingest`.
pub(crate) fn scip_index_bytes(symbol_names: &[&str]) -> Vec<u8> {
    use protobuf::{EnumOrUnknown, Message};
    use scip::types::{
        symbol_information::Kind as ScipKind, Document, Index, Occurrence, SymbolInformation,
    };

    let mut doc = Document::new();
    doc.relative_path = "src/lib.rs".into();
    doc.language = "rust".into();
    for (i, name) in symbol_names.iter().enumerate() {
        let mut sym = SymbolInformation::new();
        sym.symbol = format!("rust . . {name}().");
        sym.kind = EnumOrUnknown::new(ScipKind::Function);
        sym.display_name = (*name).into();
        let mut occ = Occurrence::new();
        occ.symbol = sym.symbol.clone();
        occ.symbol_roles = 0x1;
        occ.range = vec![i as i32 + 1, 0, 5];
        doc.symbols.push(sym);
        doc.occurrences.push(occ);
    }
    let mut index = Index::new();
    index.documents.push(doc);
    index.write_to_bytes().expect("encode scip index")
}

async fn post_scip(
    app: &Router,
    index_id: &str,
    bytes: Vec<u8>,
) -> (StatusCode, serde_json::Value) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri(format!("/indexes/{index_id}/scip"))
                .header("content-type", "application/octet-stream")
                .body(Body::from(bytes))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let body = to_bytes(resp.into_body(), 1 << 20).await.unwrap();
    let parsed = if body.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&body).unwrap()
    };
    (status, parsed)
}

#[tokio::test]
async fn scip_ingest_accepts_valid_index_and_stores_overlay() {
    let (state, _tmp) = make_state();
    let overlays = state.scip_overlays.clone();
    let app = build_router(state);

    let (status, parsed) = post_scip(&app, "myidx", scip_index_bytes(&["hello"])).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(parsed["index_id"], "myidx");
    assert_eq!(parsed["documents"], 1);
    assert_eq!(parsed["kg_nodes"], 1);

    // The overlay should be persisted in the durable store.
    let rec = overlays.get("myidx").unwrap().expect("overlay stored");
    assert_eq!(rec.graph.node_count(), 1);
    assert_eq!(rec.graph.nodes[0].name, "hello");
}

/// Why (#5049): the defect. `POST /indexes/{id}/scip` answered 200 while
/// writing only to an in-process `HashMap`, so a daemon restart discarded the
/// ingest and every later `/graph` silently served a tree-sitter-only graph.
/// What: ingests a SCIP index through one router, drops that state entirely,
/// rebuilds `AnalyzerAppState` over the SAME data directory (the in-process
/// stand-in for a restart), and asserts the second daemon still reports the
/// overlay. Fails against the pre-fix commit, where the rebuilt state starts
/// with an empty map.
/// Test: this test.
#[tokio::test]
async fn scip_overlay_survives_state_rebuild() {
    let tmp = TempDir::new().unwrap();

    {
        let app = build_router(state_in(tmp.path()));
        let (status, parsed) = post_scip(&app, "myidx", scip_index_bytes(&["hello"])).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(parsed["kg_nodes"], 1);
    }

    // Second "daemon boot" over the same data directory.
    let app = build_router(state_in(tmp.path()));
    let (status, body) = json_get(app, "/indexes/myidx/scip").await;
    assert_eq!(
        status,
        StatusCode::OK,
        "SCIP overlay must survive a daemon restart; got {body}"
    );
    assert_eq!(body["index_id"], "myidx");
    assert_eq!(body["nodes"], 1);
}

/// Why (#5049): persistence stops the loss but not the silence — a caller
/// still has to tell "nobody ingested SCIP here" from "an ingested SCIP index
/// had no symbols". Both contribute zero nodes to `/graph`.
/// What: never-ingested index → 404; index ingested with a symbol-free SCIP
/// payload → 200 with `nodes: 0`.
/// Test: this test.
#[tokio::test]
async fn empty_scip_ingest_is_distinguishable_from_no_ingest() {
    let (state, _tmp) = make_state();
    let app = build_router(state);

    let (status, _) = post_scip(&app, "empty-idx", scip_index_bytes(&[])).await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = json_get(app.clone(), "/indexes/empty-idx/scip").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["nodes"], 0, "ingested-but-empty overlay is present");

    let (status, body) = json_get(app, "/indexes/never-idx/scip").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(
        body["error"].as_str().unwrap().contains("never-idx"),
        "404 body must name the index: {body}"
    );
}

#[tokio::test]
async fn scip_overlay_status_404_when_never_ingested() {
    let (state, _tmp) = make_state();
    let app = build_router(state);
    let (status, _) = json_get(app, "/indexes/nothing-here/scip").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// Stand-in trusty-search that answers every chunk page with an empty page.
///
/// Why: `/graph` calls `get_chunks` first, so overlay-merge coverage needs a
/// reachable search daemon. An always-empty corpus keeps the tree-sitter half
/// of the graph at zero nodes, which is precisely the "empty graph" a caller
/// could not previously tell apart from "no SCIP data".
/// What: binds an ephemeral loopback port serving
/// `GET /indexes/{id}/chunks` → `{"chunks": []}` and returns its base URL.
/// Test: used by `graph_marks_scip_overlay_present_after_ingest` and
/// `graph_marks_scip_overlay_absent_without_ingest`.
async fn spawn_empty_chunk_search() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let stub = Router::new().route(
        "/indexes/{id}/chunks",
        axum::routing::get(|| async { axum::response::Json(serde_json::json!({ "chunks": [] })) }),
    );
    tokio::spawn(async move {
        axum::serve(listener, stub).await.ok();
    });
    format!("http://{addr}")
}

async fn graph_overlay_header(app: Router, index_id: &str) -> (StatusCode, String) {
    let resp = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri(format!("/indexes/{index_id}/graph"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let header = resp
        .headers()
        .get("x-scip-overlay")
        .map(|v| v.to_str().unwrap().to_string())
        .unwrap_or_default();
    (status, header)
}

/// Why (#5049): the endpoint where the ambiguity was observed. A `/graph`
/// response for an index nobody SCIP-ingested must say so in the response
/// itself, not leave the caller to infer it from an empty body.
/// What: with an empty chunk corpus and no ingest, `/graph` answers 200 with
/// `x-scip-overlay: absent`.
/// Test: this test.
#[tokio::test]
async fn graph_marks_scip_overlay_absent_without_ingest() {
    let tmp = TempDir::new().unwrap();
    let search_base = spawn_empty_chunk_search().await;
    let app = build_router(state_in_with_search(tmp.path(), &search_base));

    let (status, header) = graph_overlay_header(app, "myidx").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(header, "absent");
}

/// Why (#5049): the same response must flip to `present` once an overlay
/// exists — and must still say `present` after a restart, which is what
/// proves the merged graph is being served from disk rather than from the
/// process that accepted the ingest.
/// What: ingests through one router, rebuilds state over the same data
/// directory, then asserts `/graph` answers `x-scip-overlay: present` and the
/// merged body carries the SCIP symbol even though the chunk corpus is empty.
/// Test: this test.
#[tokio::test]
async fn graph_marks_scip_overlay_present_after_ingest() {
    let tmp = TempDir::new().unwrap();
    let search_base = spawn_empty_chunk_search().await;

    {
        let app = build_router(state_in_with_search(tmp.path(), &search_base));
        let (status, _) = post_scip(&app, "myidx", scip_index_bytes(&["hello"])).await;
        assert_eq!(status, StatusCode::OK);
    }

    let app = build_router(state_in_with_search(tmp.path(), &search_base));
    let (status, header) = graph_overlay_header(app.clone(), "myidx").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(header, "present");

    let (status, body) = json_get(app, "/indexes/myidx/graph").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["nodes"].as_array().unwrap().len(),
        1,
        "the persisted overlay must be merged into /graph: {body}"
    );
    assert_eq!(body["nodes"][0]["name"], "hello");
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
