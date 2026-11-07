//! Tests for the scoped and cached `analyze.complexity_hotspots`,
//! `analyze.smells` and `analyze.quality` handlers (#10781).

use std::sync::atomic::Ordering;
use std::sync::Arc;

use serde_json::{json, Value};
use tempfile::TempDir;

use super::*;
use crate::core::fake_search::FakeSearchSocket;
use crate::core::{quality, FactStore, ScipOverlayStore, TrustySearchClient};
use crate::service::analysis_cache::tests::{chunk_calls, corpus, serve_index, FakeIndex};
use crate::service::events::ApiErrorKind;
use crate::service::handlers::analysis::{
    complexity_hotspots, quality_report, smells, HotspotsRequest, QualityRequest, SmellsRequest,
};

fn state(search: &FakeSearchSocket, dir: &TempDir) -> AnalyzerAppState {
    let facts = FactStore::open(&dir.path().join("facts.redb")).expect("facts");
    let overlays = ScipOverlayStore::open(&dir.path().join("scip.redb")).expect("overlays");
    AnalyzerAppState::new(TrustySearchClient::new(search.path()), facts, overlays)
}

fn fixture() -> (Arc<FakeIndex>, FakeSearchSocket, TempDir) {
    let index = Arc::new(FakeIndex::default());
    index.chunk_count.store(2, Ordering::SeqCst);
    let search = serve_index(Arc::clone(&index));
    (index, search, TempDir::new().expect("tempdir"))
}

fn req<T: serde::de::DeserializeOwned>(v: Value) -> T {
    serde_json::from_value(v).expect("request decodes")
}

fn ok<T>(r: Result<T, ApiError>) -> T {
    match r {
        Ok(v) => v,
        Err(e) => panic!("handler failed: {:?}: {}", e.kind, e.message),
    }
}

/// Drop the #10781 additive keys, leaving the pre-#10781 body.
fn legacy(mut body: Value) -> Value {
    if let Some(map) = body.as_object_mut() {
        map.remove("scoped");
        map.remove("cache");
    }
    body
}

#[test]
fn scope_validation_refuses_root_and_oversized_scopes() {
    let none: ScopeParams = req(json!({}));
    assert!(
        matches!(none.resolve(), Ok(None)),
        "no scope fields is unscoped"
    );

    for root in ["", " . ", "./", "/"] {
        let p: ScopeParams = req(json!({ "paths": [root] }));
        let err = p.resolve().err().map(|e| e.kind);
        assert_eq!(
            err,
            Some(ApiErrorKind::BadRequest),
            "{root:?} names the root"
        );
        let p: ScopeParams = req(json!({ "path_prefix": root }));
        assert_eq!(
            p.resolve().err().map(|e| e.kind),
            Some(ApiErrorKind::BadRequest)
        );
    }

    let many: Vec<String> = (0..=MAX_SCOPE_ENTRIES)
        .map(|i| format!("f{i}.rs"))
        .collect();
    let p: ScopeParams = req(json!({ "paths": many }));
    assert_eq!(
        p.resolve().err().map(|e| e.kind),
        Some(ApiErrorKind::BadRequest)
    );

    let p: ScopeParams = req(json!({ "paths": ["a.rs", " a.rs "], "path_prefix": "src" }));
    let scope = ok(p.resolve()).expect("scoped");
    assert_eq!(scope.entries(), vec!["src".to_string(), "a.rs".to_string()]);
}

