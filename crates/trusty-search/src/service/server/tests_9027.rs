//! #9027: the all-index fan-out never waits on a slow index, and embeds the
//! query once.
//!
//! Why: one idle-evicted index rehydrating its corpus held `POST /search` for
//! ~26 s, and the query was embedded once for routing plus once per index.
//! What: drives `global_search_report` directly. The slow index is a held
//! indexer write lock — the same await point an index blocked on its own lock
//! or rehydrate reaches — on a paused clock, so the deadline costs no real
//! time. The embed tests count the shared embedder's single-text calls (ingest
//! uses the batch call, which is not counted).
//! Test: this module.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use super::*;
use crate::core::embed::Embedder;
use crate::core::indexer::CodeIndexer;
use crate::core::registry::{IndexHandle, IndexId, IndexRegistry};
use crate::core::store::UsearchStore;

const DIM: usize = 8;

/// An embedder that counts query embeds and can be told to fail them.
#[derive(Default)]
struct CountingEmbedder {
    query_embeds: AtomicUsize,
    fail: bool,
}

#[async_trait::async_trait]
impl Embedder for CountingEmbedder {
    async fn embed(&self, _text: &str) -> anyhow::Result<Vec<f32>> {
        self.query_embeds.fetch_add(1, Ordering::SeqCst);
        if self.fail {
            anyhow::bail!("embedder sidecar could not spawn");
        }
        Ok(vec![0.5; DIM])
    }
    async fn embed_batch(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|_| vec![0.5; DIM]).collect())
    }
    fn dimension(&self) -> usize {
        DIM
    }
}

/// Register `id` holding one file; with `embedder`, the full hybrid pipeline.
async fn add_index(
    registry: &IndexRegistry,
    id: &str,
    embedder: Option<Arc<CountingEmbedder>>,
) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut indexer = CodeIndexer::new(id, tmp.path());
    if let Some(e) = embedder {
        let store = Arc::new(UsearchStore::new(DIM).expect("usearch"));
        indexer = indexer.with_components(e, store);
    }
    indexer
        .index_files_batch(&[("src/a.rs".into(), "fn shared_symbol() {}\n".into())])
        .await
        .expect("index batch");
    registry.register(IndexHandle::bare(
        IndexId::new(id.to_string()),
        Arc::new(tokio::sync::RwLock::new(indexer)),
        tmp.path().to_path_buf(),
    ));
    tmp
}

/// Built from JSON so this file compiles against the request type whatever
/// optional fields it carries.
fn request(query: &str) -> GlobalSearchRequest {
    serde_json::from_value(serde_json::json!({ "query": query, "top_k": 5 })).expect("request")
}

/// Why: Fail-Open Check — an index that cannot answer in time is skipped and
/// COUNTED; the response never claims every index was searched.
/// What: holds the slow index's write lock, runs the fan-out under an outer
/// 60 s bound (virtual), and asserts the fast index answered, the slow one is
/// named in `deadline_skipped_index_ids` and counted in
/// `rehydrating_indexes_skipped`, and `indexes_searched` excludes it.
/// Test: this test.
#[tokio::test(start_paused = true)]
async fn global_search_skips_an_index_that_misses_the_deadline() {
    let registry = IndexRegistry::new();
    let _fast = add_index(&registry, "fast", None).await;
    let _slow = add_index(&registry, "slow", None).await;
    let state = Arc::new(SearchAppState::new(registry));
    let slow = state
        .registry
        .get(&IndexId::new("slow".to_string()))
        .expect("slow");
    let _held = slow.indexer.write().await;

    let body = tokio::time::timeout(
        Duration::from_secs(60),
        super::search_global::global_search_report(&state, request("shared_symbol")),
    )
    .await
    .expect("the fan-out must answer without waiting on the slow index")
    .expect("200");

    assert_eq!(body["total_indexes"], serde_json::json!(2), "{body}");
    assert_eq!(
        body["indexes_searched"],
        serde_json::json!(["fast"]),
        "{body}"
    );
    assert_eq!(
        body["rehydrating_indexes_skipped"],
        serde_json::json!(1),
        "{body}"
    );
    assert_eq!(
        body["deadline_skipped_index_ids"],
        serde_json::json!(["slow"]),
        "{body}"
    );
    // #9027 critic r1: a deadline skip is visible as `partial` without the
    // caller summing every skip counter.
    assert_eq!(
        body["deadline_indexes_skipped"],
        serde_json::json!(1),
        "{body}"
    );
    assert_eq!(body["partial"], serde_json::json!(true), "{body}");
    let results = body["results"].as_array().expect("results");
    assert!(
        !results.is_empty(),
        "the fast index's hit is returned: {body}"
    );
    assert!(results
        .iter()
        .all(|r| r["index_id"] == serde_json::json!("fast")));
}

