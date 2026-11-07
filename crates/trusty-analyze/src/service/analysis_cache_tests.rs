//! Tests for the per-index analysis cache (#10781).

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use super::*;
use crate::core::fake_search::FakeSearchSocket;
use crate::core::test_utils::IsolatedFake;
use crate::service::events::ApiErrorKind;

/// The fake index's mutable state: what `search.index.status` reports and
/// whether each method fails.
#[derive(Default)]
pub(crate) struct FakeIndex {
    pub chunk_count: AtomicU64,
    pub status_fails: AtomicBool,
    pub chunks_fail: AtomicBool,
    pub chunks_stall: AtomicBool,
}

/// Two code chunks; one branchy enough to rank first.
pub(crate) fn corpus() -> Vec<serde_json::Value> {
    let branchy =
        "fn hot(x: i32) -> i32 {\n    if x > 1 { if x > 2 { if x > 3 { return 3; } } }\n    \
                   match x { 0 => 0, 1 => 1, _ => 2 }\n}\n";
    vec![
        serde_json::json!({"id": "a:1:4", "file": "src/a.rs", "start_line": 1, "end_line": 4,
            "content": branchy, "score": 0.0, "match_reason": "enumerate"}),
        serde_json::json!({"id": "b:1:1", "file": "src/b.rs", "start_line": 1, "end_line": 1,
            "content": "fn cold() {}\n", "score": 0.0, "match_reason": "enumerate"}),
    ]
}

/// Serve `search.index.status` and an unscoped/scoped `search.chunks.list`
/// over [`corpus`], driven by `index`.
pub(crate) fn serve_index(index: Arc<FakeIndex>) -> FakeSearchSocket {
    FakeSearchSocket::serve(index_reply(index))
}

/// The reply behind [`serve_index`]; a stalling test serves it isolated.
fn index_reply(
    index: Arc<FakeIndex>,
) -> impl Fn(&str, &serde_json::Value) -> crate::core::fake_search::Reply + Send + Sync + 'static {
    move |method, params| match method {
        "search.index.status" => {
            if index.status_fails.load(Ordering::SeqCst) {
                return Err((-32603, "status broke".to_string()));
            }
            Ok(serde_json::json!({
                "chunk_count": index.chunk_count.load(Ordering::SeqCst),
                "last_indexed": "2026-11-07T00:00:00Z", "status": "ready"
            }))
        }
        "search.chunks.list" => {
            if index.chunks_stall.load(Ordering::SeqCst) {
                crate::core::test_utils::stall_reply(Duration::from_millis(400));
            }
            if index.chunks_fail.load(Ordering::SeqCst) {
                return Err((-32603, "corpus unreadable".to_string()));
            }
            let prefix = params["path_prefix"].as_str();
            let chunks: Vec<serde_json::Value> = corpus()
                .into_iter()
                .filter(|c| prefix.is_none_or(|p| c["file"] == p))
                .collect();
            let mut body = serde_json::json!({
                "total": chunks.len(), "chunks": chunks, "next_cursor": null
            });
            if let Some(p) = prefix {
                body["path_prefix"] = serde_json::json!(p);
            }
            Ok(body)
        }
        other => Err((-32601, format!("unexpected {other}"))),
    }
}

pub(crate) fn chunk_calls(search: &FakeSearchSocket) -> usize {
    search
        .methods()
        .iter()
        .filter(|m| *m == "search.chunks.list")
        .count()
}

fn cache(warm_wait: Duration) -> AnalysisCache {
    AnalysisCache::with_config(CacheConfig {
        warm_wait,
        ..CacheConfig::default()
    })
}

async fn fresh(cache: &AnalysisCache, client: &TrustySearchClient) -> CacheHit {
    match cache.get(client, "idx").await {
        Ok(hit) => hit,
        Err(e) => panic!("expected a hit, got {:?}: {}", e.kind, e.message),
    }
}

async fn failure(cache: &AnalysisCache, client: &TrustySearchClient) -> ApiError {
    match cache.get(client, "idx").await {
        Ok(hit) => panic!("expected an error, got a hit: {}", hit.meta()),
        Err(e) => e,
    }
}

/// Acceptance test 2: the second unscoped call makes zero page fetches.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn second_call_is_served_without_a_page_fetch() {
    let index = Arc::new(FakeIndex::default());
    index.chunk_count.store(2, Ordering::SeqCst);
    let search = serve_index(Arc::clone(&index));
    let client = TrustySearchClient::new(search.path());
    let cache = cache(Duration::from_secs(5));

    let first = fresh(&cache, &client).await;
    assert!(first.stale_reason.is_none(), "{}", first.meta());
    assert_eq!(chunk_calls(&search), 1);

    let second = fresh(&cache, &client).await;
    assert!(second.stale_reason.is_none());
    assert_eq!(second.meta()["status"], "fresh");
    assert_eq!(chunk_calls(&search), 1, "a cache hit fetches no page");
}

