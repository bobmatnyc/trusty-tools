//! #12064 regression tests: a deferred-embed pass owes what the corpus holds.
//!
//! Why: the pass planned from the chunk map while the settle measured the
//! redb corpus, so a corpus row the map lacked was never embedded and the
//! semantic stage stayed `failed` until a restart.
//! What: builds that state by dropping a committed chunk from the map, then
//! drives the pass, a removal racing it, and a failed row load.
//! Test: the functions below.

use super::corpus_fault::break_corpus_reads;
use super::*;
use crate::core::indexer::VectorCoverage;

const DIM: usize = 32;

/// An indexer over a durable corpus holding `a` and `c` with no vectors, and
/// a chunk map that has lost `a`: the state a carryover-window write leaves.
async fn unmapped_row_indexer(dir: &std::path::Path) -> (CodeIndexer, Arc<CorpusStore>) {
    let corpus = Arc::new(CorpusStore::open(&dir.join("index.redb")).expect("open corpus"));
    let store: Arc<dyn VectorStore> = Arc::new(UsearchStore::new(DIM).expect("usearch new"));
    let mut idx = CodeIndexer::new("test", TEST_ROOT)
        .with_components(Arc::new(MockEmbedder::new(DIM)), store);
    idx.set_corpus_store(Arc::clone(&corpus));
    let parsed = ParsedBatch {
        chunks: vec![
            raw("a", "src/a.rs", "fn unmapped() {}"),
            raw("c", "src/c.rs", "fn mapped() {}"),
        ],
        embeddings: vec![None, None],
        entities_by_file: vec![],
        parse_ms: 0,
        embed_ms: 0,
        vector_count: 0,
    };
    idx.commit_parsed_batch(parsed, true)
        .await
        .expect("stage chunks");
    assert!(idx.chunks.write().await.remove("a").is_some(), "fixture");
    (idx, corpus)
}

async fn has_vector(idx: &CodeIndexer, id: &str) -> bool {
    idx.store.as_ref().expect("store").contains(id).await
}

/// The #12064 failure: a corpus row missing from the chunk map must be
/// planned, loaded from redb and embedded, so coverage passes.
#[tokio::test]
#[serial_test::parallel]
async fn a_corpus_row_missing_from_the_chunk_map_gets_a_vector_12064() {
    let dir = tempfile::tempdir().unwrap();
    let (idx, _corpus) = unmapped_row_indexer(dir.path()).await;

    let outcome = idx
        .embed_deferred_chunks_gated(None, None)
        .await
        .expect("pass");

    assert!(
        has_vector(&idx, "a").await,
        "the row the map lacks was not embedded"
    );
    assert!(has_vector(&idx, "c").await);
    assert_eq!(outcome.embedded, 2);
    let coverage = idx.vector_coverage().await;
    assert!(
        matches!(&coverage, VectorCoverage::Measured { missing, .. } if missing.is_empty()),
        "coverage must pass: {coverage:?}"
    );
}

/// #8761 for a chunk read from redb: removed while it embedded, it gets no
/// vector, since its row is what the commit confirms against.
#[tokio::test]
#[serial_test::parallel]
async fn a_corpus_row_removed_during_the_embed_phase_gets_no_vector_12064() {
    let dir = tempfile::tempdir().unwrap();
    let (idx, corpus) = unmapped_row_indexer(dir.path()).await;
    let mut pass = idx.plan_deferred_pass().await.expect("plan");
    let plan = idx
        .plan_deferred_group(&mut pass, usize::MAX)
        .await
        .expect("load the owed chunks")
        .expect("chunks are owed");
    let run = plan
        .embed(None, None, &mut plan.embed_state())
        .await
        .expect("embed");

    corpus.delete_chunks(&["a".to_owned()]).expect("delete row");
    let outcome = idx.commit_deferred_embed(plan, run).await.expect("commit");

    assert!(!has_vector(&idx, "a").await, "a removed row got a vector");
    assert!(has_vector(&idx, "c").await);
    assert_eq!(outcome.embedded, 1);
}

/// Fail-closed arm: when the owed rows cannot be read from redb, the wave is
/// an error, which fails the pass, so the stage is never settled `ready`.
#[tokio::test]
#[serial_test::serial]
async fn a_failed_row_load_fails_the_wave_12064() {
    let dir = tempfile::tempdir().unwrap();
    let (idx, _corpus) = unmapped_row_indexer(dir.path()).await;
    let mut pass = idx.plan_deferred_pass().await.expect("plan");

    break_corpus_reads(&idx);
    let err = match idx.plan_deferred_group(&mut pass, usize::MAX).await {
        Ok(_) => panic!("an unreadable owed row must fail the wave"),
        Err(e) => e,
    };

    assert!(
        format!("{err:#}").contains("load owed chunks the chunk map lacks from the corpus"),
        "the error must name the failed row load: {err:#}"
    );
    assert!(!has_vector(&idx, "a").await && !has_vector(&idx, "c").await);
}
