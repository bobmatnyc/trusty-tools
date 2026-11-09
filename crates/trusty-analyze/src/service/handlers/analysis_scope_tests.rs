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
    let pages: Vec<&(String, Value)> = calls
        .iter()
        .filter(|(m, _)| m == "search.chunks.list")
        .collect();
    assert!(pages.iter().all(|(_, p)| p["path_prefix"].is_string()));
    assert_eq!(pages.len(), 2, "page fetches are bounded by the scope size");
    assert_eq!(calls.len(), 3, "plus one status read to check the scope");
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

/// trusty-search's `normalize_prefix` as a fake: an absolute prefix under the
/// root, spelled directly or through a symlink, loses the root; `""` then
/// matches every chunk. The raw prefix is echoed, as the daemon does.
fn daemon_normalize(prefix: &str, root: &std::path::Path) -> String {
    let path = std::path::Path::new(prefix);
    if !prefix.starts_with('/') {
        return prefix.to_string();
    }
    if let Ok(rel) = path.strip_prefix(root) {
        return rel.to_string_lossy().into_owned();
    }
    let canonical_root = std::fs::canonicalize(root).ok();
    path.ancestors()
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .find(|a| std::fs::canonicalize(a).ok() == canonical_root)
        .and_then(|a| path.strip_prefix(a).ok())
        .map_or(prefix.to_string(), |r| r.to_string_lossy().into_owned())
}

/// A fake trusty-search rooted at `root` that filters like the real daemon;
/// `search.index.status` reports `chunk_count`.
fn rooted_search(root: std::path::PathBuf, chunk_count: usize) -> FakeSearchSocket {
    FakeSearchSocket::serve(move |method, params| match method {
        "search.index.status" => Ok(json!({
            "root_path": root.to_string_lossy(), "chunk_count": chunk_count,
            "last_indexed": "2026-11-07T00:00:00Z", "status": "ready"
        })),
        "search.chunks.list" => {
            let raw = params["path_prefix"].as_str().unwrap_or_default();
            let rel = daemon_normalize(raw, &root);
            let chunks: Vec<Value> = corpus()
                .into_iter()
                .filter(|c| {
                    let file = c["file"].as_str().unwrap_or_default();
                    rel.is_empty() || file == rel || file.starts_with(&format!("{rel}/"))
                })
                .collect();
            Ok(
                json!({ "total": chunks.len(), "chunks": chunks, "next_cursor": null,
                "path_prefix": raw }),
            )
        }
        other => Err((-32601, format!("unexpected {other}"))),
    })
}

/// #10781 error arm: an absolute entry resolving to the index root — direct,
/// trailing `/`, trailing `/.`, or through a symlink — is `invalid_params`
/// and walks nothing; an absolute entry under the root still works.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_absolute_scope_naming_the_root_is_refused() {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path().join("repo");
    std::fs::create_dir_all(&root).expect("root");
    let link = tmp.path().join("link");
    std::os::unix::fs::symlink(&root, &link).expect("symlink");
    let search = rooted_search(root.clone(), 2);
    let dir = TempDir::new().expect("tempdir");
    let state = state(&search, &dir);

    let r = root.to_string_lossy().into_owned();
    let spellings = [
        r.clone(),
        format!("{r}/"),
        format!("{r}/."),
        link.to_string_lossy().into(),
    ];
    for entry in spellings {
        for field in ["paths", "path_prefix"] {
            let value = if field == "paths" {
                json!([entry])
            } else {
                json!(entry)
            };
            let res = complexity_hotspots(
                &state,
                req::<HotspotsRequest>(json!({ "index_id": "idx", field: value })),
            )
            .await;
            let Err(err) = res else {
                panic!("{field} {entry:?} names the root and is refused");
            };
            assert_eq!(
                err.kind,
                ApiErrorKind::BadRequest,
                "{field} {entry:?}: {}",
                err.message
            );
        }
    }
    assert_eq!(chunk_calls(&search), 0, "a root-naming scope walks nothing");

    let body = ok(complexity_hotspots(
        &state,
        req::<HotspotsRequest>(json!({ "index_id": "idx", "paths": [format!("{r}/src/a.rs")] })),
    )
    .await);
    assert_eq!(body["hotspots"].as_array().map(Vec::len), Some(1));
}

/// #10781: an absolute entry with `..` is `invalid_params` and walks nothing.
/// `<tmp>/s/..` with `s -> <root>/src` reads as `<tmp>` lexically, but the
/// daemon resolves the symlink first and lands on the root. The status count
/// (3) differs from the walk total (2), so the whole-index backstop misses it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_absolute_scope_with_a_parent_dir_segment_is_refused() {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path().join("repo");
    std::fs::create_dir_all(root.join("src")).expect("root/src");
    let outside = tmp.path().join("outside");
    std::fs::create_dir_all(&outside).expect("outside");
    let link = outside.join("s");
    std::os::unix::fs::symlink(root.join("src"), &link).expect("symlink");
    let search = rooted_search(root.clone(), 3);
    let dir = TempDir::new().expect("tempdir");
    let state = state(&search, &dir);

    let entry = format!("{}/..", link.to_string_lossy());
    for field in ["paths", "path_prefix"] {
        let value = if field == "paths" {
            json!([entry])
        } else {
            json!(entry)
        };
        let res = complexity_hotspots(
            &state,
            req::<HotspotsRequest>(json!({ "index_id": "idx", field: value })),
        )
        .await;
        let Err(err) = res else {
            panic!("{field} {entry:?} resolves to the root and is refused");
        };
        assert_eq!(
            err.kind,
            ApiErrorKind::BadRequest,
            "{field}: {}",
            err.message
        );
        assert!(err.message.contains("`..`"), "{field}: {}", err.message);
    }
    assert_eq!(chunk_calls(&search), 0, "a parent-dir scope walks nothing");
}

