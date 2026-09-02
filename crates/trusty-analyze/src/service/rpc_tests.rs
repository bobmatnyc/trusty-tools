//! Wire and handler tests for the UDS service (#6287).
//!
//! Why: this file replaces `service/tests.rs` and `service/tests_review.rs`,
//! which drove the axum router with `tower::ServiceExt::oneshot` and asserted
//! HTTP status codes. Every behavioural assertion in those files is preserved
//! here against the JSON-RPC surface that replaced it; what is gone is the
//! coverage of things that no longer exist — the SSE broadcast, the
//! same-origin write guard, and the retired `POST /webhooks/github` route,
//! whose "is it registered" question a router with no paths cannot ask.
//!
//! What: [`dispatch`] drives [`RpcRouter::dispatch`] directly for handler
//! behaviour (fast, no socket), and the `rpc_*_over_a_real_socket` tests drive
//! [`serve_with_shutdown`] for the wire behaviour the router cannot show —
//! binding, the frame budget, and the unlink.
//!
//! Test: `cargo test -p trusty-analyze`.

use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use axum::Router;
use tempfile::TempDir;
use trusty_common::uds::server::{RpcResponse, CODE_INVALID_PARAMS, CODE_METHOD_NOT_FOUND};

use super::*;
use crate::core::{FactStore, ScipOverlayStore, TrustySearchClient};
use crate::service::events::{AnalyzerAppState, CODE_NOT_FOUND};

// ─── fixtures ────────────────────────────────────────────────────────────────

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
/// Test: used by `rpc_scip_overlay_survives_state_rebuild`.
pub(crate) fn state_in(dir: &Path) -> AnalyzerAppState {
    state_in_with_search(dir, "http://127.0.0.1:1")
}

/// `state_in`, but pointed at an arbitrary trusty-search base URL.
///
/// Why (#5049): `analyze.graph` fetches chunks before it ever reads the
/// overlay, so with the unreachable default client every graph request fails
/// upstream and the overlay-merge path is untestable. Tests that need the graph
/// point this at a local stub instead.
///
/// trusty-search is still an HTTP daemon, which is why these stubs are still
/// axum: #6287 moved trusty-analyze's OWN transport, not the one it consumes.
pub(crate) fn state_in_with_search(dir: &Path, search_base: &str) -> AnalyzerAppState {
    let facts = FactStore::open(&dir.join("facts.redb")).unwrap();
    let overlays = ScipOverlayStore::open(&dir.join("scip_overlays.redb")).unwrap();
    let search = TrustySearchClient::new(search_base);
    AnalyzerAppState::new(search, facts, overlays)
}

/// Call one method on `state`'s router and return the raw JSON-RPC response.
///
/// Why: `RpcRouter::dispatch` is a pure function over a frame — the same
/// property `trusty_common`'s own dispatcher tests rely on — so handler
/// behaviour is assertable without binding anything. The socket half is covered
/// separately, by the tests that need it.
/// What: builds a well-formed request frame, dispatches, returns the response.
async fn dispatch(
    state: &AnalyzerAppState,
    method: &str,
    params: serde_json::Value,
) -> RpcResponse {
    let frame = serde_json::to_vec(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": method,
        "params": params,
    }))
    .unwrap();
    build_router(state.clone()).dispatch(&frame).await
}

/// [`dispatch`], unwrapping the success half.
async fn ok(
    state: &AnalyzerAppState,
    method: &str,
    params: serde_json::Value,
) -> serde_json::Value {
    let response = dispatch(state, method, params).await;
    assert!(
        response.error.is_none(),
        "{method} was expected to succeed: {:?}",
        response.error
    );
    response.result.expect("a success frame carries a result")
}

/// [`dispatch`], unwrapping the failure half as `(code, message)`.
async fn err(state: &AnalyzerAppState, method: &str, params: serde_json::Value) -> (i64, String) {
    let response = dispatch(state, method, params).await;
    let error = response
        .error
        .unwrap_or_else(|| panic!("{method} was expected to fail, got {:?}", response.result));
    (error.code, error.message)
}

// ─── router surface ──────────────────────────────────────────────────────────

/// Why (#6287): four crates outside this one dial these names by literal, none
/// with a Cargo edge on it. `METHODS` is the list they can be checked against;
/// this is what keeps the list equal to what is actually registered, so a
/// rename shows up here rather than as a consumer reporting `method_not_found`.
/// Test: this is the test.
#[tokio::test]
async fn rpc_router_registers_every_documented_method() {
    let (state, _tmp) = make_state();
    let router = build_router(state);
    let registered: Vec<&str> = router.method_names().collect();
    let mut documented: Vec<&str> = METHODS.to_vec();
    documented.sort_unstable();
    assert_eq!(
        registered, documented,
        "service::rpc::METHODS must name exactly what build_router registers"
    );
}

/// Why: a client that drifts must read a reason, not a dropped connection.
/// Test: this is the test.
#[tokio::test]
async fn rpc_reports_method_not_found_for_an_unknown_method() {
    let (state, _tmp) = make_state();
    let (code, message) = err(&state, "analyze.nope", serde_json::json!({})).await;
    assert_eq!(code, CODE_METHOD_NOT_FOUND);
    assert!(
        message.contains("analyze.health"),
        "the error must list what IS served: {message}"
    );
}

/// Why (#6287): the path segment that used to carry `index_id` is gone, so a
/// request that omits the field has to be refused by the decode rather than
/// silently analysing an index called `""`.
/// Test: this is the test.
#[tokio::test]
async fn rpc_reports_invalid_params_for_a_request_naming_no_index() {
    let (state, _tmp) = make_state();
    let (code, message) = err(&state, "analyze.quality", serde_json::json!({})).await;
    assert_eq!(code, CODE_INVALID_PARAMS);
    assert!(
        message.contains("index_id"),
        "the error must name the missing field: {message}"
    );
}

// ─── health and index listing ────────────────────────────────────────────────

/// Why: the stub search client points at port 1 (nothing listening), so this is
/// the degraded verdict every consumer's card renders.
///
/// #6287: degraded is a RESULT frame. The HTTP shape answered 503 here, and an
/// error frame would be that 503's literal translation — but it would take
/// `version` and `search_reachable` away from the caller who needs them.
/// Test: this is the test.
#[tokio::test]
async fn rpc_health_reports_degraded_when_search_is_unreachable() {
    let (state, _tmp) = make_state();
    let body = ok(&state, METHOD_HEALTH, serde_json::Value::Null).await;
    assert_eq!(body["status"], "degraded");
    assert_eq!(body["search_reachable"], false);
}