/// Why: one embedder call per all-index request; every index reuses it.
/// What: three indexes share one counting embedder; one fan-out must embed
/// the query exactly once and still search all three.
/// Test: this test.
#[tokio::test]
async fn global_search_embeds_the_query_once_for_every_index() {
    let embedder = Arc::new(CountingEmbedder::default());
    let registry = IndexRegistry::new();
    let mut dirs = Vec::new();
    for id in ["a", "b", "c"] {
        dirs.push(add_index(&registry, id, Some(Arc::clone(&embedder))).await);
    }
    let state = Arc::new(SearchAppState::new(registry));
    embedder.query_embeds.store(0, Ordering::SeqCst);

    let body = super::search_global::global_search_report(&state, request("shared symbol"))
        .await
        .expect("200");

    assert_eq!(
        body["indexes_searched"],
        serde_json::json!(["a", "b", "c"]),
        "{body}"
    );
    assert_eq!(
        embedder.query_embeds.load(Ordering::SeqCst),
        1,
        "one embed for the whole fan-out"
    );
    assert_eq!(body["partial"], serde_json::json!(false), "{body}");

    // The fan-out embed goes through the query-embed cache (#5024), so a
    // repeated query costs no embedder call at all.
    super::search_global::global_search_report(&state, request("shared symbol"))
        .await
        .expect("200");
    assert_eq!(
        embedder.query_embeds.load(Ordering::SeqCst),
        1,
        "a repeated query is served from the query-embed cache"
    );
}

/// Why: a failed fan-out embed must degrade every index to lexical at once,
/// not retry the same failing embedder once per index (#8348).
/// What: a failing embedder shared by two indexes; the fan-out still answers
/// from the lexical lane and calls the embedder exactly once.
/// Test: this test.
#[tokio::test]
async fn a_precomputed_embed_failure_degrades_without_re_embedding() {
    let embedder = Arc::new(CountingEmbedder {
        fail: true,
        ..Default::default()
    });
    let registry = IndexRegistry::new();
    let _a = add_index(&registry, "a", Some(Arc::clone(&embedder))).await;
    let _b = add_index(&registry, "b", Some(Arc::clone(&embedder))).await;
    let state = Arc::new(SearchAppState::new(registry));
    embedder.query_embeds.store(0, Ordering::SeqCst);

    let body = super::search_global::global_search_report(&state, request("shared_symbol"))
        .await
        .expect("200");

    assert_eq!(
        body["indexes_searched"],
        serde_json::json!(["a", "b"]),
        "{body}"
    );
    assert!(
        !body["results"].as_array().expect("results").is_empty(),
        "{body}"
    );
    assert_eq!(embedder.query_embeds.load(Ordering::SeqCst), 1);
}

/// An embedder whose query embed takes the given time; 5 s is a cold sidecar
/// respawn.
struct SlowEmbedder(Duration);

#[async_trait::async_trait]
impl Embedder for SlowEmbedder {
    async fn embed(&self, _text: &str) -> anyhow::Result<Vec<f32>> {
        tokio::time::sleep(self.0).await;
        Ok(vec![0.5; DIM])
    }
    async fn embed_batch(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|_| vec![0.5; DIM]).collect())
    }
    fn dimension(&self) -> usize {
        DIM
    }
}

/// Register `id` over a real redb corpus holding one file, on the full hybrid
/// pipeline with `embedder`.
async fn add_corpus_index(
    registry: &IndexRegistry,
    id: &str,
    embedder: Arc<dyn Embedder>,
) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().expect("tempdir");
    let corpus = Arc::new(
        crate::core::corpus::CorpusStore::open(&tmp.path().join("index.redb")).expect("corpus"),
    );
    let store = Arc::new(UsearchStore::new(DIM).expect("usearch"));
    let mut indexer = CodeIndexer::new(id, tmp.path()).with_components(embedder, store);
    indexer.set_corpus_store(corpus);
    indexer
        .index_files_batch(&[("src/a.rs".into(), "fn shared_symbol() {}\n".into())])
        .await
        .expect("index batch");
    registry.register(IndexHandle::bare(
        IndexId::new(id.to_string()),
        Arc::new(tokio::sync::RwLock::new(indexer)),
        tmp.path().to_path_buf(),
    ));
    tmp
}

