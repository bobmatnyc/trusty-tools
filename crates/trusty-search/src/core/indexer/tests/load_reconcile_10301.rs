//! #10301: a kill between the two snapshot renames leaves the key sidecar
//! ahead of the HNSW graph.
//!
//! Why: `publish_snapshot` renames the sidecar first and the binary second.
//! Before #10301 a load accepted that pair, so `contains_many` reported the
//! ids the graph lacks as embedded and no catch-up pass ever embedded them.
//! What: builds the torn pair the way the publish order leaves it, loads it,
//! and checks that the ids the graph lacks are owed and re-embedded.
//! Test: this module.

use super::*;
use crate::core::embed::Embedder;
use crate::core::store::VectorStore;

const DIM: usize = 32;

/// Save `ids` as one published pair at `hnsw`, then stage a newer pair that
/// also holds `extra` and stop after the sidecar rename, as a kill would.
async fn sidecar_ahead_of_graph(hnsw: &std::path::Path, ids: &[String], extra: &[String]) {
    let mock = MockEmbedder::new(DIM);
    let store = UsearchStore::new(DIM).expect("usearch new");
    for id in ids {
        let vector = mock.embed(id).await.expect("mock embed");
        store.upsert(id, vector).await.expect("upsert");
    }
    store.save_to(hnsw).await.expect("save published pair");
    for id in extra {
        let vector = mock.embed(id).await.expect("mock embed");
        store.upsert(id, vector).await.expect("upsert");
    }
    let staged = hnsw.with_file_name("staged.usearch");
    store.save_to(&staged).await.expect("save staged pair");
    drop(store);
    // The publish order: the sidecar rename lands, the binary rename does not.
    std::fs::rename(
        staged.with_extension("keys.json"),
        hnsw.with_extension("keys.json"),
    )
    .expect("first rename");
}

/// A sidecar ahead of the graph loads without the ids the graph lacks, and
/// the catch-up pass owes and re-embeds exactly those ids.
/// Red before #10301: the load kept every sidecar entry, so the missing ids
/// read as embedded and the catch-up pass embedded nothing.
#[tokio::test]
async fn a_sidecar_ahead_of_the_graph_owes_and_reembeds_the_missing_ids() {
    let dir = tempfile::tempdir().expect("tempdir");
    let redb = dir.path().join("index.redb");
    let hnsw = dir.path().join("hnsw.usearch");

    // The corpus holds four chunks; no vector store is wired, so no commit
    // touches a vector.
    {
        let idx = make_indexer_with_corpus(&redb);
        let chunks = ["a", "b", "c", "d"].map(|name| {
            raw(
                &format!("src/{name}.rs:1:1"),
                &format!("src/{name}.rs"),
                name,
            )
        });
        let parsed = ParsedBatch {
            embeddings: vec![None; chunks.len()],
            chunks: chunks.to_vec(),
            entities_by_file: vec![],
            parse_ms: 0,
            embed_ms: 0,
            vector_count: 0,
        };
        idx.commit_parsed_batch(parsed, false)
            .await
            .expect("commit corpus");
    }
    let ids: Vec<String> = ["a", "b", "c", "d"]
        .iter()
        .map(|name| format!("src/{name}.rs:1:1"))
        .collect();
    sidecar_ahead_of_graph(&hnsw, &ids[..2], &ids[2..]).await;

    let store = UsearchStore::load_from(&hnsw)
        .await
        .expect("load")
        .expect("a sidecar-ahead pair still loads");
    assert_eq!(
        store.contains_many(&ids).await,
        vec![true, true, false, false],
        "#10301: the ids the graph lacks must not stay in id_to_key"
    );
    let mapped = store.ids_where(&|_| true).await.expect("ids_where");
    assert_eq!(mapped.len(), 2, "id_to_key keeps only the graph's ids");

    let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::new(DIM));
    let store: Arc<dyn VectorStore> = Arc::new(store);
    let mut idx = CodeIndexer::new("corpus-test", "/tmp/corpus-test")
        .with_components(embedder, Arc::clone(&store));
    idx.set_corpus_store(Arc::new(CorpusStore::open(&redb).expect("reopen corpus")));
    assert_eq!(idx.load_chunks_from_redb().await.expect("rehydrate"), 4);
    assert_eq!(
        idx.pending_embed_count().await,
        2,
        "the dropped ids are owed"
    );

    let outcome = idx
        .embed_deferred_chunks_gated(None, None)
        .await
        .expect("catch-up pass");
    assert_eq!(outcome.embedded, 2, "the catch-up pass re-embeds both");
    assert_eq!(store.contains_many(&ids).await, vec![true; 4]);
    assert_eq!(idx.pending_embed_count().await, 0);
}

/// A sidecar ahead of the graph that also dropped keys the graph still holds
/// cannot cover the graph after the reconcile, so the load discards the
/// snapshot rather than serve a graph with keys no id names (#3970 direction).
#[tokio::test]
async fn a_sidecar_that_also_dropped_graph_keys_discards_the_snapshot() {
    let dir = tempfile::tempdir().expect("tempdir");
    let hnsw = dir.path().join("hnsw.usearch");
    let mock = MockEmbedder::new(DIM);
    let store = UsearchStore::new(DIM).expect("usearch new");
    for id in ["a", "b"] {
        store
            .upsert(id, mock.embed(id).await.expect("embed"))
            .await
            .expect("upsert");
    }
    store.save_to(&hnsw).await.expect("save published pair");
    store.remove("a").await.expect("remove");
    store
        .upsert("c", mock.embed("c").await.expect("embed"))
        .await
        .expect("upsert");
    let staged = hnsw.with_file_name("staged.usearch");
    store.save_to(&staged).await.expect("save staged pair");
    drop(store);
    std::fs::rename(
        staged.with_extension("keys.json"),
        hnsw.with_extension("keys.json"),
    )
    .expect("first rename");

    let loaded = UsearchStore::load_from(&hnsw).await.expect("load");
    assert!(
        loaded.is_none(),
        "#10301: a reconcile that leaves graph key `a` unnamed must discard the snapshot"
    );
}