/// Why: `trusty-console` and `tctl` both render `version` off this answer.
/// Test: this is the test.
#[tokio::test]
async fn rpc_health_response_includes_version() {
    let (state, _tmp) = make_state();
    let body = ok(&state, METHOD_HEALTH, serde_json::Value::Null).await;
    assert!(!body["version"].as_str().unwrap_or_default().is_empty());
}

/// Why: `params` is absent on a well-formed no-argument call, which arrives as
/// `null`. A plain unit struct refuses `null`, so every health probe in the
/// workspace would answer `invalid_params` — see [`NoParams`].
/// Test: this is the test.
#[tokio::test]
async fn rpc_health_answers_with_no_params() {
    let (state, _tmp) = make_state();
    let frame = br#"{"jsonrpc":"2.0","id":1,"method":"analyze.health"}"#;
    let response = build_router(state).dispatch(frame).await;
    assert!(response.error.is_none(), "{:?}", response.error);
}

/// Why: a caller that sends a stray field is not refused — this method has no
/// arguments to get wrong, and refusing would turn an additive client change
/// into an outage.
/// Test: this is the test.
#[tokio::test]
async fn rpc_health_answers_with_a_stray_params_object() {
    let (state, _tmp) = make_state();
    let body = ok(&state, METHOD_HEALTH, serde_json::json!({ "stray": 1 })).await;
    assert_eq!(body["status"], "degraded");
}

/// Why: an unreachable search daemon is an upstream failure, never an empty
/// list — a caller must not read "no indexes" off a broken dependency.
/// Test: this is the test.
#[tokio::test]
async fn rpc_list_indexes_reports_an_unreachable_search_daemon() {
    let (state, _tmp) = make_state();
    let (code, message) = err(&state, "analyze.list_indexes", serde_json::Value::Null).await;
    assert_eq!(code, trusty_common::uds::server::CODE_INTERNAL_ERROR);
    assert!(
        message.contains("search"),
        "the error must name the upstream: {message}"
    );
}

/// Why: same rule for the diagnostics corpus fetch, which is the method most
/// likely to be blamed for an upstream outage because it runs longest.
/// Test: this is the test.
#[tokio::test]
async fn rpc_diagnostics_reports_an_unreachable_search_daemon() {
    let (state, _tmp) = make_state();
    let (code, _) = err(
        &state,
        "analyze.diagnostics",
        serde_json::json!({ "index_id": "demo" }),
    )
    .await;
    assert_eq!(code, trusty_common::uds::server::CODE_INTERNAL_ERROR);
}

// ─── facts ───────────────────────────────────────────────────────────────────

/// Why: the CRUD round-trip is what proves the redb store is wired through the
/// blocking pool correctly (issue #67) and that `facts_list` reads what
/// `facts_upsert` wrote.
/// Test: this is the test.
#[tokio::test]
async fn rpc_upsert_then_list_facts_round_trip() {
    let (state, _tmp) = make_state();
    let upserted = ok(
        &state,
        "analyze.facts_upsert",
        serde_json::json!({
            "subject": "fn search",
            "predicate": "implements",
            "object": "trait Searcher",
            "index_id": "test",
        }),
    )
    .await;
    assert_eq!(upserted["upserted"], true);

    let listing = ok(&state, "analyze.facts_list", serde_json::json!({})).await;
    assert_eq!(listing["count"], 1);

    let id = upserted["id"].as_u64().expect("an upsert reports its id");
    let deleted = ok(
        &state,
        "analyze.facts_delete",
        serde_json::json!({ "id": id }),
    )
    .await;
    assert_eq!(deleted["removed"], true);
}

/// Why (#6287): `facts_list`'s three filters are all optional, and the daemon's
/// request struct gives each a `None` default. A call with no params at all —
/// `null` — must list everything rather than fail the decode.
/// Test: this is the test.
#[tokio::test]
async fn rpc_facts_list_accepts_absent_params() {
    let (state, _tmp) = make_state();
    let listing = ok(&state, "analyze.facts_list", serde_json::Value::Null).await;
    assert_eq!(listing["count"], 0);
}

// ─── SCIP ingest and overlay status ──────────────────────────────────────────

/// Encode a SCIP `Index` protobuf carrying `symbol_names.len()` function
/// symbols in `src/lib.rs`. An empty slice yields a valid, symbol-free index.
///
/// Why (#5049): the empty-vs-absent distinction needs a SCIP payload that
/// legitimately produces zero KG nodes, so the ingest path can be driven with
/// both shapes from one helper.
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

/// `scip_index_bytes`, base64-encoded as the method now takes it (#6287).
fn scip_index_base64(symbol_names: &[&str]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(scip_index_bytes(symbol_names))
}

#[tokio::test]
async fn rpc_scip_ingest_accepts_a_valid_index_and_stores_the_overlay() {
    let (state, _tmp) = make_state();
    let overlays = state.scip_overlays.clone();

    let parsed = ok(
        &state,
        "analyze.scip_ingest",
        serde_json::json!({ "index_id": "myidx", "scip_base64": scip_index_base64(&["hello"]) }),
    )
    .await;
    assert_eq!(parsed["index_id"], "myidx");
    assert_eq!(parsed["documents"], 1);
    assert_eq!(parsed["kg_nodes"], 1);

    // The overlay should be persisted in the durable store.
    let rec = overlays.get("myidx").unwrap().expect("overlay stored");
    assert_eq!(rec.graph.node_count(), 1);
    assert_eq!(rec.graph.nodes[0].name, "hello");
}

/// Why (#5049): the defect. SCIP ingest answered 200 while writing only to an
/// in-process `HashMap`, so a daemon restart discarded the ingest and every
/// later graph call silently served a tree-sitter-only graph.
/// What: ingests through one state, drops it entirely, rebuilds
/// `AnalyzerAppState` over the SAME data directory (the in-process stand-in for
/// a restart), and asserts the second daemon still reports the overlay.
/// Test: this is the test.
#[tokio::test]
async fn rpc_scip_overlay_survives_state_rebuild() {
    let tmp = TempDir::new().unwrap();

    {
        let state = state_in(tmp.path());
        let parsed = ok(
            &state,
            "analyze.scip_ingest",
            serde_json::json!({
                "index_id": "myidx",
                "scip_base64": scip_index_base64(&["hello"]),
            }),
        )
        .await;
        assert_eq!(parsed["kg_nodes"], 1);
    }

    // Second "daemon boot" over the same data directory.
    let state = state_in(tmp.path());
    let body = ok(
        &state,
        "analyze.scip_status",
        serde_json::json!({ "index_id": "myidx" }),
    )
    .await;
    assert_eq!(body["index_id"], "myidx");
    assert_eq!(body["nodes"], 1);
}

