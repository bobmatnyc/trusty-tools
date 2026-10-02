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