/// Why: a cold embedder respawn (2–15 s) used to spend the whole fan-out
/// deadline before any index was searched, so every corpus-backed index was
/// skipped and the search returned nothing (code-critic r1 on #9027).
/// What: a 5 s embedder and two corpus-backed, resident indexes on a paused
/// clock. The embed must time out, every index must answer from the lexical
/// lane, and the response must say so (`query_embed`, `embed_degraded`,
/// `partial`).
/// Test: this test.
#[tokio::test(start_paused = true)]
async fn a_slow_embed_degrades_to_lexical_and_still_searches_every_index() {
    let registry = IndexRegistry::new();
    let _one = add_corpus_index(
        &registry,
        "one",
        Arc::new(SlowEmbedder(Duration::from_secs(5))),
    )
    .await;
    let _two = add_corpus_index(
        &registry,
        "two",
        Arc::new(SlowEmbedder(Duration::from_secs(5))),
    )
    .await;
    let state = Arc::new(SearchAppState::new(registry));

    let body = tokio::time::timeout(
        Duration::from_secs(60),
        super::search_global::global_search_report(&state, request("shared_symbol")),
    )
    .await
    .expect("the fan-out answers")
    .expect("200");

    assert!(
        !body["results"].as_array().expect("results").is_empty(),
        "a slow embed must not cost the fan-out its results: {body}"
    );
    assert_eq!(
        body["indexes_searched"],
        serde_json::json!(["one", "two"]),
        "{body}"
    );
    assert_eq!(body["deadline_indexes_skipped"], serde_json::json!(0));
    assert_eq!(body["query_embed"], serde_json::json!("timed_out"));
    assert_eq!(body["embed_degraded"], serde_json::json!(true));
    assert_eq!(body["partial"], serde_json::json!(true), "{body}");
}

/// Why: the fan-out deadline used to start before the query embed, so an
/// embed that answered inside its own timeout still spent the deadline and
/// every index was skipped (code-critic r1 HIGH on #9027).
/// What: an 800 ms embed (inside the 1 s embed timeout) and a 500 ms fan-out
/// deadline on a paused clock. Both resident indexes must answer with the
/// embedded vector; none may be skipped.
/// Test: this test.
#[tokio::test(start_paused = true)]
async fn the_fan_out_deadline_starts_after_the_query_embed() {
    let registry = IndexRegistry::new();
    let slow: Arc<dyn Embedder> = Arc::new(SlowEmbedder(Duration::from_millis(800)));
    let _one = add_corpus_index(&registry, "one", Arc::clone(&slow)).await;
    let _two = add_corpus_index(&registry, "two", slow).await;
    let state = Arc::new(SearchAppState::new(registry));
    let req: GlobalSearchRequest = serde_json::from_value(serde_json::json!({
        "query": "shared_symbol", "top_k": 5, "per_index_deadline_ms": 500,
    }))
    .expect("request");

    let body = tokio::time::timeout(
        Duration::from_secs(60),
        super::search_global::global_search_report(&state, req),
    )
    .await
    .expect("the fan-out answers")
    .expect("200");

    assert_eq!(
        body["indexes_searched"],
        serde_json::json!(["one", "two"]),
        "the embed must not spend the fan-out deadline: {body}"
    );
    assert_eq!(body["deadline_skipped_index_ids"], serde_json::json!([]));
    assert_eq!(body["query_embed"], serde_json::json!("embedded"), "{body}");
    assert_eq!(body["partial"], serde_json::json!(false), "{body}");
}

/// Why: routing decides which indexes a fan-out touches; a rehydrate kicked
/// for an index routing then dropped spends an O(corpus) scan and its memory
/// on nothing (code-critic r1 on #9027).
/// What: two evicted corpus-backed indexes; threshold routing keeps the one
/// whose context embedding matches the query. After the fan-out the dropped
/// index must still be evicted, with no rehydrate started.
/// Test: this test.
#[tokio::test]
async fn global_search_kicks_rehydrates_only_for_routed_indexes() {
    let embedder: Arc<dyn Embedder> = Arc::new(CountingEmbedder::default());
    let registry = IndexRegistry::new();
    let _kept = add_corpus_index(&registry, "kept", Arc::clone(&embedder)).await;
    let _dropped = add_corpus_index(&registry, "dropped", Arc::clone(&embedder)).await;
    let state = Arc::new(SearchAppState::new(registry));
    for (id, ctx) in [("kept", 0.5_f32), ("dropped", -0.5)] {
        let handle = state.registry.get(&IndexId::new(id)).expect("registered");
        *handle.context_embedding.write().await = Some(vec![ctx; DIM]);
        assert!(handle.indexer.read().await.reclaim_memory_now().await > 0);
    }
    let req: GlobalSearchRequest = serde_json::from_value(serde_json::json!({
        "query": "shared_symbol", "top_k": 5, "routing": "threshold", "routing_threshold": 0.3,
    }))
    .expect("request");

    let body = super::search_global::global_search_report(&state, req)
        .await
        .expect("200");
    assert_eq!(
        body["indexes_searched"],
        serde_json::json!(["kept"]),
        "{body}"
    );

    // A kicked rehydrate of a one-file corpus finishes in milliseconds.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let dropped = state
        .registry
        .get(&IndexId::new("dropped"))
        .expect("registered");
    assert!(
        dropped.indexer.read().await.corpus_evicted(),
        "routing dropped this index, so nothing may rehydrate it"
    );
}