/// Why (#5049): persistence stops the loss but not the silence — a caller still
/// has to tell "nobody ingested SCIP here" from "an ingested SCIP index had no
/// symbols". Both contribute zero nodes to the graph.
///
/// #6287 carried that distinction across the transport change: what was HTTP
/// 404 versus 200-with-`nodes: 0` is now an ERROR frame carrying
/// [`CODE_NOT_FOUND`] versus a RESULT frame carrying `nodes: 0`.
/// What: index ingested with a symbol-free payload → result, `nodes: 0`;
/// never-ingested index → error, `CODE_NOT_FOUND`, message naming the index.
/// Test: this is the test.
#[tokio::test]
async fn rpc_empty_scip_ingest_is_distinguishable_from_no_ingest() {
    let (state, _tmp) = make_state();

    ok(
        &state,
        "analyze.scip_ingest",
        serde_json::json!({ "index_id": "empty-idx", "scip_base64": scip_index_base64(&[]) }),
    )
    .await;

    let body = ok(
        &state,
        "analyze.scip_status",
        serde_json::json!({ "index_id": "empty-idx" }),
    )
    .await;
    assert_eq!(body["nodes"], 0, "ingested-but-empty overlay is present");

    let (code, message) = err(
        &state,
        "analyze.scip_status",
        serde_json::json!({ "index_id": "never-idx" }),
    )
    .await;
    assert_eq!(
        code, CODE_NOT_FOUND,
        "never-ingested must be its own code, not internal_error: {message}"
    );
    assert!(
        message.contains("never-idx"),
        "the error must name the index: {message}"
    );
}

#[tokio::test]
async fn rpc_scip_status_reports_not_found_when_never_ingested() {
    let (state, _tmp) = make_state();
    let (code, _) = err(
        &state,
        "analyze.scip_status",
        serde_json::json!({ "index_id": "nothing-here" }),
    )
    .await;
    assert_eq!(code, CODE_NOT_FOUND);
}

#[tokio::test]
async fn rpc_scip_ingest_rejects_garbage_bytes() {
    use base64::Engine as _;
    let (state, _tmp) = make_state();
    let garbage = base64::engine::general_purpose::STANDARD.encode([0xFFu8; 4]);
    let (code, message) = err(
        &state,
        "analyze.scip_ingest",
        serde_json::json!({ "index_id": "x", "scip_base64": garbage }),
    )
    .await;
    assert_eq!(code, CODE_INVALID_PARAMS);
    assert!(
        message.contains("SCIP"),
        "the error must say what failed to parse: {message}"
    );
}

/// Why (#6287): the base64 decode moved from the MCP client to the daemon, so
/// the daemon now owns the "that is not base64" message a client used to
/// produce. Without this, a malformed argument would surface as a protobuf
/// parse failure and point the caller at the wrong layer.
/// Test: this is the test.
#[tokio::test]
async fn rpc_scip_ingest_rejects_invalid_base64() {
    let (state, _tmp) = make_state();
    let (code, message) = err(
        &state,
        "analyze.scip_ingest",
        serde_json::json!({ "index_id": "x", "scip_base64": "not base64!!" }),
    )
    .await;
    assert_eq!(code, CODE_INVALID_PARAMS);
    assert!(
        message.contains("base64"),
        "the error must name the encoding, not the protobuf: {message}"
    );
}

// ─── review, deep analysis, and the synthesis helpers ────────────────────────

/// Why: review cross-references the named index's corpus, so an empty
/// `index_id` names no corpus and must be refused before any work is done. The
/// query parameter that used to carry it was `Option<String>`; the field is now
/// required, but an empty string still decodes, so the check is still owed.
/// Test: this is the test.
#[tokio::test]
async fn rpc_review_requires_a_non_empty_index_id() {
    let (state, _tmp) = make_state();
    let (code, message) = err(
        &state,
        "analyze.review",
        serde_json::json!({ "index_id": "", "diff": "" }),
    )
    .await;
    assert_eq!(code, CODE_INVALID_PARAMS);
    assert!(message.contains("index_id"), "{message}");
}

/// Why: with trusty-search unreachable the corpus fetch fails, and review must
/// report that rather than grading a diff against nothing.
/// Test: this is the test.
#[tokio::test]
async fn rpc_review_reports_an_unreachable_search_daemon() {
    let (state, _tmp) = make_state();
    let diff = "diff --git a/x.rs b/x.rs\n--- a/x.rs\n+++ b/x.rs\n@@ -1 +1 @@\n-a\n+b\n";
    let (code, _) = err(
        &state,
        "analyze.review",
        serde_json::json!({ "index_id": "demo", "diff": diff }),
    )
    .await;
    assert_eq!(code, trusty_common::uds::server::CODE_INTERNAL_ERROR);
}

/// Why: a malformed hunk header is the caller's fault, not an upstream outage,
/// and the two must not report the same way — one is worth retrying and the
/// other never is.
/// Test: this is the test.
#[tokio::test]
async fn rpc_review_rejects_a_malformed_diff() {
    let (state, _tmp) = make_state();
    let (code, message) = err(
        &state,
        "analyze.review",
        serde_json::json!({
            "index_id": "demo",
            "diff": "diff --git a/x.rs b/x.rs\n--- a/x.rs\n+++ b/x.rs\n@@ not-a-hunk @@\n",
        }),
    )
    .await;
    assert_eq!(code, CODE_INVALID_PARAMS);
    assert!(message.contains("diff"), "{message}");
}

/// Why: `analyze.review_github_pr` reads `GITHUB_TOKEN` from the daemon's own
/// environment, so an unconfigured daemon must say so rather than fail at the
/// GitHub call with an opaque 401.
///
/// The test only runs when the variable is ABSENT: setting or clearing it would
/// be process-global, and this crate runs its tests in parallel.
/// Test: this is the test.
#[tokio::test]
async fn rpc_github_pr_requires_a_token() {
    if std::env::var(trusty_common::env_vars::ENV_GITHUB_TOKEN).is_ok() {
        eprintln!("skip: GITHUB_TOKEN is set in this environment");
        return;
    }
    let (state, _tmp) = make_state();
    let (code, message) = err(
        &state,
        "analyze.review_github_pr",
        serde_json::json!({
            "owner": "bobmatnyc", "repo": "trusty-tools", "pr": 1, "index_id": "demo",
        }),
    )
    .await;
    assert_eq!(code, CODE_INVALID_PARAMS);
    assert!(message.contains("GITHUB_TOKEN"), "{message}");
}

