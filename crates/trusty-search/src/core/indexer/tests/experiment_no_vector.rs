use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

struct RejectingEmbedder(Arc<AtomicUsize>);

#[async_trait::async_trait]
impl Embedder for RejectingEmbedder {
    async fn embed(&self, _: &str) -> anyhow::Result<Vec<f32>> {
        self.0.fetch_add(1, Ordering::SeqCst);
        anyhow::bail!("embedding forbidden in lexical/KG experiment")
    }
    async fn embed_batch(&self, _: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        self.0.fetch_add(1, Ordering::SeqCst);
        anyhow::bail!("batch embedding forbidden in lexical/KG experiment")
    }
    fn dimension(&self) -> usize {
        32
    }
}

#[tokio::test]
async fn experiment_graph_bulk_incremental_and_refine_never_embed() {
    let calls = Arc::new(AtomicUsize::new(0));
    let embedder: Arc<dyn Embedder> = Arc::new(RejectingEmbedder(calls.clone()));
    let store: Arc<dyn VectorStore> = Arc::new(UsearchStore::new(32).unwrap());
    let mut idx =
        CodeIndexer::new("experiment", "/tmp/experiment").with_components(embedder, store);
    idx.skip_vector = true;
    let source = "pub fn alpha() { beta(); }\npub fn beta() {}\n";
    idx.index_files_batch(&[("src/gate.rs".into(), source.into())])
        .await
        .unwrap();
    idx.index_file("src/gate.rs", &source.replace("beta", "gamma"))
        .await
        .unwrap();
    assert!(idx.chunk_count() > 0);
    assert!(idx.snapshot_symbol_graph().await.node_count() > 0);
    for stage in [SearchStage::Lexical, SearchStage::Graph] {
        for refine_query in [None, Some("gamma".into())] {
            let hits = idx
                .search(&SearchQuery {
                    text: "alpha".into(),
                    stage: Some(stage),
                    refine_query,
                    top_k: 10,
                    ..Default::default()
                })
                .await
                .unwrap();
            assert!(!hits.is_empty());
        }
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(idx.store.as_ref().unwrap().len().await.unwrap(), 0);
}

#[tokio::test]
async fn experiment_vector_enabled_query_still_attempts_embedding() {
    let calls = Arc::new(AtomicUsize::new(0));
    let embedder: Arc<dyn Embedder> = Arc::new(RejectingEmbedder(calls.clone()));
    let store: Arc<dyn VectorStore> = Arc::new(UsearchStore::new(32).unwrap());
    let idx = CodeIndexer::new("control", "/tmp/control").with_components(embedder, store);
    let result = idx
        .search(&SearchQuery {
            text: "alpha".into(),
            stage: Some(SearchStage::Graph),
            ..Default::default()
        })
        .await;
    assert!(result.is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn experiment_enriched_terms_survive_corpus_reload() {
    use crate::core::experiment::{enrich_chunk_context, ExperimentConfig};
    let fixture = tempfile::tempdir().unwrap();
    let path = fixture.path().join("corpus.redb");
    let mut idx = make_indexer_with_corpus(&path);
    idx.skip_vector = true;
    let mut chunk = raw(
        "src/refresh.rs:1:1",
        "src/refresh.rs",
        "pub fn refresh() {}",
    );
    chunk.function_name = Some("refresh".into());
    // The doc word exists only in context: persisted source remains the function.
    chunk.start_line = 2;
    chunk.end_line = 2;
    let source = "/// renewalpolicy\npub fn refresh() {}";
    let mut chunks = vec![chunk];
    enrich_chunk_context(
        &mut chunks,
        source,
        &ExperimentConfig {
            context_words: 64,
            subchunk_window: 100,
        },
    );
    let terms = chunks[0].virtual_terms.clone();
    idx.commit_parsed_batch(
        ParsedBatch {
            chunks,
            embeddings: vec![None],
            ..Default::default()
        },
        false,
    )
    .await
    .unwrap();
    let query = SearchQuery {
        text: "renewalpolicy".into(),
        stage: Some(SearchStage::Lexical),
        ..Default::default()
    };
    let before = idx.search(&query).await.unwrap();
    assert_eq!(before.len(), 1);
    drop(idx);
    {
        let corpus = CorpusStore::open(&path).unwrap();
        let persisted = corpus.load_all_chunks().unwrap();
        assert_eq!(persisted[0].virtual_terms, terms);
    }
    let mut restored = make_indexer_with_corpus(&path);
    restored.skip_vector = true;
    restored.load_chunks_from_redb().await.unwrap();
    let after = restored.search(&query).await.unwrap();
    assert_eq!(
        before.iter().map(|c| (&c.id, c.score)).collect::<Vec<_>>(),
        after.iter().map(|c| (&c.id, c.score)).collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn experiment_existing_corpus_accepts_incremental_context_without_reindex() {
    const PHASE: &str = "TRUSTY_SEARCH_COMPATIBILITY_TEST_PHASE";
    const CORPUS: &str = "TRUSTY_SEARCH_COMPATIBILITY_TEST_CORPUS";
    let Ok(phase) = std::env::var(PHASE) else {
        let fixture = tempfile::tempdir().unwrap();
        // Separate processes exercise the real frozen config without mutating
        // the environment shared by concurrently running unit tests.
        for (phase, budget) in [("baseline", "0"), ("upgrade", "128"), ("rollback", "0")] {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "core::indexer::tests::experiment_no_vector::experiment_existing_corpus_accepts_incremental_context_without_reindex",
                    "--nocapture",
                ])
                .env(PHASE, phase)
                .env(CORPUS, fixture.path().join("corpus.redb"))
                .env("TRUSTY_SEARCH_EXPERIMENT_CONTEXT_WORDS", budget)
                .env("TRUSTY_SEARCH_EXPERIMENT_SUBCHUNK_WINDOW", "100")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{phase}: {}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
        }
        return;
    };

    let path = std::path::PathBuf::from(std::env::var(CORPUS).unwrap());
    let calls = Arc::new(AtomicUsize::new(0));
    let mut idx = make_indexer_with_corpus(&path).with_components(
        Arc::new(RejectingEmbedder(calls.clone())),
        Arc::new(UsearchStore::new(32).unwrap()),
    );
    idx.skip_vector = true;
    let old_source = "pub fn legacy_entry() { legacy_callee(); }\npub fn legacy_callee() {}\n";
    let snapshot_path = path.with_extension("baseline.json");
    let query = SearchQuery {
        text: "legacy_entry".into(),
        stage: Some(SearchStage::Lexical),
        ..Default::default()
    };
    if phase == "baseline" {
        idx.index_file("src/legacy.rs", old_source).await.unwrap();
        let chunks = idx.corpus.as_ref().unwrap().load_all_chunks().unwrap();
        std::fs::write(snapshot_path, serde_json::to_vec(&chunks).unwrap()).unwrap();
        std::fs::write(
            path.with_extension("response.json"),
            serde_json::to_vec(&idx.search(&query).await.unwrap()).unwrap(),
        )
        .unwrap();
    } else {
        // No source files exist on disk: warm restore and each incremental
        // update must work entirely from persisted rows plus supplied text.
        let baseline: Vec<RawChunk> =
            serde_json::from_slice(&std::fs::read(snapshot_path).unwrap()).unwrap();
        let count = idx.load_chunks_from_redb().await.unwrap();
        assert!(count >= baseline.len());
        if phase == "upgrade" {
            assert_eq!(count, baseline.len());
            assert_eq!(
                serde_json::to_vec(&idx.search(&query).await.unwrap()).unwrap(),
                std::fs::read(path.with_extension("response.json")).unwrap()
            );
            idx.index_file("src/renewalpolicy/new.rs", "pub fn fresh_entry() {}\n")
                .await
                .unwrap();
        }
        let current = idx.corpus.as_ref().unwrap().load_all_chunks().unwrap();
        for old in &baseline {
            let unchanged = current.iter().find(|chunk| chunk.id == old.id).unwrap();
            assert_eq!(
                serde_json::to_value(unchanged).unwrap(),
                serde_json::to_value(old).unwrap(),
                "enabling context must not rewrite an untouched row"
            );
        }
        for (term, file) in [
            ("legacy_entry", "src/legacy.rs"),
            ("renewalpolicy", "src/renewalpolicy/new.rs"),
        ] {
            for stage in [SearchStage::Lexical, SearchStage::Graph] {
                let hits = idx
                    .search(&SearchQuery {
                        text: term.into(),
                        stage: Some(stage),
                        ..Default::default()
                    })
                    .await
                    .unwrap();
                assert!(hits.iter().any(|hit| hit.path.as_deref() == Some(file)));
            }
        }
        let fresh = current
            .iter()
            .find(|chunk| chunk.file.ends_with("new.rs"))
            .unwrap();
        assert!(fresh
            .virtual_terms
            .iter()
            .any(|term| term == "renewalpolicy"));
        assert!(!fresh.content.contains("renewalpolicy"));
        if phase == "upgrade" {
            let ids: Vec<_> = current.iter().map(|chunk| chunk.id.clone()).collect();
            idx.index_file(
                "src/renewalpolicy/new.rs",
                "pub fn fresh_entry() { legacy_entry(); }\n",
            )
            .await
            .unwrap();
            let updated = idx.corpus.as_ref().unwrap().load_all_chunks().unwrap();
            assert_eq!(
                ids,
                updated
                    .iter()
                    .map(|chunk| chunk.id.clone())
                    .collect::<Vec<_>>()
            );
            assert!(updated
                .iter()
                .any(|chunk| chunk.calls.iter().any(|call| call == "legacy_entry")));
        }
        assert!(idx.snapshot_symbol_graph().await.node_count() >= 3);
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(idx.store.as_ref().unwrap().len().await.unwrap(), 0);
}
