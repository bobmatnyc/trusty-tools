use super::*;

#[tokio::test]
async fn test_save_chunks_roundtrip() {
    // Issue #85: a freshly-loaded indexer must have its chunks restored
    // and its BM25 posting list rebuilt from disk — no re-parsing of
    // source files allowed.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("chunks.json");

    // Phase 1: populate an indexer and snapshot it.
    let idx = make_indexer();
    idx.add_chunk(raw("a", "src/a.rs", "fn authenticate() {}"))
        .await
        .unwrap();
    idx.add_chunk(raw("b", "src/b.rs", "fn verify_token() {}"))
        .await
        .unwrap();
    idx.save_chunks_to_disk(&path).await.expect("save chunks");
    assert!(path.exists());

    // Phase 2: load into a fresh indexer and confirm both corpus and
    // BM25 see the restored chunks.
    let restored = make_indexer();
    let n = restored
        .load_chunks_from_disk(&path)
        .await
        .expect("load chunks");
    assert_eq!(n, 2);
    assert_eq!(restored.chunk_count(), 2);
    // BM25 must be rebuilt — a "authenticate" lexical query should hit
    // chunk "a".
    let bm25 = restored.bm25.read().await;
    let hits = bm25.score_query_all("authenticate", 5);
    drop(bm25);
    assert!(
        hits.iter().any(|(id, _)| id == "a"),
        "BM25 not rebuilt from restored chunks: {:?}",
        hits
    );
}

#[tokio::test]
async fn test_load_chunks_missing_file_returns_zero() {
    let idx = make_indexer();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nope.json");
    let n = idx.load_chunks_from_disk(&path).await.unwrap();
    assert_eq!(n, 0);
}

/// Phase 2 + 3: a committed batch must persist to redb, and a fresh indexer
/// pointed at the same redb file must rehydrate the corpus on warm-boot.
#[tokio::test]
async fn test_corpus_store_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let redb_path = dir.path().join("index.redb");

    // Phase 1: index two files into an indexer with a durable corpus.
    {
        let idx = make_indexer_with_corpus(&redb_path);
        idx.index_files_batch(&[
            ("src/auth.rs".into(), "fn authenticate() {}".into()),
            ("src/token.rs".into(), "fn verify_token() {}".into()),
        ])
        .await
        .expect("index batch");
        assert!(idx.chunk_count() >= 2);
    } // indexer (and its CorpusStore Arc) dropped here — simulates shutdown.

    // The redb file must hold the committed chunks.
    {
        let store = crate::core::corpus::CorpusStore::open(&redb_path).unwrap();
        assert!(
            store.chunk_count().unwrap() >= 2,
            "committed batch was not persisted to redb"
        );
    }

    // Phase 2: a fresh indexer warm-boots from the redb corpus — no reindex.
    let restored = make_indexer_with_corpus(&redb_path);
    let n = restored
        .load_chunks_from_redb()
        .await
        .expect("warm-boot from redb");
    assert!(n >= 2, "warm-boot rehydrated {n} chunks, expected >= 2");
    assert_eq!(restored.chunk_count(), n);

    // BM25 must be rebuilt from the rehydrated corpus.
    let bm25 = restored.bm25.read().await;
    let hits = bm25.score_query_all("authenticate", 5);
    drop(bm25);
    assert!(
        !hits.is_empty(),
        "BM25 not rebuilt from redb-restored chunks"
    );
}

/// Phase 3: warm-boot from an empty / missing redb corpus must yield zero
/// chunks (the first-run / post-upgrade fallback that triggers a reindex).
#[tokio::test]
async fn test_corpus_store_warm_boot_empty_is_zero() {
    let dir = tempfile::tempdir().unwrap();
    let idx = make_indexer_with_corpus(&dir.path().join("fresh.redb"));
    let n = idx.load_chunks_from_redb().await.unwrap();
    assert_eq!(n, 0, "empty redb corpus must rehydrate zero chunks");

    // An indexer with no corpus store at all also yields zero (BM25-only).
    let bare = CodeIndexer::new("bare", "/tmp/bare");
    assert_eq!(bare.load_chunks_from_redb().await.unwrap(), 0);
}