/// Why: deep analysis without an index has nothing to synthesise a report from.
/// Test: this is the test.
#[tokio::test]
async fn rpc_deep_requires_an_index_id() {
    let (state, _tmp) = make_state();
    let (code, message) = err(
        &state,
        "analyze.deep_analysis",
        serde_json::json!({ "index_id": "  " }),
    )
    .await;
    assert_eq!(code, CODE_INVALID_PARAMS);
    assert!(message.contains("index_id"), "{message}");
}

/// Why: the key is read once at startup, so a daemon started without it can
/// never serve this method and has to say so at the first call rather than at
/// the provider.
/// Test: this is the test.
#[tokio::test]
async fn rpc_deep_requires_an_api_key() {
    let (state, _tmp) = make_state();
    let state = state.with_api_key(None);
    let (code, message) = err(
        &state,
        "analyze.deep_analysis",
        serde_json::json!({ "index_id": "demo" }),
    )
    .await;
    assert_eq!(code, CODE_INVALID_PARAMS);
    assert!(message.contains("OPENROUTER_API_KEY"), "{message}");
}

#[test]
fn synthesise_review_from_chunks_groups_by_file() {
    // Synthesis should produce one FileReview per distinct chunk.file,
    // with NewFile source and no spurious recommendations.
    use crate::core::review::ReviewSource;
    use crate::types::CodeChunk;
    let chunks = vec![
        CodeChunk {
            id: "a:1:5".into(),
            file: "src/a.rs".into(),
            start_line: 1,
            end_line: 5,
            content: "fn a() {}".into(),
            ..Default::default()
        },
        CodeChunk {
            id: "a:10:20".into(),
            file: "src/a.rs".into(),
            start_line: 10,
            end_line: 20,
            content: "fn aa() {}".into(),
            ..Default::default()
        },
        CodeChunk {
            id: "b:1:3".into(),
            file: "src/b.rs".into(),
            start_line: 1,
            end_line: 3,
            content: "fn b() {}".into(),
            ..Default::default()
        },
    ];
    let report = crate::service::handlers::deep::synthesise_review_from_chunks(&chunks);
    assert_eq!(report.files.len(), 2);
    let paths: Vec<&str> = report.files.iter().map(|f| f.path.as_str()).collect();
    assert!(paths.contains(&"src/a.rs"));
    assert!(paths.contains(&"src/b.rs"));
    for f in &report.files {
        assert_eq!(f.source, ReviewSource::NewFile);
        assert!(f.recommendations.is_empty());
    }
}

#[test]
fn synthesise_review_from_chunks_empty_corpus_is_grade_a() {
    let report = crate::service::handlers::deep::synthesise_review_from_chunks(&[]);
    assert!(report.files.is_empty());
    assert_eq!(report.overall_grade, crate::types::ComplexityGrade::A);
    assert_eq!(report.smell_count, 0);
}

#[test]
fn lookup_frameworks_reads_stored_facts() {
    // record_frameworks → lookup_frameworks round-trip: the deep handler
    // must be able to read back the framework names that registry.rs
    // recorded under the (`index_id`, `uses_framework`, ...) triple.
    use crate::core::facts::new_fact;
    let (state, _tmp) = make_state();
    for fw in ["React", "Next.js"] {
        let f = new_fact(
            "my-idx".to_string(),
            "uses_framework".to_string(),
            fw.to_string(),
            "my-idx".to_string(),
        );
        state.facts.upsert(f).unwrap();
    }
    let mut got = crate::service::handlers::deep::lookup_frameworks(&state, "my-idx");
    got.sort();
    assert_eq!(got, vec!["Next.js".to_string(), "React".to_string()]);
}

// ─── stub trusty-search daemons ──────────────────────────────────────────────

/// Stand-in trusty-search that answers every chunk page with an empty page.
///
/// Why: the graph method calls `get_chunks` first, so overlay-merge coverage
/// needs a reachable search daemon. An always-empty corpus keeps the
/// tree-sitter half of the graph at zero nodes, which is precisely the "empty
/// graph" a caller could not previously tell apart from "no SCIP data".
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

/// Stand-in trusty-search serving one code chunk and one Markdown chunk.
///
/// Why (#5317/#5320): both defects are about what the analyzer does with a
/// non-code file and how a caller reads a truncated result, and neither is
/// observable without a corpus that mixes the two.
async fn spawn_mixed_corpus_search() -> String {
    let mut branchy = String::from("/// doc\nfn m(a: u32) {\n");
    for _ in 0..30 {
        branchy.push_str("    if a == 1 { return; }\n");
    }
    branchy.push_str("}\n");
    // Release-note prose in the shape that actually reached a report's RED
    // band: enough conditional English for the keyword text heuristic — the
    // fallback for an unmapped extension — to grade it as complex code.
    let mut changelog_prose = String::from("# Changelog\n\n## 1.0\n");
    for i in 0..30 {
        changelog_prose.push_str(&format!(
            "- Fixed a case where, if the cache was cold and the token had expired, \
             the retry loop would spin for each queued item while the lock was held ({i})\n"
        ));
    }
    let body = serde_json::json!({ "chunks": [
        {
            "id": "code:1", "file": "/repo/src/lib.rs", "start_line": 1, "end_line": 40,
            "content": branchy, "score": 0.0, "match_reason": "test"
        },
        {
            "id": "doc:1", "file": "/repo/CHANGELOG.md", "start_line": 1, "end_line": 40,
            "content": changelog_prose,
            "score": 0.0, "match_reason": "test"
        }
    ]});
    spawn_paged_chunk_search(body).await
}