/// Why: Fail-Open Check — an index whose search errored contributed no lane,
/// so the answer is not complete; `partial` must say so, the same as for a
/// deadline skip (code-critic r2 on #9027).
/// What: one healthy index and one detached for delete, whose search refuses
/// with `IndexDeleted` (an error no other skip reason classifies). The fan-out
/// answers from the healthy index and reports `partial: true`.
/// Test: this test.
#[tokio::test]
async fn an_index_whose_search_errors_marks_the_fan_out_partial() {
    let registry = IndexRegistry::new();
    let _ok = add_index(&registry, "ok", None).await;
    let _broken = add_index(&registry, "broken", None).await;
    let state = Arc::new(SearchAppState::new(registry));
    let broken = state
        .registry
        .get(&IndexId::new("broken".to_string()))
        .expect("broken");
    let _detached = broken.indexer.write().await.detach_for_delete();

    let body = super::search_global::global_search_report(&state, request("shared_symbol"))
        .await
        .expect("200");

    assert_eq!(
        body["indexes_searched"],
        serde_json::json!(["ok"]),
        "{body}"
    );
    assert_eq!(body["deadline_indexes_skipped"], serde_json::json!(0));
    assert_eq!(body["partial"], serde_json::json!(true), "{body}");
    assert_eq!(body["errored_indexes_skipped"], serde_json::json!(1));
}

/// Why: the embed timeout is operator-tunable (`TRUSTY_SEARCH_FANOUT_EMBED_TIMEOUT_MS`);
/// the configured value, not the 1 s default, must bound the embed (critic r2).
/// What: sets the env var to 2000 ms — above the default, so no other test in
/// this module changes outcome while it is set. An embed of 1.5 s (past the
/// default) answers `Embedded`; one of 2.5 s answers `TimedOut` with a message
/// naming the env var. Paused clock: no real time is spent.
/// Test: this test.
#[tokio::test(start_paused = true)]
#[serial_test::serial]
async fn the_embed_timeout_env_is_honoured_and_a_slower_embed_times_out() {
    use super::fanout_deadline::{
        embed_query_once, resolve_embed_timeout, EmbedStatus, FANOUT_EMBED_TIMEOUT_ENV,
    };
    // SAFETY: test-only, in the unnamed serial group (#5937); every reader
    // in this binary tolerates 2000 ms.
    unsafe { std::env::set_var(FANOUT_EMBED_TIMEOUT_ENV, "2000") };
    let timeout = resolve_embed_timeout();
    unsafe { std::env::remove_var(FANOUT_EMBED_TIMEOUT_ENV) };
    assert_eq!(timeout, Duration::from_millis(2_000));

    let within = IndexRegistry::new();
    let _w = add_corpus_index(
        &within,
        "within",
        Arc::new(SlowEmbedder(Duration::from_millis(1_500))),
    )
    .await;
    let ids = [IndexId::new("within".to_string())];
    let embed = embed_query_once(&within, &ids, "shared_symbol", timeout).await;
    assert_eq!(
        embed.status,
        EmbedStatus::Embedded,
        "the configured 2 s holds"
    );

    let past = IndexRegistry::new();
    let _p = add_corpus_index(
        &past,
        "past",
        Arc::new(SlowEmbedder(Duration::from_millis(2_500))),
    )
    .await;
    let ids = [IndexId::new("past".to_string())];
    let embed = embed_query_once(&past, &ids, "shared_symbol", timeout).await;
    assert_eq!(embed.status, EmbedStatus::TimedOut);
    let err = embed
        .vector
        .as_deref()
        .and_then(|r| r.as_ref().err().cloned())
        .expect("a timed-out embed carries its failure text");
    assert!(err.contains(FANOUT_EMBED_TIMEOUT_ENV), "{err}");
}