/// Phase 2: `remove_file` / `remove_chunk` must evict from the durable redb
/// corpus too — otherwise a warm-boot resurrects the deleted chunks.
#[tokio::test]
async fn test_corpus_store_deletes_on_remove() {
    let dir = tempfile::tempdir().unwrap();
    let redb_path = dir.path().join("index.redb");

    let idx = make_indexer_with_corpus(&redb_path);
    idx.index_files_batch(&[
        ("src/keep.rs".into(), "fn keep_me() {}".into()),
        ("src/drop.rs".into(), "fn drop_me() {}".into()),
    ])
    .await
    .unwrap();
    let before = idx.chunk_count();
    assert!(before >= 2);

    // Remove one file — this must delete its chunks from redb as well.
    idx.remove_file("src/drop.rs").await.unwrap();
    drop(idx);

    // Re-open the redb corpus directly: the dropped file's chunks must be gone.
    // redb is single-process-exclusive, so this store MUST be dropped before
    // the warm-boot indexer below re-opens the same file.
    let chunks = {
        let store = crate::core::corpus::CorpusStore::open(&redb_path).unwrap();
        store.load_all_chunks().unwrap()
    };
    assert!(
        chunks.iter().all(|c| c.file != "src/drop.rs"),
        "removed file's chunks still present in redb after remove_file"
    );
    assert!(
        chunks.iter().any(|c| c.file == "src/keep.rs"),
        "remove_file evicted the wrong file's chunks from redb"
    );

    // Warm-boot a fresh indexer: the removal must survive the restart.
    let restored = make_indexer_with_corpus(&redb_path);
    restored.load_chunks_from_redb().await.unwrap();
    let ids = restored.find_chunk_id("drop.rs", None).await;
    assert!(ids.is_none(), "deleted chunk resurrected on warm-boot");
}

/// Phase 3 migration: a daemon upgraded from a JSON-snapshot build has a
/// populated `chunks.json` and an empty `index.redb`. `migrate_corpus_to_redb`
/// must seed redb so the next restart uses the fast path.
#[tokio::test]
async fn test_corpus_store_migrates_from_json() {
    let dir = tempfile::tempdir().unwrap();
    let json_path = dir.path().join("chunks.json");
    let redb_path = dir.path().join("index.redb");

    // Stage a legacy JSON snapshot via an indexer with no corpus store.
    {
        let legacy = make_indexer();
        legacy
            .add_chunk(raw("a", "src/a.rs", "fn legacy_a() {}"))
            .await
            .unwrap();
        legacy
            .add_chunk(raw("b", "src/b.rs", "fn legacy_b() {}"))
            .await
            .unwrap();
        legacy.save_chunks_to_disk(&json_path).await.unwrap();
    }
    assert!(json_path.exists());

    // Warm-boot path: load the JSON snapshot, then migrate it into redb.
    let idx = make_indexer_with_corpus(&redb_path);
    let n = idx.load_chunks_from_disk(&json_path).await.unwrap();
    assert_eq!(n, 2);
    idx.migrate_corpus_to_redb().await;
    drop(idx);

    // The redb corpus must now hold the migrated chunks, so a subsequent
    // restart can skip the JSON file entirely.
    let restored = make_indexer_with_corpus(&redb_path);
    let m = restored.load_chunks_from_redb().await.unwrap();
    assert_eq!(m, 2, "redb corpus was not seeded by the JSON migration");
}

/// Phase 4: `swap_corpus_store` / `take_corpus_store` give the reindex
/// orchestrator the ability to stage a rebuilt corpus in a temp file and then
/// restore the indexer's durable store — without losing the original.
#[tokio::test]
async fn test_corpus_store_swap_and_take() {
    use crate::core::corpus::CorpusStore;
    let dir = tempfile::tempdir().unwrap();
    let live_path = dir.path().join("index.redb");
    let tmp_path = dir.path().join("index.redb.tmp");

    let mut idx = make_indexer_with_corpus(&live_path);
    assert!(idx.has_corpus_store());

    // Stage a fresh tmp corpus, capturing the live one it replaced. The prior
    // store's `Arc` is dropped immediately: redb is single-process-exclusive,
    // and `commit_force_corpus_swap` likewise drops the prior handle before
    // the rename. We only assert its path first.
    let staged = Arc::new(CorpusStore::open_fresh(&tmp_path).unwrap());
    let prev = idx.swap_corpus_store(staged).expect("prior store returned");
    assert_eq!(prev.path(), live_path.as_path());
    drop(prev);

    // Commit a batch — it must land in the *staging* file, not the live one.
    idx.index_files_batch(&[("src/new.rs".into(), "fn brand_new() {}".into())])
        .await
        .unwrap();

    // Take the staging store back out so its Arc can be dropped before a
    // rename (mirrors `commit_force_corpus_swap`).
    let staged_back = idx.take_corpus_store().expect("staging store taken");
    assert_eq!(staged_back.path(), tmp_path.as_path());
    assert!(!idx.has_corpus_store());
    assert!(
        staged_back.chunk_count().unwrap() >= 1,
        "batch did not commit to the staged corpus"
    );
    // Drop the staging handle so the live file can be re-opened below.
    drop(staged_back);

    // The original live file must be untouched — it never saw the new batch.
    let live = CorpusStore::open(&live_path).unwrap();
    assert_eq!(
        live.chunk_count().unwrap(),
        0,
        "live corpus was mutated while a staging corpus was swapped in"
    );
}