/// Spawn a stub trusty-search that serves a small, real chunk corpus.
///
/// Why (#5067): `spawn_empty_chunk_search` short-circuits clustering before it
/// ever reaches the embedder, so it cannot show that the surviving BOW path
/// still produces usable vectors. Clustering needs actual content.
async fn spawn_chunk_search_with_corpus() -> String {
    let bodies = [
        "fn authenticate(user: User) -> Result<Session> { verify_password(user) }",
        "fn authorize(session: Session) -> bool { session.scopes.contains(\"admin\") }",
        "fn parse_config(path: &Path) -> Config { toml::from_str(&read(path)) }",
        "fn merge_config(a: Config, b: Config) -> Config { a.overlay(b) }",
        "fn render_chart(series: &[Point]) -> Svg { svg::plot(series) }",
        "fn render_legend(labels: &[String]) -> Svg { svg::legend(labels) }",
    ];
    let chunks: Vec<serde_json::Value> = bodies
        .iter()
        .enumerate()
        .map(|(i, content)| {
            serde_json::json!({
                "id": format!("src/lib.rs:{i}:{}", i + 3),
                "file": "src/lib.rs",
                "start_line": i,
                "end_line": i + 3,
                "content": content,
            })
        })
        .collect();
    spawn_paged_chunk_search(serde_json::json!({ "chunks": chunks })).await
}

/// Serve `first_page` at the first offset and an empty page beyond it.
///
/// Why: the client pages the corpus with a concurrent window, so the stub must
/// honour `after` — answering every offset with the same page would multiply
/// the corpus by the window width.
async fn spawn_paged_chunk_search(first_page: serde_json::Value) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let stub = Router::new().route(
        "/indexes/{id}/chunks",
        axum::routing::get(
            move |axum::extract::Query(q): axum::extract::Query<HashMap<String, String>>| {
                let body = first_page.clone();
                async move {
                    let first = q
                        .get("after")
                        .map(|c: &String| c.is_empty())
                        .unwrap_or(true);
                    axum::response::Json(if first {
                        body
                    } else {
                        serde_json::json!({ "chunks": [] })
                    })
                }
            },
        ),
    );
    tokio::spawn(async move {
        axum::serve(listener, stub).await.ok();
    });
    format!("http://{addr}")
}

// ─── analysis over a live corpus ─────────────────────────────────────────────

/// Why (#5320): the report's §7 complexity table was a bucketed top-N hotspot
/// slice rendered as a distribution, so a 1.37M-line repository reported zero
/// simple functions. The method that replaces it must answer with every band
/// and with the denominator those percentages are shares of.
/// Test: this is the test.
#[tokio::test]
async fn rpc_complexity_distribution_returns_all_bands() {
    let tmp = TempDir::new().unwrap();
    let search_base = spawn_mixed_corpus_search().await;
    let state = state_in_with_search(tmp.path(), &search_base);

    let body = ok(
        &state,
        "analyze.complexity_distribution",
        serde_json::json!({ "index_id": "myidx" }),
    )
    .await;

    let buckets = body["buckets"].as_array().expect("buckets array");
    let grades: Vec<&str> = buckets
        .iter()
        .map(|b| b["grade"].as_str().unwrap())
        .collect();
    assert_eq!(
        grades,
        vec!["A", "B", "C", "D", "F"],
        "every band renders, empty included: {body}"
    );
    assert_eq!(body["total"], 1, "only the .rs chunk is counted: {body}");
    assert_eq!(body["skipped_non_code"], 1, "the .md chunk is skipped");
    let summed: u64 = buckets.iter().map(|b| b["count"].as_u64().unwrap()).sum();
    assert_eq!(
        summed, 1,
        "band counts must sum to the stated denominator: {body}"
    );
}

/// Why (#5317): `CHANGELOG.md`, `FAQ.md`, and `.github/workflows/release.yml`
/// were rendered as the Component of an "Extract method" finding, because the
/// text-heuristic fallback grades prose F.
/// What: asserts every returned suggestion names a file with a mapped language,
/// and that the code chunk itself still produces one — the filter must not
/// silence real signal.
/// Test: this is the test.
#[tokio::test]
async fn rpc_refactor_suggestions_skip_non_code_files() {
    let tmp = TempDir::new().unwrap();
    let search_base = spawn_mixed_corpus_search().await;
    let state = state_in_with_search(tmp.path(), &search_base);

    let body = ok(
        &state,
        "analyze.refactor_suggestions",
        serde_json::json!({ "index_id": "myidx" }),
    )
    .await;
    let suggestions = body["suggestions"].as_array().expect("suggestions array");
    assert!(
        suggestions
            .iter()
            .all(|s| s["file"].as_str().unwrap().ends_with(".rs")),
        "a refactor suggestion may only name a code file: {body}"
    );
    assert_eq!(
        suggestions.len(),
        1,
        "the grade-F .rs chunk must still be suggested: {body}"
    );
}

/// Why (#5049): the method where the ambiguity was observed. A graph response
/// for an index nobody SCIP-ingested must say so in the response itself, not
/// leave the caller to infer it from an empty body.
///
/// #6287 moved the answer from the `x-scip-overlay` header into the body, so
/// this asserts the field rather than a header.
/// Test: this is the test.
#[tokio::test]
async fn rpc_graph_marks_scip_overlay_absent_without_ingest() {
    let tmp = TempDir::new().unwrap();
    let search_base = spawn_empty_chunk_search().await;
    let state = state_in_with_search(tmp.path(), &search_base);

    let body = ok(
        &state,
        "analyze.graph",
        serde_json::json!({ "index_id": "myidx" }),
    )
    .await;
    assert_eq!(body["scip_overlay"], false, "body: {body}");
}

/// Why (#5049): the same response must flip to present once an overlay exists —
/// and must still say present after a restart, which is what proves the merged
/// graph is being served from disk rather than from the process that accepted
/// the ingest.
/// Test: this is the test.
#[tokio::test]
async fn rpc_graph_marks_scip_overlay_present_after_ingest() {
    let tmp = TempDir::new().unwrap();
    let search_base = spawn_empty_chunk_search().await;

    {
        let state = state_in_with_search(tmp.path(), &search_base);
        ok(
            &state,
            "analyze.scip_ingest",
            serde_json::json!({
                "index_id": "myidx",
                "scip_base64": scip_index_base64(&["hello"]),
            }),
        )
        .await;
    }

    let state = state_in_with_search(tmp.path(), &search_base);
    let body = ok(
        &state,
        "analyze.graph",
        serde_json::json!({ "index_id": "myidx" }),
    )
    .await;
    assert_eq!(body["scip_overlay"], true, "body: {body}");
    assert_eq!(
        body["nodes"].as_array().unwrap().len(),
        1,
        "the persisted overlay must be merged into the graph: {body}"
    );
    assert_eq!(body["nodes"][0]["name"], "hello");
}