/// Acceptance test 1 (shape): a scoped call walks only its entries — every
/// page carries a `path_prefix`, one walk per entry — echoes the scope, and
/// reports the entry that selected no chunks.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scoped_hotspots_fetch_only_the_scope() {
    let (_index, search, dir) = fixture();
    let state = state(&search, &dir);
    let body = ok(complexity_hotspots(
        &state,
        req::<HotspotsRequest>(json!({
            "index_id": "idx", "top_n": 5, "paths": ["src/a.rs", "src/missing.rs"]
        })),
    )
    .await);

    assert_eq!(body["scoped"], true);
    assert_eq!(
        body["scope"]["paths"],
        json!(["src/a.rs", "src/missing.rs"])
    );
    assert_eq!(body["scope"]["unmatched"], json!(["src/missing.rs"]));
    let files: Vec<&str> = body["hotspots"]
        .as_array()
        .expect("hotspots")
        .iter()
        .filter_map(|h| h["file"].as_str())
        .collect();
    assert_eq!(files, vec!["src/a.rs"]);

    let calls = search.calls();
    assert!(calls
        .iter()
        .all(|(m, p)| m == "search.chunks.list" && p["path_prefix"].is_string()));
    assert_eq!(calls.len(), 2, "page fetches are bounded by the scope size");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scoped_quality_grades_only_the_scope() {
    let (_index, search, dir) = fixture();
    let state = state(&search, &dir);
    let body = ok(quality_report(
        &state,
        req::<QualityRequest>(json!({ "index_id": "idx", "path_prefix": "src/b.rs" })),
    )
    .await);
    let chunks: Vec<crate::types::CodeChunk> = corpus()
        .into_iter()
        .filter(|c| c["file"] == "src/b.rs")
        .map(|c| serde_json::from_value(c).expect("chunk"))
        .collect();
    let mut expected = serde_json::to_value(quality::aggregate_quality(&chunks)).expect("enc");
    expected["scoped"] = json!(true);
    expected["scope"] = json!({ "paths": [], "path_prefix": "src/b.rs", "unmatched": [] });
    assert_eq!(body, expected);
}

/// `paths: []` is a scope that selects nothing, never the corpus.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_empty_paths_scope_walks_nothing() {
    let (_index, search, dir) = fixture();
    let state = state(&search, &dir);
    let body = ok(smells(
        &state,
        req::<SmellsRequest>(json!({ "index_id": "idx", "paths": [] })),
    )
    .await);
    assert_eq!(body["scoped"], true);
    assert_eq!(body["total"], 0);
    assert!(search.calls().is_empty());
}

/// Error arm: one failing scope walk fails the request; a partial scope is
/// never answered as the whole scope.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_scope_walk_fails_the_request() {
    let (index, search, dir) = fixture();
    index.chunks_fail.store(true, Ordering::SeqCst);
    let state = state(&search, &dir);
    let Err(err) = smells(
        &state,
        req::<SmellsRequest>(json!({ "index_id": "idx", "paths": ["src/a.rs"] })),
    )
    .await
    else {
        panic!("a failed walk is an error");
    };
    assert_eq!(err.kind, ApiErrorKind::BadGateway);
    assert!(err.message.contains("src/a.rs"), "{}", err.message);
}

/// Acceptance tests 5: the unscoped request shape returns the pre-#10781
/// body — computed here with the pre-#10781 code path — plus only the
/// additive `scoped`/`cache` keys, and a warm-cache answer equals the cold
/// one.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unscoped_results_match_the_uncached_computation() {
    let (_index, search, dir) = fixture();
    let state = state(&search, &dir);
    let chunks: Vec<crate::types::CodeChunk> = corpus()
        .into_iter()
        .map(|c| serde_json::from_value(c).expect("chunk"))
        .collect();

    let quality_golden = serde_json::to_value(quality::aggregate_quality(&chunks)).expect("enc");
    let hotspots_golden = json!({
        "index_id": "idx", "top_n": 1,
        "hotspots": quality::complexity_hotspots(&chunks, 1),
    });
    let smelly = quality::smelly_chunks_with_smells(&chunks);
    let smells_golden = json!({
        "index_id": "idx", "total": smelly.len(), "offset": 0, "limit": 500,
        "returned": smelly.len(), "truncated": false,
        "chunks": smelly.iter()
            .map(|(c, s)| json!({
                "id": c.id, "file": c.file, "start_line": c.start_line,
                "end_line": c.end_line, "match_reason": c.match_reason, "smells": s,
            }))
            .collect::<Vec<_>>(),
    });

    for round in ["cold", "warm"] {
        let q = ok(quality_report(&state, req(json!({ "index_id": "idx" }))).await);
        assert_eq!(q["scoped"], false);
        assert_eq!(q["cache"]["status"], "fresh", "{round}");
        assert_eq!(legacy(q), quality_golden, "{round} quality");

        let h =
            ok(complexity_hotspots(&state, req(json!({ "index_id": "idx", "top_n": 1 }))).await);
        assert_eq!(legacy(h), hotspots_golden, "{round} hotspots");

        let s = ok(smells(&state, req(json!({ "index_id": "idx" }))).await);
        assert_eq!(legacy(s), smells_golden, "{round} smells");
    }
    assert_eq!(
        chunk_calls(&search),
        1,
        "six unscoped calls, one corpus walk"
    );
}