/// Idle-eviction (issue #83 follow-up): `idle_evict_secs` honours the default
/// and the `TRUSTY_CHUNKS_IDLE_EVICT_SECS` override, including `0` (disabled)
/// and an unparseable value (falls back to default).
#[test]
fn idle_evict_secs_default_and_env_override() {
    let prior = std::env::var("TRUSTY_CHUNKS_IDLE_EVICT_SECS").ok();

    // Unset → default.
    // SAFETY: this test is the only reader/writer of this env var.
    unsafe { std::env::remove_var("TRUSTY_CHUNKS_IDLE_EVICT_SECS") };
    assert_eq!(idle_evict_secs(), DEFAULT_CHUNKS_IDLE_EVICT_SECS);

    // Valid override wins.
    // SAFETY: see above.
    unsafe { std::env::set_var("TRUSTY_CHUNKS_IDLE_EVICT_SECS", "30") };
    assert_eq!(idle_evict_secs(), 30);

    // Zero disables (returned verbatim; the caller treats 0 as "off").
    // SAFETY: see above.
    unsafe { std::env::set_var("TRUSTY_CHUNKS_IDLE_EVICT_SECS", "0") };
    assert_eq!(idle_evict_secs(), 0);

    // Garbage falls back to default (with a warn).
    // SAFETY: see above.
    unsafe { std::env::set_var("TRUSTY_CHUNKS_IDLE_EVICT_SECS", "nope") };
    assert_eq!(idle_evict_secs(), DEFAULT_CHUNKS_IDLE_EVICT_SECS);

    // Restore.
    // SAFETY: see above.
    unsafe {
        match prior {
            Some(v) => std::env::set_var("TRUSTY_CHUNKS_IDLE_EVICT_SECS", v),
            None => std::env::remove_var("TRUSTY_CHUNKS_IDLE_EVICT_SECS"),
        }
    }
}

/// Idle-eviction core behaviour: a durably-backed indexer drops its in-memory
/// `chunks` map once idle past the threshold, and the next in-memory read
/// transparently rehydrates it from redb.
#[tokio::test]
async fn idle_eviction_drops_and_lazily_rehydrates_chunks() {
    let dir = tempfile::tempdir().unwrap();
    let redb_path = dir.path().join("index.redb");
    let idx = make_indexer_with_corpus(&redb_path);

    // Populate two chunks; they land in both the in-memory map and redb.
    idx.index_files_batch(&[
        ("src/auth.rs".into(), "fn authenticate() {}".into()),
        ("src/token.rs".into(), "fn verify_token() {}".into()),
    ])
    .await
    .expect("index batch");
    let resident_before = idx.in_memory_chunk_count().await;
    assert!(resident_before >= 2, "expected >= 2 resident chunks");

    // A zero threshold disables eviction — nothing is dropped.
    assert_eq!(idx.evict_chunks_if_idle(std::time::Duration::ZERO).await, 0);
    assert_eq!(idx.in_memory_chunk_count().await, resident_before);

    // A long threshold means the index isn't idle yet (it was just ingested,
    // which calls touch_activity) — nothing is dropped.
    assert_eq!(
        idx.evict_chunks_if_idle(std::time::Duration::from_secs(3600))
            .await,
        0
    );
    assert_eq!(idx.in_memory_chunk_count().await, resident_before);

    // A zero-length idle window (every elapsed duration exceeds it) forces
    // eviction now. The durable corpus is wired, so this is safe.
    let evicted = idx
        .evict_chunks_if_idle(std::time::Duration::from_nanos(1))
        .await;
    assert_eq!(evicted, resident_before, "eviction should drop every chunk");
    assert_eq!(
        idx.in_memory_chunk_count().await,
        0,
        "map must be empty after eviction"
    );
    assert!(
        idx.chunks_evicted.load(Ordering::Relaxed),
        "chunks_evicted flag must be set after eviction"
    );

    // The durable corpus is untouched — redb still has every chunk.
    assert!(idx.corpus_store().unwrap().chunk_count().unwrap() >= 2);

    // An in-memory read (raw_chunks_snapshot) lazily rehydrates from redb.
    let snapshot = idx.raw_chunks_snapshot().await;
    assert_eq!(
        snapshot.len(),
        resident_before,
        "raw_chunks_snapshot must rehydrate the evicted map"
    );
    assert_eq!(
        idx.in_memory_chunk_count().await,
        resident_before,
        "map must be repopulated after a read"
    );
    assert!(
        !idx.chunks_evicted.load(Ordering::Relaxed),
        "chunks_evicted flag must clear after rehydration"
    );
}

/// Idle-eviction safety: a BM25-only indexer (no durable corpus) is NEVER
/// evicted — its in-memory map is the only copy of the data.
#[tokio::test]
async fn idle_eviction_skips_indexers_without_corpus() {
    let idx = make_indexer(); // embedder + store, but corpus: None
    idx.add_chunk(raw("a", "src/a.rs", "fn a() {}"))
        .await
        .unwrap();
    let before = idx.in_memory_chunk_count().await;
    assert_eq!(before, 1);

    // Even with an always-idle window, eviction is a no-op without a corpus.
    let evicted = idx
        .evict_chunks_if_idle(std::time::Duration::from_nanos(1))
        .await;
    assert_eq!(evicted, 0, "must not evict without a durable corpus");
    assert_eq!(idx.in_memory_chunk_count().await, before);
    assert!(!idx.chunks_evicted.load(Ordering::Relaxed));
}