/// Why (#5067): removing the neural embedder must not remove clustering. This
/// is the "used path still works" half of the fix — the method every real
/// caller hits has to keep returning usable vectors, now sourced from the
/// state's BOW embedder rather than a model loaded at boot.
/// Test: this is the test.
#[tokio::test]
async fn rpc_clusters_return_bow_vectors_for_a_live_corpus() {
    let tmp = TempDir::new().unwrap();
    let search_base = spawn_chunk_search_with_corpus().await;
    let state = state_in_with_search(tmp.path(), &search_base);

    let body = ok(
        &state,
        "analyze.clusters",
        serde_json::json!({ "index_id": "demo", "k": 3 }),
    )
    .await;
    assert_eq!(body["method"], "bow");
    assert_eq!(body["dim"], 256);
    assert_eq!(body["chunk_count"], 6);
    let clusters = body["clusters"].as_array().expect("clusters array");
    assert!(!clusters.is_empty(), "no clusters produced: {body}");
    let members: usize = clusters
        .iter()
        .map(|c| c["members"].as_array().map(Vec::len).unwrap_or(0))
        .sum();
    assert_eq!(members, 6, "every chunk must land in a cluster: {body}");
}

/// Why (#5067): the pre-fix daemon answered `method=neural` with BOW vectors
/// whenever the model had failed to load — it logged a warning at startup and
/// then said nothing to the caller. Deleting the backend while still accepting
/// the parameter would make that substitution permanent and invisible.
/// Test: this is the test.
#[tokio::test]
async fn rpc_clusters_reject_removed_neural_method() {
    let tmp = TempDir::new().unwrap();
    let search_base = spawn_chunk_search_with_corpus().await;
    let state = state_in_with_search(tmp.path(), &search_base);

    let (code, message) = err(
        &state,
        "analyze.clusters",
        serde_json::json!({ "index_id": "demo", "k": 3, "method": "neural" }),
    )
    .await;
    assert_eq!(
        code, CODE_INVALID_PARAMS,
        "`method=neural` must be rejected, not silently served by BOW: {message}"
    );
    assert!(
        message.contains("neural") && message.contains("bow"),
        "the error must name the rejected value and the supported one: {message}"
    );
}

/// Why: `analyze.clusters` must report an unreachable search daemon rather than
/// answering with zero clusters, which reads as "this corpus has no themes".
/// Test: this is the test.
#[tokio::test]
async fn rpc_clusters_report_an_unreachable_search_daemon() {
    let (state, _tmp) = make_state();
    let (code, _) = err(
        &state,
        "analyze.clusters",
        serde_json::json!({ "index_id": "demo" }),
    )
    .await;
    assert_eq!(code, trusty_common::uds::server::CODE_INTERNAL_ERROR);
}

// ─── the wire ────────────────────────────────────────────────────────────────

/// Start `serve_with_shutdown` on a temp socket and return its handle plus the
/// shutdown trigger.
///
/// Why: the tests below drive the REAL serve path — bind, accept loop, unlink —
/// rather than a hand-rolled listener, which is what makes them able to fail if
/// that path regresses. The shutdown future is a parameter for exactly this
/// reason (see [`serve_with_shutdown`]).
async fn spawn_daemon(
    state: AnalyzerAppState,
    socket: &Path,
) -> (
    tokio::sync::oneshot::Sender<()>,
    tokio::task::JoinHandle<anyhow::Result<()>>,
) {
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let socket_owned = socket.to_path_buf();
    let handle = tokio::spawn(async move {
        serve_with_shutdown(state, &socket_owned, async {
            let _ = stop_rx.await;
        })
        .await
    });
    // Wait for the bind. A socket that never appears fails the test below on
    // the dial, which reports better than a sleep that was too short.
    for _ in 0..200 {
        if trusty_common::uds::socket_is_serving(socket, Duration::from_millis(50)).await {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    (stop_tx, handle)
}

/// Send one frame over a real socket and read the response.
async fn call_over_socket(
    socket: &Path,
    method: &str,
    params: serde_json::Value,
) -> Result<RpcResponse, trusty_common::uds::UdsRpcError> {
    let request = serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": method, "params": params,
    });
    trusty_common::uds::send_framed_request_capped(
        socket,
        &request,
        Duration::from_secs(30),
        MAX_FRAME_BYTES,
    )
    .await
}

/// Why: everything above dispatches a frame in-process. This is the test that
/// proves a frame gets there — through `bind_singleton_hardened`, the peer-uid
/// check `serve_until` runs before a byte is read, and the framing.
///
/// The peer check is exercised, not asserted on: a test cannot connect as
/// another uid without root. What it proves here is the SAME-uid case is
/// ACCEPTED — a check that refused its own user would break every consumer, and
/// `trusty_common::uds::peer` owns the rejection half.
/// Test: this is the test.
#[tokio::test(flavor = "multi_thread")]
async fn rpc_health_answers_over_a_real_socket() {
    let tmp = TempDir::new().unwrap();
    let socket = tmp.path().join("sockets").join("analyze.sock");
    let (state, _state_tmp) = make_state();
    let (stop, handle) = spawn_daemon(state, &socket).await;

    let response = call_over_socket(&socket, METHOD_HEALTH, serde_json::Value::Null)
        .await
        .expect("a live socket must answer");
    let result = response.result.expect("health answers with a result");
    assert_eq!(result["status"], "degraded");
    assert!(!result["version"].as_str().unwrap_or_default().is_empty());

    let _ = stop.send(());
    handle.await.unwrap().unwrap();
}

/// Why: `bind_hardened` binds and chmods; neither it nor `UnixListener`'s
/// `Drop` removes the path, so a server that just returned would leave a file
/// the next start has to take over. This drives the real shutdown path and
/// asserts the file is gone — a test that deleted the file itself would pass
/// whether or not the unlink exists.
/// Test: this is the test.
#[tokio::test(flavor = "multi_thread")]
async fn rpc_unlinks_its_socket_on_shutdown() {
    let tmp = TempDir::new().unwrap();
    let socket = tmp.path().join("sockets").join("analyze.sock");
    let (state, _state_tmp) = make_state();
    let (stop, handle) = spawn_daemon(state, &socket).await;
    assert!(socket.exists(), "the daemon must have bound its socket");

    let _ = stop.send(());
    handle.await.unwrap().unwrap();
    assert!(
        !socket.exists(),
        "the socket file must be unlinked on clean shutdown: {}",
        socket.display()
    );
}