/// #10781 backstop: a prefix the daemon widens to the whole index — here an
/// alias the analyzer cannot see — stops at the first page as
/// `invalid_params`, before the walk pages the corpus.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_scope_the_daemon_widens_to_the_corpus_is_refused() {
    let search = FakeSearchSocket::serve(|method, params| match method {
        "search.index.status" => Ok(json!({
            "root_path": "/abs/repo", "chunk_count": 2,
            "last_indexed": "2026-11-07T00:00:00Z", "status": "ready"
        })),
        "search.chunks.list" => {
            // One chunk per page; `workspace` matches everything.
            let page = usize::from(params["after"].as_str().is_some_and(|a| !a.is_empty()));
            let chunk = corpus().swap_remove(page);
            let next = if page == 0 {
                json!("a:1:4")
            } else {
                json!(null)
            };
            Ok(json!({ "total": 2, "chunks": [chunk], "next_cursor": next,
                "path_prefix": params["path_prefix"] }))
        }
        other => Err((-32601, format!("unexpected {other}"))),
    });
    let dir = TempDir::new().expect("tempdir");
    let state = state(&search, &dir);
    let res = smells(
        &state,
        req::<SmellsRequest>(json!({ "index_id": "idx", "path_prefix": "workspace" })),
    )
    .await;
    let Err(err) = res else {
        panic!("a scope covering the whole index is refused");
    };
    assert_eq!(err.kind, ApiErrorKind::BadRequest, "{}", err.message);
    assert!(
        err.message.contains("selects all 2 chunks"),
        "{}",
        err.message
    );
    assert_eq!(chunk_calls(&search), 1, "the walk stops at the first page");
}

/// A fake whose corpus walk stalls past a 50 ms `warm_wait`, on its own
/// runtime, plus a state whose cache uses that bound.
fn slow_corpus() -> (crate::core::test_utils::IsolatedFake, TempDir) {
    let fake = crate::core::test_utils::IsolatedFake::serve(|method, _| match method {
        "search.index.status" => Ok(json!({
            "chunk_count": 2, "last_indexed": "2026-11-07T00:00:00Z", "status": "ready"
        })),
        _ => {
            crate::core::test_utils::stall_reply(std::time::Duration::from_millis(400));
            Ok(json!({ "total": 2, "chunks": corpus(), "next_cursor": null }))
        }
    });
    (fake, TempDir::new().expect("tempdir"))
}

fn slow_state(search: &FakeSearchSocket, dir: &TempDir) -> AnalyzerAppState {
    use crate::service::analysis_cache::{AnalysisCache, CacheConfig};
    state(search, dir).with_analysis_cache(AnalysisCache::with_config(CacheConfig {
        warm_wait: std::time::Duration::from_millis(50),
        ..CacheConfig::default()
    }))
}

/// #10781 error arm (a): an old-shape cold call returns the full result even
/// when the walk outlasts `warm_wait` — never `-32007` — and fills the cache.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_old_shape_cold_call_waits_for_the_walk() {
    let (fake, dir) = slow_corpus();
    let state = slow_state(fake.get(), &dir);
    let chunks: Vec<crate::types::CodeChunk> = corpus()
        .into_iter()
        .map(|c| serde_json::from_value(c).expect("chunk"))
        .collect();
    let golden = serde_json::to_value(quality::aggregate_quality(&chunks)).expect("enc");

    let body = ok(quality_report(&state, req(json!({ "index_id": "idx" }))).await);
    assert_eq!(body["cache"]["status"], "fresh");
    assert_eq!(legacy(body), golden);
    let again = ok(quality_report(&state, req(json!({ "index_id": "idx" }))).await);
    assert_eq!(again["cache"]["status"], "fresh");
    assert_eq!(
        chunk_calls(fake.get()),
        1,
        "the first call filled the cache"
    );
}

/// #10781 (b): a caller that opts in with `allow_warming` still gets the
/// retryable `-32007` on a cold walk past `warm_wait`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_opted_in_cold_call_answers_warming() {
    let (fake, dir) = slow_corpus();
    let state = slow_state(fake.get(), &dir);
    let Err(err) = quality_report(
        &state,
        req(json!({ "index_id": "idx", "allow_warming": true })),
    )
    .await
    else {
        panic!("an opted-in cold call past warm_wait is not served");
    };
    assert_eq!(err.kind, ApiErrorKind::Warming, "{}", err.message);
}

/// #10781 error arm (c): two concurrent cold old-shape calls both get the
/// result from exactly one corpus walk.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_cold_calls_share_one_walk() {
    let (fake, dir) = slow_corpus();
    let state = slow_state(fake.get(), &dir);
    let (q, s) = tokio::join!(
        quality_report(&state, req(json!({ "index_id": "idx" }))),
        smells(&state, req(json!({ "index_id": "idx" }))),
    );
    assert_eq!(ok(q)["scoped"], false);
    assert_eq!(ok(s)["scoped"], false);
    assert_eq!(chunk_calls(fake.get()), 1, "one walk served both callers");
}
