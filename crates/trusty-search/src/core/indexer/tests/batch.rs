/// Batch indexing, persist coalescing, and domain-terms tests for the indexer.
use super::*;

#[tokio::test]
async fn test_persist_coalesces_concurrent_calls() {
    let idx = make_indexer();
    idx.add_chunk(raw("a", "a.rs", "fn a() {}")).await.unwrap();

    for _ in 0..64 {
        idx.spawn_incremental_persist(true);
    }

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    loop {
        let in_flight = idx.persist_state.in_flight.load(Ordering::Acquire);
        let dirty = idx.persist_state.dirty.load(Ordering::Acquire);
        if !in_flight && !dirty {
            break;
        }
        if std::time::Instant::now() >= deadline {
            panic!(
                "persist coalescing loop did not drain within 15s: \
                 in_flight={in_flight}, dirty={dirty}"
            );
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }

    idx.persist_state.dirty.store(false, Ordering::Release);
    idx.spawn_incremental_persist(true);
    let _ = idx.persist_state.in_flight.load(Ordering::Acquire);
}

/// Issue #29: non-forced `spawn_incremental_persist` increments the per-index
/// batch counter; only calls whose post-increment count is a multiple of
/// `HNSW_SNAPSHOT_BATCH_INTERVAL` proceed past the throttle.
#[tokio::test]
async fn test_incremental_persist_throttles_to_interval() {
    let idx = make_indexer();

    assert_eq!(idx.persist_state.batch_counter.load(Ordering::Acquire), 0);

    for _ in 0..HNSW_SNAPSHOT_BATCH_INTERVAL {
        idx.spawn_incremental_persist(false);
    }
    assert_eq!(
        idx.persist_state.batch_counter.load(Ordering::Acquire),
        HNSW_SNAPSHOT_BATCH_INTERVAL,
        "every non-forced call must increment the batch counter"
    );

    idx.spawn_incremental_persist(false);
    assert_eq!(
        idx.persist_state.batch_counter.load(Ordering::Acquire),
        HNSW_SNAPSHOT_BATCH_INTERVAL + 1
    );

    let before = idx.persist_state.batch_counter.load(Ordering::Acquire);
    idx.force_incremental_persist();
    assert_eq!(
        idx.persist_state.batch_counter.load(Ordering::Acquire),
        before,
        "force_incremental_persist must not increment the batch counter"
    );

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    while idx.persist_state.in_flight.load(Ordering::Acquire)
        || idx.persist_state.dirty.load(Ordering::Acquire)
    {
        if std::time::Instant::now() >= deadline {
            panic!("persist tasks did not drain within 15s");
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
}

#[tokio::test]
async fn test_index_files_batch_indexes_all_chunks_once() {
    let idx = make_indexer();
    let files = vec![
        (
            "src/a.rs".to_string(),
            "fn alpha() { beta(); }\nfn beta() {}\n".to_string(),
        ),
        (
            "src/b.rs".to_string(),
            "fn gamma() {}\nfn delta() { gamma(); }\n".to_string(),
        ),
    ];
    let added = idx.index_files_batch(&files).await.unwrap();
    assert!(added >= 4, "expected at least 4 chunks, got {added}");
    let g = idx.symbol_graph().await;
    assert!(g.node_count() >= 4);
    let q = SearchQuery {
        text: "fn alpha".to_string(),
        top_k: 5,
        expand_graph: false,
        compact: false,
        ..Default::default()
    };
    let r = idx.search(&q).await.unwrap();
    assert!(r.iter().any(|c| c.file == abs("src/a.rs")));
}

#[tokio::test]
async fn test_index_files_batch_empty_input_is_noop() {
    let idx = make_indexer();
    let added = idx.index_files_batch(&[]).await.unwrap();
    assert_eq!(added, 0);
    assert_eq!(idx.chunk_count(), 0);
}

#[tokio::test]
async fn test_index_files_batch_bm25_only_mode() {
    let idx = CodeIndexer::new("bm25-batch", "/tmp/test");
    let files = vec![(
        "x.rs".to_string(),
        "fn authenticate() {}\nfn other() {}\n".to_string(),
    )];
    let added = idx.index_files_batch(&files).await.unwrap();
    assert!(added >= 2);
    let r = idx
        .search(&SearchQuery {
            text: "authenticate".to_string(),
            top_k: 5,
            expand_graph: false,
            compact: false,
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(r.iter().any(|c| c.content.contains("authenticate")));
}

#[tokio::test]
async fn search_uses_domain_terms_when_provided() {
    use crate::core::classifier::{QueryClassifier, QueryIntent};

    let plain = QueryClassifier::classify("rezo integration query");
    assert_eq!(
        plain,
        QueryIntent::Unknown,
        "baseline: plain classifier must treat the rezo phrase as Unknown"
    );

    let idx =
        CodeIndexer::new("domain-test", "/tmp/domain").with_domain_terms(vec!["rezo".to_string()]);
    idx.index_file("api.rs", "fn rezo_handler() {}\nfn other() {}\n")
        .await
        .expect("index_file ok");
    let r = idx
        .search(&SearchQuery {
            text: "rezo integration query".into(),
            top_k: 5,
            expand_graph: false,
            compact: false,
            ..Default::default()
        })
        .await
        .expect("search ok");
    assert!(
        r.iter().any(|c| c.content.contains("rezo_handler")),
        "expected rezo_handler chunk to appear in results: {:?}",
        r.iter().map(|c| &c.content).collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn test_symbol_graph_rebuilds_after_indexing() {
    let idx = make_indexer();
    assert_eq!(idx.symbol_graph().await.node_count(), 0);
    idx.index_file("a.rs", "fn alpha() { beta(); }\nfn beta() {}\n")
        .await
        .unwrap();
    let g = idx.symbol_graph().await;
    assert!(g.node_count() >= 2, "graph should hold alpha + beta");
    assert!(
        !g.callees_of("alpha", 1).is_empty(),
        "alpha should have a callee edge to beta"
    );
}