/// Why: [`MAX_FRAME_BYTES`] is four times the shared control-plane default
/// because `analyze.scip_ingest` carries a base64 SCIP index and
/// `analyze.review` carries a raw diff. Without the raised budget a large
/// ingest arrives as a dropped connection rather than as an answer, so this
/// asserts a frame past the shared default is accepted.
/// What: a `analyze.review` frame roughly 12 MiB long — past
/// `trusty_common::uds::MAX_FRAME_BYTES` (8 MiB), inside this service's 32 MiB.
/// Test: this is the test.
#[tokio::test(flavor = "multi_thread")]
async fn rpc_accepts_a_request_larger_than_the_shared_default() {
    let tmp = TempDir::new().unwrap();
    let socket = tmp.path().join("sockets").join("analyze.sock");
    let (state, _state_tmp) = make_state();
    let (stop, handle) = spawn_daemon(state, &socket).await;

    let oversized_for_the_shared_default = 12 * 1024 * 1024;
    assert!(
        oversized_for_the_shared_default > trusty_common::uds::MAX_FRAME_BYTES as usize,
        "the fixture must exceed the budget it is testing the raise of"
    );
    let diff = "+".repeat(oversized_for_the_shared_default);

    // The search client is unreachable, so the ANSWER is an upstream error —
    // which is the point: an error frame means the request was read, whereas the
    // pre-raise failure was a dropped connection with no frame at all.
    let response = call_over_socket(
        &socket,
        "analyze.review",
        serde_json::json!({ "index_id": "demo", "diff": diff }),
    )
    .await
    .expect("a frame inside this service's budget must be read, not dropped");
    assert!(
        response.error.is_some() || response.result.is_some(),
        "the daemon answered with a frame either way"
    );

    let _ = stop.send(());
    handle.await.unwrap().unwrap();
}

/// Why: the budget bounds the read, and a peer that overruns it must be
/// refused rather than allowed to grow the server's buffer without limit. This
/// is the other half of the raise — 32 MiB is a bound, not an absence of one.
/// What: a frame past [`MAX_FRAME_BYTES`] is refused. The client's own read cap
/// is irrelevant here: `dial_and_send` writes the request uncapped, so the
/// refusal is the server's.
/// Test: this is the test.
#[tokio::test(flavor = "multi_thread")]
async fn rpc_refuses_a_request_past_its_own_budget() {
    let tmp = TempDir::new().unwrap();
    let socket = tmp.path().join("sockets").join("analyze.sock");
    let (state, _state_tmp) = make_state();
    let (stop, handle) = spawn_daemon(state, &socket).await;

    let diff = "+".repeat(MAX_FRAME_BYTES as usize + 1024);
    let outcome = call_over_socket(
        &socket,
        "analyze.review",
        serde_json::json!({ "index_id": "demo", "diff": diff }),
    )
    .await;
    assert!(
        outcome.is_err(),
        "a frame past the budget must not be answered: {outcome:?}"
    );

    // The daemon is still serving — refusing one connection must not take the
    // accept loop down with it.
    let response = call_over_socket(&socket, METHOD_HEALTH, serde_json::Value::Null)
        .await
        .expect("the daemon must still answer after refusing an oversized frame");
    assert!(response.result.is_some());

    let _ = stop.send(());
    handle.await.unwrap().unwrap();
}

/// Why (#6287): `analyze.diagnostics` runs external linters, so on a CI runner
/// or a machine without clippy installed the honest answer is an empty
/// `diagnostics` list beside a `tools_run` that names nothing — NOT an error,
/// and not a missing envelope. A client reading `tools_run` is how
/// `trusty-review` tells "no linter existed" from "the codebase is clean"
/// (#5317), so the envelope has to arrive intact even when the list is empty.
/// What: drives the method over a REAL socket against a stub search daemon
/// serving an empty corpus, and asserts every pagination field the envelope
/// promises. `tools_run` is not asserted non-empty: whether clippy is installed
/// is a property of the machine, not of this daemon.
/// Test: this is the test.
#[tokio::test(flavor = "multi_thread")]
async fn rpc_diagnostics_returns_empty_when_no_tools() {
    let tmp = TempDir::new().unwrap();
    let search_base = spawn_paged_chunk_search(serde_json::json!({ "chunks": [] })).await;
    let socket = tmp.path().join("sockets").join("analyze.sock");
    let state = state_in_with_search(tmp.path(), &search_base);
    let (stop, handle) = spawn_daemon(state, &socket).await;

    let response = call_over_socket(
        &socket,
        "analyze.diagnostics",
        serde_json::json!({ "index_id": "demo" }),
    )
    .await
    .expect("a live socket must answer");
    assert!(response.error.is_none(), "{:?}", response.error);
    let body = response.result.expect("diagnostics answers with a result");

    assert_eq!(body["index_id"], "demo");
    assert_eq!(body["total"], 0);
    assert_eq!(body["returned"], 0);
    assert_eq!(body["truncated"], false);
    assert_eq!(
        body["timed_out"], false,
        "an empty corpus cannot exhaust the deadline"
    );
    assert!(body["cutoff"].is_null(), "a complete run reports no cutoff");
    assert!(
        body["diagnostics"].as_array().is_some_and(Vec::is_empty),
        "the list must be present and empty, never absent: {body}"
    );
    assert!(
        body["tools_run"].is_array(),
        "the field that separates 'no linter ran' from 'nothing found' must \
         always be present: {body}"
    );

    let _ = stop.send(());
    handle.await.unwrap().unwrap();
}

/// Why (#6287, preserving #6034/#6041): a handler that exhausted its own
/// deadline and a daemon that simply broke point at different remedies — raise
/// the budget versus start the dependency — and under HTTP that split was 504
/// versus 502. On the socket it is [`CODE_DEADLINE_EXCEEDED`] versus
/// `internal_error`. `trusty-review`'s `classify_failure` reads the CODE to
/// choose `EndpointFailure::TimedOut` over `Unanswered`, which decides which
/// sentence its Gaps & Caveats section prints, so folding this into
/// `internal_error` would silently drop that sentence.
///
/// What: the mapping itself, over every [`ApiErrorKind`]. Driving a real
/// deadline through `analyze.diagnostics` is not an option — the budget is
/// operator-tunable with a 180 s floor, and the test would have to outlast it.
/// The mapping is what a client can observe, and it is what a rename or a
/// collapsed match arm would break.
/// Test: this is the test, with trusty-review's
/// `a_daemon_side_deadline_is_a_timeout_not_a_rejection` holding the other end.
#[test]
fn rpc_diagnostics_reports_deadline_exceeded_distinctly() {
    use crate::service::events::{ApiError, CODE_DEADLINE_EXCEEDED};
    use trusty_common::uds::server::{RpcError, CODE_INTERNAL_ERROR};

    let code_of = |e: ApiError| RpcError::from(e).code;

    assert_eq!(
        CODE_DEADLINE_EXCEEDED, -32005,
        "trusty-review copies this literal; changing it silently breaks the \
         Gaps & Caveats line that distinguishes a timeout from an outage"
    );
    assert_eq!(
        code_of(ApiError::gateway_timeout(
            "diagnostics for 'demo' exceeded 210s"
        )),
        CODE_DEADLINE_EXCEEDED
    );

    // The three arms it must stay distinct from. Without them this test would
    // pass against a router that answered -32005 for everything.
    assert_eq!(code_of(ApiError::internal("we broke")), CODE_INTERNAL_ERROR);
    assert_eq!(
        code_of(ApiError::bad_gateway("search is down")),
        CODE_INTERNAL_ERROR
    );
    assert_eq!(
        code_of(ApiError::not_found("never ingested")),
        CODE_NOT_FOUND,
        "#5049's ingested-but-empty distinction shares the band and must not \
         collide with it"
    );
}