/// Acceptance test 3: after the fingerprint changes, the old result is
/// served only marked stale, and the refresh replaces it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_changed_fingerprint_is_never_served_as_fresh() {
    let index = Arc::new(FakeIndex::default());
    index.chunk_count.store(2, Ordering::SeqCst);
    let search = serve_index(Arc::clone(&index));
    let client = TrustySearchClient::new(search.path());
    let cache = cache(Duration::from_secs(5));
    fresh(&cache, &client).await;

    index.chunk_count.store(3, Ordering::SeqCst);
    let changed = fresh(&cache, &client).await;
    assert_eq!(changed.meta()["status"], "stale");
    assert!(changed
        .stale_reason
        .as_deref()
        .is_some_and(|r| r.contains("index changed")));

    // The stale serve started a refresh; once it lands the entry is current.
    for _ in 0..100 {
        let hit = fresh(&cache, &client).await;
        if hit.stale_reason.is_none() {
            assert_eq!(hit.entry.fingerprint.chunk_count, Some(3));
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("the refresh never replaced the stale entry");
}

/// Error arm: a status read that fails never lets an entry read as fresh.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn status_failure_marks_the_entry_stale() {
    let index = Arc::new(FakeIndex::default());
    index.chunk_count.store(2, Ordering::SeqCst);
    let search = serve_index(Arc::clone(&index));
    let client = TrustySearchClient::new(search.path());
    let cache = cache(Duration::from_secs(5));
    fresh(&cache, &client).await;

    index.status_fails.store(true, Ordering::SeqCst);
    let hit = fresh(&cache, &client).await;
    assert_eq!(hit.meta()["status"], "stale");
    assert!(hit
        .stale_reason
        .as_deref()
        .is_some_and(|r| r.contains("index status unavailable")));
    assert_eq!(chunk_calls(&search), 1, "an unkeyable state starts no walk");
}

/// Error arm: a cold cache whose status read fails is an error, not a walk.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cold_status_failure_is_an_error_and_starts_no_walk() {
    let index = Arc::new(FakeIndex::default());
    index.status_fails.store(true, Ordering::SeqCst);
    let search = serve_index(Arc::clone(&index));
    let client = TrustySearchClient::new(search.path());
    let err = failure(&cache(Duration::from_secs(5)), &client).await;
    assert_eq!(err.kind, ApiErrorKind::BadGateway);
    assert_eq!(chunk_calls(&search), 0);
}

/// A cold call whose walk outlasts `warm_wait` answers the retryable
/// warming error; the walk finishes in the background.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_cold_walk_past_warm_wait_answers_warming() {
    let index = Arc::new(FakeIndex::default());
    index.chunk_count.store(2, Ordering::SeqCst);
    index.chunks_stall.store(true, Ordering::SeqCst);
    let isolated = IsolatedFake::serve(index_reply(Arc::clone(&index)));
    let search = isolated.get();
    let client = TrustySearchClient::new(search.path());
    let cache = cache(Duration::from_millis(50));

    let err = failure(&cache, &client).await;
    assert_eq!(err.kind, ApiErrorKind::Warming, "{}", err.message);
    let wire = trusty_common::uds::server::RpcError::from(err);
    assert_eq!(wire.code, crate::service::events::CODE_ANALYSIS_WARMING);
    assert_eq!(
        wire.data.as_ref().map(|d| d["retryable"].clone()),
        Some(serde_json::json!(true))
    );

    for _ in 0..100 {
        if let Ok(hit) = cache.get(&client, "idx").await {
            assert!(hit.stale_reason.is_none());
            assert_eq!(
                chunk_calls(&search),
                1,
                "one background walk served both calls"
            );
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("the background walk never completed");
}

/// Error arm: a refresh whose walk fails is an upstream error naming the
/// failure — never an empty result — and the cooldown stops a retry storm.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_refresh_is_an_error_not_an_empty_result() {
    let index = Arc::new(FakeIndex::default());
    index.chunk_count.store(2, Ordering::SeqCst);
    index.chunks_fail.store(true, Ordering::SeqCst);
    let search = serve_index(Arc::clone(&index));
    let client = TrustySearchClient::new(search.path());
    let cache = cache(Duration::from_secs(5));

    let err = failure(&cache, &client).await;
    assert_eq!(err.kind, ApiErrorKind::BadGateway);
    assert!(err.message.contains("corpus unreadable"), "{}", err.message);
    let again = failure(&cache, &client).await;
    assert_eq!(again.kind, ApiErrorKind::BadGateway);
    assert_eq!(
        chunk_calls(&search),
        1,
        "the cooldown holds the second walk"
    );
}

/// An entry older than `max_age` is stale even with a matching fingerprint.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_entry_past_max_age_is_stale() {
    let index = Arc::new(FakeIndex::default());
    index.chunk_count.store(2, Ordering::SeqCst);
    let search = serve_index(Arc::clone(&index));
    let client = TrustySearchClient::new(search.path());
    let cache = AnalysisCache::with_config(CacheConfig {
        max_age: Duration::ZERO,
        ..CacheConfig::default()
    });
    fresh(&cache, &client).await;
    tokio::time::sleep(Duration::from_millis(5)).await;
    let hit = fresh(&cache, &client).await;
    assert!(hit
        .stale_reason
        .as_deref()
        .is_some_and(|r| r.contains("maximum age")));
}