/// Why (#6601 review): the first version of this test pinned the drain to
/// `SHUTDOWN_FLUSH_TIMEOUT` and asserted it fitted inside `sigterm_patience` —
/// a deadline that never applies to a serving analyze process. A bound analyze
/// child is detached, so `ensure_running` never enters it in the supervisor's
/// population and no `terminate_child` call site can reach it; `sigterm_patience`
/// governs only a child that failed to bind. `trusty-analyze stop` sends SIGTERM
/// and polls 5 s to REPORT, never to kill. The only bounded terminator left is
/// the OS grace window, so that is what the drain must be sized to.
///
/// What: `serve_options().shutdown_drain` must be the plannable grace — and the
/// assertion pins the RELATION to the grace window, so an operator's
/// `TRUSTY_TERMINATION_GRACE_SECS` moves both together instead of falsifying a
/// hardcoded number. A 3 s drain fails the first assertion; a whole-window drain
/// fails the second.
/// Test: this is the test.
#[test]
fn serve_options_drain_for_as_long_as_this_server_may_actually_live() {
    let drain = serve_options().shutdown_drain;
    assert_eq!(
        drain,
        trusty_common::shutdown::plannable_grace(),
        "the drain must be the part of the OS grace window this process may \
         plan inside — no supervisor deadline binds a SERVING analyze child"
    );
    assert_eq!(
        drain + trusty_common::shutdown::CLEANUP_RESERVE,
        trusty_common::shutdown::termination_grace(),
        "the drain must still leave the cleanup reserve for the socket unlink \
         and the store drop that follow it"
    );
    assert!(
        drain > SHUTDOWN_FLUSH_TIMEOUT,
        "the #6595 guarantee — redb released before the unlink — must not be \
         abandoned at the supervisor's spawn-failure budget"
    );
}

/// Why (#6595): the idle exit reaches the unlink with zero open connections —
/// `IdleGuard` guarantees it — but the SIGTERM/SIGINT exit used not to.
/// `serve_until_idle` returned `ServeExit::Shutdown` the moment the signal
/// future resolved, with no check on connections in flight, so a connection
/// task still held an `Arc<RpcRouter>` clone and with it the `FactStore`'s
/// `Arc<Database>`. The unlink then ran while that lock was held and handed the
/// next `ensure_running` a successor that could not open facts.redb.
///
/// What closes it since #6601: `drain_shutdown` inside `serve_until_idle`. Every
/// accepted connection holds an `IdleGuard`, and the shutdown arm waits for that
/// count to reach zero — bounded by `RpcServeOptions::shutdown_drain`, which
/// [`serve_options`] inherits from the plannable grace window — before it
/// returns and [`release_stores`] drops the router.
///
/// What forces that path deterministically: a peer that has been accepted but
/// never completes its request frame parks the connection task in a read, so the
/// guard count is above zero when the shutdown lands. The peer is then released,
/// which is what the drain is there to pick up.
/// Test: this is the test.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shutdown_with_a_connection_in_flight_frees_its_redb_lock_before_the_unlink() {
    use tokio::io::AsyncWriteExt as _;

    let tmp = TempDir::new().unwrap();
    let stores = tmp.path().join("stores");
    std::fs::create_dir_all(&stores).unwrap();
    let facts_path = stores.join("facts.redb");
    let socket = tmp.path().join("sockets").join("analyze.sock");
    let (stop, handle) = spawn_daemon(state_in(&stores), &socket).await;

    // No trailing newline, so the frame is incomplete and the handler never
    // runs: the task sits in `read_one_frame` holding its router clone.
    let mut stalled = tokio::net::UnixStream::connect(&socket).await.unwrap();
    stalled
        .write_all(br#"{"jsonrpc":"2.0","id":1"#)
        .await
        .unwrap();
    stalled.flush().await.unwrap();

    // An answered request on a SECOND connection proves the accept loop got
    // past the stalled one — the accept queue is FIFO — so the clone is
    // outstanding by the time the shutdown below fires. A sleep would only
    // make that likely.
    let answered = call_over_socket(&socket, METHOD_HEALTH, serde_json::json!({})).await;
    assert!(
        answered.is_ok(),
        "the daemon must answer before the shutdown"
    );

    let _ = stop.send(());
    // Release the stalled peer just after the shutdown, so the wait has
    // something to wait FOR rather than a task that was already gone.
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(150)).await;
        drop(stalled);
    });

    let watched = socket.clone();
    let probed = facts_path.clone();
    let verdict = tokio::task::spawn_blocking(move || {
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while watched.exists() && std::time::Instant::now() < deadline {
            std::hint::spin_loop();
        }
        assert!(!watched.exists(), "the shutdown never unlinked its socket");
        FactStore::open(&probed).map(|_| ())
    })
    .await
    .unwrap();

    handle.await.unwrap().unwrap();
    if let Err(e) = verdict {
        panic!(
            "a successor spawned at the unlink cannot open {}: {e:#}",
            facts_path.display()
        );
    }
}

/// Why: `remove_retired_discovery_files` is deliberately never called from a
/// test — it resolves the real data directory. Its removal step is this
/// function, and this is where it is proven.
/// Test: this is the test.
#[test]
fn remove_if_present_deletes_a_stale_file_and_tolerates_an_absent_one() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("http_addr");
    std::fs::write(&path, "127.0.0.1:7879").unwrap();
    remove_if_present(&path);
    assert!(!path.exists(), "a stale discovery file must be deleted");
    // The common case on a fresh install: already gone, and silent.
    remove_if_present(&path);
}
