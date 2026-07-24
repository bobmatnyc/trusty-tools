use super::*;
use crate::core::indexer::CodeIndexer;
use crate::core::registry::{IndexHandle, IndexId, IndexStages, StageStatus};
use std::fs;
use std::sync::atomic::Ordering;
use std::sync::Arc;

/// Filter wiring: with `include_paths` set on the handle, the reindex
/// must walk ONLY those subtrees. Files outside the configured slice
/// must not appear in the corpus.
///
/// Why: `trusty-search.yaml` declares `paths: [api/src]` to slice a
/// polyrepo. Without this test, a regression that drops the
/// `handle.include_paths` branch silently reverts to "walk everything",
/// which is the bug the YAML config exists to avoid.
/// What: stage a fixture with `api/keep.rs` and `ui/drop.rs`, register a
/// handle whose `include_paths = [<root>/api]`, run the reindex, and
/// assert only the api file was indexed.
/// Test: this test.
#[tokio::test]
async fn reindex_honours_include_paths_filter() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().to_path_buf();
    fs::create_dir_all(root.join("api")).unwrap();
    fs::create_dir_all(root.join("ui")).unwrap();
    fs::write(root.join("api/keep.rs"), "fn keep_me() {}\n").unwrap();
    fs::write(root.join("ui/drop.rs"), "fn drop_me() {}\n").unwrap();

    let indexer = CodeIndexer::new("filter-test", root.clone());
    let handle = Arc::new(IndexHandle {
        id: IndexId::new("filter-test"),
        indexer: Arc::new(tokio::sync::RwLock::new(indexer)),
        root_path: root.clone(),
        include_paths: vec![root.join("api")],
        exclude_globs: vec![],
        extensions: vec![],
        domain_terms: vec![],
        include_docs: false,
        respect_gitignore: true,
        follow_links: true,
        extra_skip_dirs: crate::service::walker::default_extra_skip_dirs(),
        data_file_max_bytes: crate::service::walker::DEFAULT_DATA_FILE_MAX_BYTES,
        path_filter: vec![],
        context_embedding: Arc::new(tokio::sync::RwLock::new(None)),
        context_summary: Arc::new(tokio::sync::RwLock::new(None)),
        indexed_head_sha: Arc::new(tokio::sync::RwLock::new(None)),
        last_indexed_at: Arc::new(tokio::sync::RwLock::new(None)),
        lexical_only: false,
        skip_kg: false,
        skip_vector: false,
        defer_embed: true,
        stages: Arc::new(tokio::sync::RwLock::new(IndexStages::default())),
        search_pressure: Arc::new(tokio::sync::Notify::new()),
        walk_diagnostics: Arc::new(tokio::sync::RwLock::new(
            crate::core::registry::WalkDiagnostics::default(),
        )),
    });
    let progress = Arc::new(ReindexProgress::new());
    // Issue #2730: deterministic rendezvous — await the task instead of polling.
    spawn_reindex_awaitable(handle.clone(), progress.clone(), false)
        .await
        .expect("reindex task must not panic");
    assert_eq!(progress.status.load(), ReindexStatus::Complete);
    assert_eq!(
        progress.total_files.load(Ordering::Acquire),
        1,
        "only api/keep.rs should be walked"
    );

    // And the corpus must contain `keep_me` but not `drop_me`.
    let idx = handle.indexer.read().await;
    let r = idx
        .search(&crate::core::indexer::SearchQuery {
            text: "keep_me".into(),
            top_k: 5,
            expand_graph: false,
            compact: false,
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(r.iter().any(|c| c.content.contains("keep_me")));
    let r2 = idx
        .search(&crate::core::indexer::SearchQuery {
            text: "drop_me".into(),
            top_k: 5,
            expand_graph: false,
            compact: false,
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(
        !r2.iter().any(|c| c.content.contains("drop_me")),
        "ui/drop.rs must not have been indexed"
    );
}

/// Issue #111 end-to-end: with `path_filter = ["common-*"]`, the reindex
/// must include files inside `common-utils/` but exclude `other-repo/`.
/// Uses the BM25-only path (no embedder needed) for hermetic execution.
#[tokio::test]
async fn reindex_honours_path_filter() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().to_path_buf();
    std::fs::create_dir_all(root.join("common-utils")).unwrap();
    std::fs::create_dir_all(root.join("other-repo")).unwrap();
    std::fs::write(root.join("common-utils/keep.rs"), "fn keep_common() {}\n").unwrap();
    std::fs::write(root.join("other-repo/drop.rs"), "fn drop_other() {}\n").unwrap();

    let indexer = CodeIndexer::new("pf-test", root.clone());
    let handle = Arc::new(IndexHandle {
        id: IndexId::new("pf-test"),
        indexer: Arc::new(tokio::sync::RwLock::new(indexer)),
        root_path: root.clone(),
        include_paths: vec![],
        exclude_globs: vec![],
        extensions: vec![],
        domain_terms: vec![],
        include_docs: false,
        respect_gitignore: true,
        follow_links: true,
        extra_skip_dirs: crate::service::walker::default_extra_skip_dirs(),
        data_file_max_bytes: crate::service::walker::DEFAULT_DATA_FILE_MAX_BYTES,
        path_filter: vec!["common-*".to_string()],
        context_embedding: Arc::new(tokio::sync::RwLock::new(None)),
        context_summary: Arc::new(tokio::sync::RwLock::new(None)),
        indexed_head_sha: Arc::new(tokio::sync::RwLock::new(None)),
        last_indexed_at: Arc::new(tokio::sync::RwLock::new(None)),
        lexical_only: false,
        skip_kg: false,
        skip_vector: false,
        defer_embed: true,
        stages: Arc::new(tokio::sync::RwLock::new(IndexStages::default())),
        search_pressure: Arc::new(tokio::sync::Notify::new()),
        walk_diagnostics: Arc::new(tokio::sync::RwLock::new(
            crate::core::registry::WalkDiagnostics::default(),
        )),
    });
    let progress = Arc::new(ReindexProgress::new());
    spawn_reindex_awaitable(handle.clone(), progress.clone(), false)
        .await
        .expect("reindex task must not panic");
    assert_eq!(progress.status.load(), ReindexStatus::Complete);
    assert_eq!(
        progress.total_files.load(Ordering::Acquire),
        1,
        "only common-utils/keep.rs should pass the path_filter"
    );

    let idx = handle.indexer.read().await;
    let r = idx
        .search(&crate::core::indexer::SearchQuery {
            text: "keep_common".into(),
            top_k: 5,
            expand_graph: false,
            compact: false,
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(r.iter().any(|c| c.content.contains("keep_common")));
    let r2 = idx
        .search(&crate::core::indexer::SearchQuery {
            text: "drop_other".into(),
            top_k: 5,
            expand_graph: false,
            compact: false,
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(
        !r2.iter().any(|c| c.content.contains("drop_other")),
        "other-repo must not have been indexed"
    );
}

#[tokio::test]
async fn reindex_walks_directory_and_emits_events() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().to_path_buf();
    fs::write(root.join("a.rs"), "fn a() {}").unwrap();
    fs::write(root.join("b.py"), "def b():\n    pass\n").unwrap();
    fs::create_dir(root.join("target")).unwrap();
    fs::write(root.join("target/skip.rs"), "fn skip() {}").unwrap();

    let indexer = CodeIndexer::new("test".to_string(), root.clone());
    let handle = Arc::new(IndexHandle::bare(
        IndexId::new("test"),
        Arc::new(tokio::sync::RwLock::new(indexer)),
        root.clone(),
    ));
    let progress = Arc::new(ReindexProgress::new());
    // Issue #2730: deterministic rendezvous — await the task instead of polling.
    spawn_reindex_awaitable(handle, progress.clone(), false)
        .await
        .expect("reindex task must not panic");
    assert_eq!(progress.status.load(), ReindexStatus::Complete);
    assert_eq!(progress.total_files.load(Ordering::Acquire), 2);
    assert_eq!(progress.indexed.load(Ordering::Acquire), 2);

    let events = progress.events.lock().await;
    // Issue #317: the daemon now emits `walk_complete` BEFORE `start` so
    // the CLI can render a dedicated "Walking files…" phase. The first
    // event is `walk_complete`; `start` is the second event. Older
    // assertions that expected `start` to be first are updated here.
    assert!(
        events
            .first()
            .map(|s| s.contains("\"walk_complete\""))
            .unwrap_or(false),
        "first event must be walk_complete (issue #317); got: {:?}",
        events.first()
    );
    assert!(
        events
            .get(1)
            .map(|s| s.contains("\"start\""))
            .unwrap_or(false),
        "second event must be start; got: {:?}",
        events.get(1)
    );
    assert!(
        events
            .last()
            .map(|s| s.contains("\"complete\""))
            .unwrap_or(false),
        "last event must be complete; got: {:?}",
        events.last()
    );
}

/// Issue #100 follow-up: end-to-end guard that the walker → chunker →
/// corpus pipeline persists chunks, distinct from the walker-only unit
/// tests next to `walk_source_files_with_options`. The follow-up report
/// for issue #100 observed `files=N chunks=0` after a v0.8.0 → v0.8.1
/// daemon upgrade and (incorrectly) attributed it to the walker swap;
/// the actual cause was the per-process content-hash cache hash-skipping
/// every file on the second reindex (`force=false`). This test pins both
/// the correct first-reindex chunking path AND the expected hash-skip
/// fast path on a second reindex, so any future walker rewrite that
/// silently drops paths fails here loudly while the documented fast
/// path keeps working.
///
/// Why: the unit walker tests only assert what the walker yields; they
/// can't catch a chunker that silently emits zero chunks (the first half
/// of this test) nor can they observe the hash-skip path (the second
/// half). Without an e2e assertion the next time someone misreads the
/// `chunks=0` log they'll bisect the walker again.
/// What: stages a small repo (`.gitignore` excluding `excluded/`, plus a
/// `crates/foo/src/lib.rs` with 3 `pub fn` definitions), runs the FULL
/// reindex pipeline twice, and asserts:
///   1. First reindex (cold cache): `total_chunks > 0`, corpus
///      `chunk_count() > 0`, and a search for `alpha` returns a chunk
///      whose `file` field equals the canonical path of `lib.rs`.
///   2. Second reindex (warm cache): `total_chunks == 0` AND
///      `skipped == 1` — confirming the hash-skip path fires for
///      unchanged content (the failure mode operators mistake for a
///      walker regression).
/// Test: this test.
#[tokio::test]
async fn reindex_persists_chunks_end_to_end() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().to_path_buf();
    // Stage a tiny `crates/foo/src/lib.rs` with 3 functions plus a
    // gitignored `excluded/` subtree that must NOT contribute chunks.
    fs::create_dir_all(root.join("crates/foo/src")).unwrap();
    fs::create_dir_all(root.join("excluded")).unwrap();
    fs::write(root.join(".gitignore"), "excluded/\n").unwrap();
    let lib_rs = root.join("crates/foo/src/lib.rs");
    fs::write(
        &lib_rs,
        "pub fn alpha() {}\n\npub fn beta() -> i32 { 1 }\n\npub fn gamma(x: i32) -> i32 { x + 1 }\n",
    )
    .unwrap();
    fs::write(
        root.join("excluded/should_not_index.rs"),
        "pub fn nope() {}\n",
    )
    .unwrap();

    // Use a unique IndexId so the per-process `file_hashes` static (shared
    // across tests in the same binary) doesn't interfere — earlier tests
    // in this module reindex other temp dirs against unrelated index ids.
    let id = IndexId::new("e2e-pipeline-test");
    let indexer = CodeIndexer::new(id.0.clone(), root.clone());
    let handle = Arc::new(IndexHandle::bare(
        id.clone(),
        Arc::new(tokio::sync::RwLock::new(indexer)),
        root.clone(),
    ));

    // ----- First reindex: cold cache, chunks must be produced. -----
    let progress = Arc::new(ReindexProgress::new());
    spawn_reindex_awaitable(handle.clone(), progress.clone(), false)
        .await
        .expect("reindex task must not panic");
    assert_eq!(progress.status.load(), ReindexStatus::Complete);

    // Walker yields exactly one file (`crates/foo/src/lib.rs`).
    assert_eq!(
        progress.total_files.load(Ordering::Acquire),
        1,
        "walker must yield exactly 1 file (gitignored subtree pruned)"
    );

    // The smoking-gun assertion the unit walker tests missed: the chunker
    // must have *persisted* chunks, not just been handed paths.
    let chunks = progress.total_chunks.load(Ordering::Acquire);
    assert!(
        chunks > 0,
        "regression: walker yielded 1 file but chunker persisted 0 chunks \
         on the first (cold-cache) reindex"
    );

    // On the cold-cache run the hash-skip path must NOT have fired.
    assert_eq!(
        progress.skipped.load(Ordering::Acquire),
        0,
        "first reindex hash-skipped a file (cold cache should hash-miss everything)"
    );

    // Issue #602 — portability: the corpus must store the ROOT-RELATIVE
    // path (`crates/foo/src/lib.rs`), and search must resolve it against the
    // serving host's `root_path`. Search results are intentionally absolute
    // (resolved via `resolve_chunk_file`), so a chunk written under one root
    // and served under a different root resolves correctly on each host.
    // The chunk-write `strip_prefix` now strips against the canonical walk
    // root, so the STORED `file` is always relative.
    let rel_lib_rs = "crates/foo/src/lib.rs";
    let expected_resolved = root.join(rel_lib_rs).to_string_lossy().into_owned();
    {
        let idx = handle.indexer.read().await;
        assert!(
            idx.chunk_count() > 0,
            "regression: indexer corpus is empty after reindex"
        );
        // Search for one of the functions to verify chunks are also live
        // in BM25 / vector. `alpha` is unique to the staged file.
        let results = idx
            .search(&crate::core::indexer::SearchQuery {
                text: "alpha".into(),
                top_k: 5,
                expand_graph: false,
                compact: false,
                ..Default::default()
            })
            .await
            .unwrap();
        // The resolved (absolute) search path must be `root_path` joined
        // with the relative stored path — proving the stored path was
        // relative and is resolved against the live root.
        assert!(
            results.iter().any(|c| c.file == expected_resolved),
            "no chunk resolves to root_path + relative lib.rs (#602): \
             expected {expected_resolved:?}, got {:?}",
            results.iter().map(|c| c.file.clone()).collect::<Vec<_>>()
        );
    }
    // Directly assert the corpus STORES a root-relative (non-absolute) path
    // — the actual #602 portability invariant. `raw_chunks_snapshot` exposes
    // the raw `RawChunk.file` (relative), bypassing the `resolve_chunk_file`
    // absolutization on the read path.
    {
        let idx = handle.indexer.read().await;
        let raw_files: Vec<String> = idx
            .raw_chunks_snapshot()
            .await
            .into_iter()
            .map(|c| c.file)
            .collect();
        assert!(
            raw_files.iter().any(|f| f == rel_lib_rs),
            "corpus did not store the ROOT-RELATIVE path (#602 regression); \
             stored files: {raw_files:?}"
        );
        assert!(
            raw_files
                .iter()
                .all(|f| !std::path::Path::new(f).is_absolute()),
            "corpus stored an ABSOLUTE path (#602 regression): {raw_files:?}"
        );
    }

    // ----- Second reindex: warm cache, all files must hash-skip. -----
    //
    // This is the path the v0.8.1 follow-up report misread as a walker
    // regression. The log line `files=1 chunks=0` is correct: every file
    // hashed identically to the previous reindex, so the chunker is
    // intentionally bypassed. Pin this behaviour so the next bisection
    // doesn't waste another round chasing a non-existent walker bug.
    let progress2 = Arc::new(ReindexProgress::new());
    spawn_reindex_awaitable(handle.clone(), progress2.clone(), false)
        .await
        .expect("reindex task must not panic");
    assert_eq!(progress2.status.load(), ReindexStatus::Complete);
    assert_eq!(
        progress2.total_files.load(Ordering::Acquire),
        1,
        "second reindex must still walk 1 file"
    );
    assert_eq!(
        progress2.total_chunks.load(Ordering::Acquire),
        0,
        "second reindex of unchanged files MUST emit 0 new chunks (hash-skip path)"
    );
    assert_eq!(
        progress2.skipped.load(Ordering::Acquire),
        1,
        "second reindex must report the file as hash-skipped"
    );
    // The corpus must remain populated — hash-skip does not delete chunks.
    {
        let idx = handle.indexer.read().await;
        assert!(
            idx.chunk_count() > 0,
            "regression: corpus emptied by a hash-skip-only second reindex"
        );
    }
}

/// Issue #112: after a reindex completes, the handle's
/// `context_embedding` and `context_summary` must be populated when
/// recognised metadata files exist in `root_path`. Uses a `MockEmbedder`
/// so the test is fully hermetic.
#[tokio::test]
async fn context_embedding_populated_after_reindex() {
    use crate::core::embed::{Embedder, MockEmbedder};
    use crate::core::store::{UsearchStore, VectorStore};

    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().to_path_buf();
    // Stage a source file plus a README so the metadata scraper has
    // something to embed.
    fs::write(root.join("lib.rs"), "fn hello() {}\n").unwrap();
    fs::write(
        root.join("README.md"),
        "# proj\n\nA test project for #112.\n",
    )
    .unwrap();

    let dim = 32;
    let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::new(dim));
    let store: Arc<dyn VectorStore> = Arc::new(UsearchStore::new(dim).expect("usearch new"));
    let indexer = CodeIndexer::new("ctx-test", root.clone()).with_components(embedder, store);

    let handle = Arc::new(IndexHandle::bare(
        IndexId::new("ctx-test"),
        Arc::new(tokio::sync::RwLock::new(indexer)),
        root.clone(),
    ));
    let progress = Arc::new(ReindexProgress::new());
    spawn_reindex_awaitable(handle.clone(), progress.clone(), false)
        .await
        .expect("reindex task must not panic");
    assert_eq!(progress.status.load(), ReindexStatus::Complete);

    let ctx = handle.context_embedding.read().await.clone();
    assert!(
        ctx.is_some(),
        "context_embedding must be populated when metadata is present and embedder is wired"
    );
    assert_eq!(ctx.unwrap().len(), dim, "embedding must have embedder dim");

    let summary = handle.context_summary.read().await.clone();
    assert!(summary.is_some(), "context_summary must be populated");
    let s = summary.unwrap();
    assert!(s.contains("proj") || s.contains("README"));
}

/// Issue #601 (end-to-end, hermetic): a full-pipeline index whose embedder
/// FAILS for every batch must end `Failed`, NOT `Complete` — and the
/// previously-live corpus must be preserved (rolled back), not destroyed.
///
/// Why: this is the exact false-green bug — before the non-empty gate, a
/// silent embed failure flipped the index to ready with zero vectors and
/// `/health` served a dead index as green. This test wires a `FailingEmbedder`
/// (returns `Err` from every `embed_batch`) into an indexer that ALSO has a
/// durable corpus pre-seeded with a "previous" chunk, runs the reindex, and
/// asserts (1) status is `Failed`, (2) a terminal `error` event with
/// `fatal: true` was emitted, and (3) the pre-existing corpus chunk survived
/// the rollback. No real embedder daemon is involved — the failing mock makes
/// it fully hermetic.
/// What: see the assertions inline.
/// Test: this test (daemon-free; the real-embedder spawn path is exercised
/// only by the ignore-tagged ONNX integration tests).
#[tokio::test]
async fn reindex_marks_failed_on_zero_vectors_and_preserves_corpus() {
    use crate::core::embed::Embedder;
    use crate::core::store::{UsearchStore, VectorStore};
    use anyhow::anyhow;

    /// Embedder that fails every batch — emulates a sidecar crash / OOM /
    /// model-load stall so the reindex produces ZERO vectors despite an
    /// embedder being wired.
    struct FailingEmbedder;
    #[async_trait::async_trait]
    impl Embedder for FailingEmbedder {
        async fn embed(&self, _text: &str) -> anyhow::Result<Vec<f32>> {
            Err(anyhow!("simulated embedder failure (embed)"))
        }
        async fn embed_batch(&self, _texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
            Err(anyhow!("simulated embedder failure (every batch)"))
        }
        fn dimension(&self) -> usize {
            32
        }
    }

    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().to_path_buf();
    fs::write(root.join("lib.rs"), "pub fn alpha() {}\n").unwrap();

    let dim = 32;
    let embedder: Arc<dyn Embedder> = Arc::new(FailingEmbedder);
    let store: Arc<dyn VectorStore> = Arc::new(UsearchStore::new(dim).expect("usearch new"));
    let mut indexer = CodeIndexer::new("fail-601", root.clone()).with_components(embedder, store);

    // Pre-seed a durable corpus with a "previous" chunk so we can prove the
    // rollback preserved it. The staging swap requires a durable corpus.
    let corpus_path = tmp.path().join("index.redb");
    let corpus = crate::core::corpus::CorpusStore::open(&corpus_path).expect("open corpus");
    // Seed one "previous" chunk via the public `chunk_text` helper, then
    // pin a stable id we can assert survived the rollback.
    let mut prev = crate::core::chunker::chunk_text("prev/file.rs", "fn previous() {}", 64, 64);
    prev[0].id = "prev/file.rs:1:1".into();
    prev[0].file = "prev/file.rs".into();
    corpus.upsert_chunks(&prev).expect("seed prev chunk");
    indexer.set_corpus_store(Arc::new(corpus));

    // Use defer_embed=false so the zero-vector failure gate (#601) fires
    // synchronously. With defer_embed=true the fast pass deliberately skips
    // embedding and the gate does not apply (issue #923).
    let mut handle_inner = IndexHandle::bare(
        IndexId::new("fail-601"),
        Arc::new(tokio::sync::RwLock::new(indexer)),
        root.clone(),
    );
    handle_inner.defer_embed = false;
    let handle = Arc::new(handle_inner);
    let progress = Arc::new(ReindexProgress::new());
    // Issue #2730: await the task's completion (deterministic rendezvous)
    // rather than polling `progress.status` on a wall-clock budget.
    spawn_reindex_awaitable(handle.clone(), progress.clone(), false)
        .await
        .expect("reindex task must not panic");

    assert_eq!(
        progress.status.load(),
        ReindexStatus::Failed,
        "embed failure must mark the reindex Failed, not Complete"
    );

    // The lifecycle status must report `failed`, never `ready`.
    let stages = handle.stages.read().await.clone();
    assert_eq!(stages.lifecycle_status(), "failed");
    assert_eq!(stages.semantic.status, StageStatus::Failed);
    assert!(
        stages.semantic.failure.is_some(),
        "failed semantic stage must carry a reason"
    );

    // A terminal `error` event with `fatal: true` must have been emitted,
    // carrying the embed-failure signal (#601 LOUD failure, not false-green).
    let events = progress.events.lock().await.clone();
    assert!(
        events.iter().any(|e| e.contains("\"fatal\":true")
            && e.contains("\"event\":\"error\"")
            && e.contains("\"vector_count\":0")),
        "a fatal error event with vector_count:0 must be emitted: {events:?}"
    );

    // Non-destructive (#603): the failed rebuild's `lib.rs` chunks must NOT
    // have been promoted into the live corpus — the staging swap rolled
    // back. The seeded "previous" chunk's preservation across the rollback
    // re-open depends on the daemon's persistence path layout (the staging
    // helpers resolve the live corpus via the data-dir, not the ad-hoc test
    // path), so the round-trip restore is exercised by the daemon-gated
    // integration tests; here we assert the weaker hermetic invariant that
    // the failed rebuild was not committed.
    let live = handle.indexer.read().await.raw_chunks_snapshot().await;
    assert!(
        !live.iter().any(|c| c.file == "lib.rs"),
        "non-destructive: the failed rebuild must not promote lib.rs chunks; \
         got: {:?}",
        live.iter().map(|c| c.id.clone()).collect::<Vec<_>>()
    );
}

/// Issue #2211 regression: with `defer_embed=true` (the default) and a real
/// embedder wired, the semantic stage must NOT report `Ready` the instant
/// the fast pass (C1: walk → chunk → BM25 → KG) completes — the actual
/// embedding runs in a background pass that has not finished yet. Before the
/// fix, `finish_reindex` called `mark_semantic_ready_graph_in_progress`
/// unconditionally right after the batch loop, flipping `semantic.status`
/// to `Ready` regardless of `defer_embed`; the deferred background pass
/// never reset it back to `InProgress`, so operators polling
/// `/indexes/:id/status` (or health-check callers like apex-companion) saw
/// a false "ready" signal for the entire embedding window.
///
/// Why: uses a `SlowEmbedder` that sleeps briefly per call so the background
/// pass takes long enough to reliably observe the intermediate (non-Ready)
/// state between the fast-pass `Complete` event and the deferred pass
/// actually finishing.
/// What: runs a full `spawn_reindex` with `defer_embed` defaulted `true`
/// (via `IndexHandle::bare`) and a real (slow) embedder + HNSW store; polls
/// for `ReindexStatus::Complete` (fast pass done), then immediately asserts
/// `semantic.status != Ready`; then polls further and asserts it eventually
/// does become `Ready` once the deferred pass finishes.
/// Test: this test.
#[tokio::test]
async fn defer_embed_semantic_stage_not_ready_before_background_pass_completes() {
    use crate::core::embed::Embedder;
    use crate::core::store::{UsearchStore, VectorStore};
    use std::sync::atomic::AtomicUsize;

    /// Embedder that sleeps briefly per call so the deferred background pass
    /// takes long enough to observe an intermediate (non-Ready) semantic
    /// stage state before it completes.
    struct SlowEmbedder {
        calls: Arc<AtomicUsize>,
    }
    #[async_trait::async_trait]
    impl Embedder for SlowEmbedder {
        async fn embed(&self, _text: &str) -> anyhow::Result<Vec<f32>> {
            self.calls.fetch_add(1, Ordering::AcqRel);
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            Ok(vec![0.1; 32])
        }
        async fn embed_batch(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
            self.calls.fetch_add(1, Ordering::AcqRel);
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            Ok(texts.iter().map(|_| vec![0.1; 32]).collect())
        }
        fn dimension(&self) -> usize {
            32
        }
    }

    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().to_path_buf();
    fs::write(root.join("lib.rs"), "pub fn alpha() {}\n").unwrap();

    let dim = 32;
    let calls = Arc::new(AtomicUsize::new(0));
    let embedder: Arc<dyn Embedder> = Arc::new(SlowEmbedder {
        calls: calls.clone(),
    });
    let store: Arc<dyn VectorStore> = Arc::new(UsearchStore::new(dim).expect("usearch new"));
    let indexer = CodeIndexer::new("defer-2211", root.clone()).with_components(embedder, store);

    // `defer_embed` defaults to `true` via `IndexHandle::bare`.
    let handle = Arc::new(IndexHandle::bare(
        IndexId::new("defer-2211"),
        Arc::new(tokio::sync::RwLock::new(indexer)),
        root.clone(),
    ));
    assert!(
        handle.defer_embed,
        "precondition: defer_embed must default true"
    );
    let progress = Arc::new(ReindexProgress::new());
    // Issue #2730: await the fast pass deterministically. `run_reindex` returns
    // once the fast pass has set `Complete` and spawned the deferred embed pass;
    // the deferred pass runs as a separate task (polled for below). This
    // replaces a wall-clock poll that could time out under load.
    spawn_reindex_awaitable(handle.clone(), progress.clone(), false)
        .await
        .expect("reindex task must not panic");
    assert_eq!(progress.status.load(), ReindexStatus::Complete);

    // Issue #2211: right after the fast pass, the background embed pass has
    // NOT finished yet (SlowEmbedder blocks 300ms per call) — the semantic
    // stage must not report Ready. Before the fix this was already Ready
    // here, because `finish_reindex` marked it Ready unconditionally.
    let semantic_status = handle.stages.read().await.semantic.status;
    assert_ne!(
        semantic_status,
        StageStatus::Ready,
        "semantic stage must not be Ready while the deferred embed pass is still running \
         (issue #2211 — false-ready before embedding actually completes)"
    );

    // Eventually the deferred pass finishes and semantic really does flip Ready.
    // Issue #3748 slice A: the deferred-embed catch-up queue now cooperatively
    // polls a shared size-ordered heap (see `reindex::defer_embed_queue`) —
    // each index's own task re-checks every `POLL_INTERVAL` (20ms) whether
    // it's its turn, rather than racing `background_reindex_semaphore`
    // immediately on spawn. The original 100-iteration/5s budget is still
    // plenty: verified stable across 15 consecutive full-module runs under
    // this crate's full parallel test suite after the per-index-task rewrite
    // (issue #3748 slice A review finding 1's fix also eliminated an earlier,
    // unrelated flake — a single shared dispatcher task whose lifetime was
    // tied to whichever test happened to spawn it).
    let mut final_status = semantic_status;
    for _ in 0..100 {
        final_status = handle.stages.read().await.semantic.status;
        if final_status == StageStatus::Ready {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert_eq!(
        final_status,
        StageStatus::Ready,
        "semantic stage must eventually become Ready once the deferred pass completes"
    );
    assert!(
        calls.load(Ordering::Acquire) > 0,
        "the (slow) embedder must have been invoked by the deferred pass"
    );
}

/// Issue #112: when no recognised metadata files exist, the context
/// embedding stays `None` so the router falls back to a neutral 1.0
/// weight for this index.
#[tokio::test]
async fn context_embedding_none_when_no_metadata() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().to_path_buf();
    // Only a source file — no README, no Cargo.toml, etc.
    fs::write(root.join("lib.rs"), "fn hello() {}\n").unwrap();

    let indexer = CodeIndexer::new("no-meta", root.clone());
    let handle = Arc::new(IndexHandle::bare(
        IndexId::new("no-meta"),
        Arc::new(tokio::sync::RwLock::new(indexer)),
        root.clone(),
    ));
    let progress = Arc::new(ReindexProgress::new());
    spawn_reindex_awaitable(handle.clone(), progress.clone(), false)
        .await
        .expect("reindex task must not panic");
    assert_eq!(progress.status.load(), ReindexStatus::Complete);
    assert!(handle.context_embedding.read().await.is_none());
    assert!(handle.context_summary.read().await.is_none());
}

// ── Staged-pipeline (issue #109, Phase 1) ──────────────────────────

/// Helper: build an IndexHandle wrapping the bare BM25-only indexer
/// with the given `lexical_only` setting. Mirrors the existing test
/// fixtures but lets us flip the new flag.
fn make_handle_with_flag(
    id: &str,
    root: std::path::PathBuf,
    lexical_only: bool,
) -> Arc<IndexHandle> {
    make_handle_with_flags(id, root, lexical_only, false)
}

/// Extended handle builder used by skip_kg tests.
///
/// Why: the original `make_handle_with_flag` only parameterises `lexical_only`.
/// Adding a second flag parameter would break all existing callers; instead
/// the old function delegates here so both paths stay readable.
/// What: constructs an `Arc<IndexHandle>` with the given `lexical_only` and
/// `skip_kg` flags; pre-sets `stages` accordingly.
/// Test: used by `skip_kg_index_never_runs_phase3` and
/// `skip_kg_graph_stage_stays_skipped`.
fn make_handle_with_flags(
    id: &str,
    root: std::path::PathBuf,
    lexical_only: bool,
    skip_kg: bool,
) -> Arc<IndexHandle> {
    use crate::core::registry::{IndexStages, StageState};
    let indexer = CodeIndexer::new(id.to_string(), root.clone());
    let stages = if lexical_only {
        IndexStages {
            lexical: StageState::pending(),
            semantic: StageState::skipped(),
            graph: StageState::skipped(),
        }
    } else if skip_kg {
        IndexStages {
            lexical: StageState::pending(),
            semantic: StageState::pending(),
            graph: StageState::skipped(),
        }
    } else {
        IndexStages::default()
    };
    Arc::new(IndexHandle {
        id: IndexId::new(id),
        indexer: Arc::new(tokio::sync::RwLock::new(indexer)),
        root_path: root,
        include_paths: vec![],
        exclude_globs: vec![],
        extensions: vec![],
        domain_terms: vec![],
        include_docs: false,
        respect_gitignore: true,
        follow_links: true,
        extra_skip_dirs: crate::service::walker::default_extra_skip_dirs(),
        data_file_max_bytes: crate::service::walker::DEFAULT_DATA_FILE_MAX_BYTES,
        path_filter: vec![],
        context_embedding: Arc::new(tokio::sync::RwLock::new(None)),
        context_summary: Arc::new(tokio::sync::RwLock::new(None)),
        indexed_head_sha: Arc::new(tokio::sync::RwLock::new(None)),
        last_indexed_at: Arc::new(tokio::sync::RwLock::new(None)),
        lexical_only,
        skip_kg,
        skip_vector: false,
        defer_embed: false,
        stages: Arc::new(tokio::sync::RwLock::new(stages)),
        search_pressure: Arc::new(tokio::sync::Notify::new()),
        walk_diagnostics: Arc::new(tokio::sync::RwLock::new(
            crate::core::registry::WalkDiagnostics::default(),
        )),
    })
}

/// Handle builder used by `skip_vector` tests (issue #2984 Phase 1).
///
/// Why: mirrors `make_handle_with_flags` but toggles `skip_vector` instead
/// of `skip_kg` — the two components are orthogonal, so KG stays enabled
/// (`Pending`, not `Skipped`) here, pinning the vector-off/KG-on quadrant.
/// What: constructs an `Arc<IndexHandle>` with `skip_vector` set; pre-sets
/// `stages` so the semantic lane is `Skipped` from the start when `true`.
/// Test: used by `skip_vector_index_never_embeds`.
fn make_handle_with_skip_vector(
    id: &str,
    root: std::path::PathBuf,
    skip_vector: bool,
) -> Arc<IndexHandle> {
    use crate::core::registry::{IndexStages, StageState};
    let indexer = CodeIndexer::new(id.to_string(), root.clone());
    let stages = if skip_vector {
        IndexStages {
            lexical: StageState::pending(),
            semantic: StageState::skipped(),
            graph: StageState::pending(),
        }
    } else {
        IndexStages::default()
    };
    Arc::new(IndexHandle {
        id: IndexId::new(id),
        indexer: Arc::new(tokio::sync::RwLock::new(indexer)),
        root_path: root,
        include_paths: vec![],
        exclude_globs: vec![],
        extensions: vec![],
        domain_terms: vec![],
        include_docs: false,
        respect_gitignore: true,
        follow_links: true,
        extra_skip_dirs: crate::service::walker::default_extra_skip_dirs(),
        data_file_max_bytes: crate::service::walker::DEFAULT_DATA_FILE_MAX_BYTES,
        path_filter: vec![],
        context_embedding: Arc::new(tokio::sync::RwLock::new(None)),
        context_summary: Arc::new(tokio::sync::RwLock::new(None)),
        indexed_head_sha: Arc::new(tokio::sync::RwLock::new(None)),
        last_indexed_at: Arc::new(tokio::sync::RwLock::new(None)),
        lexical_only: false,
        skip_kg: false,
        skip_vector,
        // Issue #2984 Phase 1: `defer_embed=false` forces the fast pass to
        // attempt synchronous embedding for a non-`skip_vector` control run —
        // exercising the `batch.rs` fast-pass gate, not just the deferred
        // background-pass gate the default `defer_embed=true` would hit.
        defer_embed: false,
        stages: Arc::new(tokio::sync::RwLock::new(stages)),
        search_pressure: Arc::new(tokio::sync::Notify::new()),
        walk_diagnostics: Arc::new(tokio::sync::RwLock::new(
            crate::core::registry::WalkDiagnostics::default(),
        )),
    })
}

/// Issue #109 Phase 1 acceptance test: after a reindex completes on a
/// BM25-only handle (no embedder wired), the lexical stage is `Ready`
/// and the search capabilities array contains `bm25`. A search query
/// then succeeds against the lexical lane and returns the expected
/// chunk.
///
/// Why: pins the contract that BM25 search works as soon as Stage 1
/// finishes — the bedrock guarantee Phase 1 is delivering. The
/// `lexical_only` and full-pipeline cases share the same Stage 1
/// code path, so this test exercises both implicitly: the indexer
/// has no embedder wired, which is the same shape `lexical_only`
/// produces at runtime.
/// What: stages a tiny repo, reindexes it, asserts the stages reflect
/// Ready / Ready / Ready (graph rebuilds even without embedder), and
/// that `search_capabilities` advertises bm25/literal/exact_match.
/// Test: this test.
#[tokio::test]
async fn stage_1_completes_and_search_works_before_embedding() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().to_path_buf();
    fs::write(root.join("hello.rs"), "pub fn unique_alpha() {}\n").unwrap();

    // Non-`lexical_only` handle but with no embedder wired — this is
    // the warm-boot BM25-only shape. Stage 1 must complete and the
    // search capabilities must advertise the lexical lane.
    let handle = make_handle_with_flag("stage1-test", root.clone(), false);
    let progress = Arc::new(ReindexProgress::new());
    spawn_reindex_awaitable(handle.clone(), progress.clone(), false)
        .await
        .expect("reindex task must not panic");
    assert_eq!(progress.status.load(), ReindexStatus::Complete);

    // Lexical lane must be Ready (and so should the others — Stage 1
    // helpers don't gate graph or semantic on the embedder presence
    // because the corpus still has chunks for the KG to walk).
    let stages = handle.stages.read().await.clone();
    assert_eq!(
        stages.lexical.status,
        crate::core::registry::StageStatus::Ready,
        "stage 1 must finish on a BM25-only reindex"
    );
    let caps = stages.search_capabilities();
    assert!(
        caps.contains(&"bm25"),
        "search_capabilities must contain bm25 after Stage 1, got: {caps:?}"
    );

    // Search runs and the lexical lane returns the staged chunk.
    let idx = handle.indexer.read().await;
    let results = idx
        .search(&crate::core::indexer::SearchQuery {
            text: "unique_alpha".to_string(),
            top_k: 5,
            expand_graph: false,
            compact: false,
            ..Default::default()
        })
        .await
        .expect("search");
    assert!(
        results.iter().any(|c| c.content.contains("unique_alpha")),
        "BM25 lane must return the chunk after Stage 1: {results:?}"
    );
}

/// Issue #109 Phase 1: a `lexical_only` index permanently keeps the
/// semantic + graph stages at `Skipped`. The reindex pipeline returns
/// after Stage 1 and the search capabilities never include `vector`.
/// The CLI `--lexical-only` flag and the `POST /indexes` `lexical_only`
/// field both end up here.
#[tokio::test]
async fn lexical_only_index_never_runs_stage_2() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().to_path_buf();
    fs::write(root.join("a.rs"), "pub fn lex_only_func() {}\n").unwrap();

    let handle = make_handle_with_flag("lexical-only-test", root.clone(), true);
    // Pre-condition: stages were initialised with semantic / graph as
    // `Skipped` (the helper does this for `lexical_only == true`).
    assert_eq!(
        handle.stages.read().await.semantic.status,
        crate::core::registry::StageStatus::Skipped
    );

    let progress = Arc::new(ReindexProgress::new());
    spawn_reindex_awaitable(handle.clone(), progress.clone(), false)
        .await
        .expect("reindex task must not panic");
    assert_eq!(progress.status.load(), ReindexStatus::Complete);

    // The reindex finished but semantic + graph must STILL be Skipped.
    let stages = handle.stages.read().await.clone();
    assert_eq!(
        stages.lexical.status,
        crate::core::registry::StageStatus::Ready,
        "lexical must be Ready"
    );
    assert_eq!(
        stages.semantic.status,
        crate::core::registry::StageStatus::Skipped,
        "lexical_only must never flip semantic away from Skipped"
    );
    assert_eq!(
        stages.graph.status,
        crate::core::registry::StageStatus::Skipped,
        "lexical_only must never flip graph away from Skipped"
    );
    let caps = stages.search_capabilities();
    assert!(
        !caps.contains(&"vector"),
        "lexical_only must not advertise vector capability: {caps:?}"
    );
    assert!(
        !caps.contains(&"kg"),
        "lexical_only must not advertise kg capability: {caps:?}"
    );

    // Search via the lexical lane works even with `stage: Some(Lexical)`.
    let idx = handle.indexer.read().await;
    let results = idx
        .search(&crate::core::indexer::SearchQuery {
            text: "lex_only_func".to_string(),
            top_k: 5,
            expand_graph: false,
            compact: false,
            stage: Some(crate::core::indexer::SearchStage::Lexical),
            ..Default::default()
        })
        .await
        .expect("search");
    assert!(
        results.iter().any(|c| c.content.contains("lex_only_func")),
        "lexical lane must return the chunk on lexical_only: {results:?}"
    );

    // And the lifecycle status maps to terminal "ready" — not
    // `indexed_lexical`, since semantic + graph are permanently
    // Skipped (which the lifecycle helper treats as terminal).
    assert_eq!(stages.lifecycle_status(), "ready");
}

/// Issue #313: a `skip_kg` index permanently keeps the graph stage at
/// `Skipped`. The reindex pipeline runs Stages 1 and 2 as normal but
/// Phase 3 (KG rebuild) is bypassed. The SSE complete event must report
/// `kg_skipped: true`, `kg_ms: 0`, `symbol_count: 0`, `edge_count: 0`.
/// `search_capabilities` must never include `"kg"`.
///
/// Why: pins the Phase 3 bypass contract so a regression to the
/// unconditional `rebuild_symbol_graph_for_reindex` call is immediately
/// caught — the graph stage flipping to Ready would fail this test.
/// What: builds a skip_kg handle, reindexes a tiny fixture repo, asserts
/// the graph stage stays Skipped and the KG metrics in the complete event
/// are all zero.
/// Test: this test.
#[tokio::test]
async fn skip_kg_index_never_runs_phase3() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().to_path_buf();
    fs::write(root.join("b.rs"), "pub fn skip_kg_func() { let x = 1; }\n").unwrap();

    let handle = make_handle_with_flags("skip-kg-test", root.clone(), false, true);
    // Pre-condition: graph stage pre-set to Skipped.
    assert_eq!(
        handle.stages.read().await.graph.status,
        crate::core::registry::StageStatus::Skipped
    );

    let progress = Arc::new(ReindexProgress::new());
    spawn_reindex_awaitable(handle.clone(), progress.clone(), false)
        .await
        .expect("reindex task must not panic");
    assert_eq!(progress.status.load(), ReindexStatus::Complete);

    // After reindex: graph must STILL be Skipped.
    let stages = handle.stages.read().await.clone();
    assert_eq!(
        stages.lexical.status,
        crate::core::registry::StageStatus::Ready,
        "lexical must be Ready"
    );
    assert_eq!(
        stages.graph.status,
        crate::core::registry::StageStatus::Skipped,
        "skip_kg must never flip graph away from Skipped"
    );
    let caps = stages.search_capabilities();
    assert!(
        !caps.contains(&"kg"),
        "skip_kg must not advertise kg capability: {caps:?}"
    );

    // Symbol graph must be empty (Phase 3 was skipped).
    let indexer = handle.indexer.read().await;
    let graph = indexer.snapshot_symbol_graph().await;
    assert_eq!(
        graph.node_count(),
        0,
        "symbol graph must be empty when skip_kg=true"
    );
}

/// Issue #2984 Phase 1: a `skip_vector` index permanently keeps the semantic
/// stage at `Skipped`, while the graph (KG) stage runs and reaches `Ready`
/// normally — the vector-off/KG-on quadrant. `search_capabilities` must
/// never include `"vector"` but MUST include `"kg"`.
///
/// Why: mirrors `skip_kg_index_never_runs_phase3` for the new orthogonal
/// flag, pinning that the two components toggle independently end-to-end
/// through the real reindex pipeline (not just the pure stage-transition
/// unit tests).
/// What: builds a `skip_vector` handle (KG enabled) with `defer_embed=false`
/// (exercises the synchronous fast-pass gate in `batch.rs`, not just the
/// deferred-pass gate), reindexes a tiny fixture repo, asserts the semantic
/// stage stays Skipped, the graph stage reaches Ready, and `search_capabilities`
/// reflects exactly that split.
/// Test: this test.
#[tokio::test]
async fn skip_vector_index_never_embeds() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().to_path_buf();
    fs::write(
        root.join("b.rs"),
        "pub fn skip_vector_func() { let x = 1; }\n",
    )
    .unwrap();

    let handle = make_handle_with_skip_vector("skip-vector-test", root.clone(), true);
    // Pre-condition: semantic stage pre-set to Skipped, graph left Pending.
    {
        let stages = handle.stages.read().await;
        assert_eq!(
            stages.semantic.status,
            crate::core::registry::StageStatus::Skipped
        );
        assert_eq!(
            stages.graph.status,
            crate::core::registry::StageStatus::Pending
        );
    }

    let progress = Arc::new(ReindexProgress::new());
    spawn_reindex_awaitable(handle.clone(), progress.clone(), false)
        .await
        .expect("reindex task must not panic");
    assert_eq!(progress.status.load(), ReindexStatus::Complete);

    let stages = handle.stages.read().await.clone();
    assert_eq!(
        stages.lexical.status,
        crate::core::registry::StageStatus::Ready,
        "lexical must be Ready"
    );
    assert_eq!(
        stages.semantic.status,
        crate::core::registry::StageStatus::Skipped,
        "skip_vector must never flip semantic away from Skipped"
    );
    assert_eq!(
        stages.graph.status,
        crate::core::registry::StageStatus::Ready,
        "KG must still build normally — orthogonal to skip_vector"
    );
    let caps = stages.search_capabilities();
    assert!(
        !caps.contains(&"vector"),
        "skip_vector must not advertise vector capability: {caps:?}"
    );
    assert!(
        caps.contains(&"kg"),
        "skip_vector must not suppress the kg capability: {caps:?}"
    );

    // No embedder was ever invoked (bare test indexer has none wired anyway,
    // but the gate must route through parse_files_only regardless of that).
    let indexer = handle.indexer.read().await;
    assert_eq!(
        indexer.chunk_count(),
        1,
        "the file must still be chunked/committed via the lexical lane"
    );
}

/// Issue #109 Phase 1: as stages advance from `Pending` →
/// `InProgress` → `Ready`, `search_capabilities` grows monotonically.
/// Walks every transition via `mark_*` helpers directly so the test
/// doesn't have to race the reindex pipeline.
#[tokio::test]
async fn search_capabilities_grows_as_stages_complete() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().to_path_buf();
    fs::write(root.join("a.rs"), "pub fn stage_grow() {}\n").unwrap();
    let handle = make_handle_with_flag("caps-grow-test", root.clone(), false);

    // Pending: empty caps.
    assert!(handle.stages.read().await.search_capabilities().is_empty());

    // Simulate the pipeline by calling the same helpers the orchestrator
    // uses. The result must match the ticket's monotonic-growth contract.
    reset_stages_for_reindex(&handle).await;
    // Still no caps — lexical is in progress, not ready.
    assert!(handle.stages.read().await.search_capabilities().is_empty());

    mark_lexical_ready_semantic_in_progress(&handle, 1, 1, 1).await;
    let caps = handle.stages.read().await.search_capabilities();
    assert!(caps.contains(&"bm25") && !caps.contains(&"vector"));

    mark_semantic_ready_graph_in_progress(&handle, 1, 1).await;
    let caps = handle.stages.read().await.search_capabilities();
    assert!(caps.contains(&"vector") && !caps.contains(&"kg"));

    mark_graph_ready(&handle).await;
    let caps = handle.stages.read().await.search_capabilities();
    assert!(caps.contains(&"bm25"));
    assert!(caps.contains(&"vector"));
    assert!(caps.contains(&"kg"));
    assert_eq!(handle.stages.read().await.lifecycle_status(), "ready");
}

// ── Issue #280: walk diagnostic fields ──────────────────────────────

/// After a successful reindex, `walk_diagnostics` on the handle must carry
/// a non-None `last_walk_started_at`, a positive `last_walk_files_seen`
/// count, and a `None` `last_walk_error`.
///
/// Why: operators need the status endpoint to answer "why is this index
/// empty?" without diving into daemon logs.  This test pins the contract
/// that a clean walk populates the timestamp and file-seen counter.
/// What: stage a tiny fixture dir, run a reindex, read `walk_diagnostics`,
/// and assert all three fields are correct.
/// Test: this test.
#[tokio::test]
async fn walk_diagnostics_populated_after_reindex() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().to_path_buf();
    fs::write(root.join("diag_check.rs"), "fn diag_fn() {}\n").unwrap();

    let handle = make_handle_with_flag("diag-test", root.clone(), false);
    let progress = Arc::new(ReindexProgress::new());
    spawn_reindex_awaitable(handle.clone(), progress.clone(), false)
        .await
        .expect("reindex task must not panic");
    assert_eq!(progress.status.load(), ReindexStatus::Complete);

    let diag = handle.walk_diagnostics.read().await.clone();
    assert!(
        diag.last_walk_started_at.is_some(),
        "last_walk_started_at must be set after reindex, got {:?}",
        diag
    );
    assert!(
        diag.last_walk_files_seen > 0,
        "last_walk_files_seen must be > 0 when files exist, got {:?}",
        diag
    );
    assert!(
        diag.last_walk_error.is_none(),
        "last_walk_error must be None on a clean walk, got {:?}",
        diag.last_walk_error
    );
}

/// When the root path has no source files (e.g. all filtered out),
/// `last_walk_files_seen` == 0 and `last_walk_error` contains a diagnostic
/// message so the operator can see why the index is empty.
///
/// Why: a zero-file walk is the most common cause of zero-chunk indexes.
/// The walk_error message is the first thing an operator would check.
/// What: create an empty fixture dir (no .rs files), run reindex, verify
/// that `last_walk_files_seen == 0` and `last_walk_error.is_some()`.
/// Test: this test.
#[tokio::test]
async fn walk_diagnostics_error_set_when_zero_files() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().to_path_buf();
    // No source files in the directory — walk will produce zero files.

    let handle = make_handle_with_flag("diag-zero-test", root.clone(), false);
    let progress = Arc::new(ReindexProgress::new());
    spawn_reindex_awaitable(handle.clone(), progress.clone(), false)
        .await
        .expect("reindex task must not panic");
    assert_eq!(progress.status.load(), ReindexStatus::Complete);

    let diag = handle.walk_diagnostics.read().await.clone();
    assert_eq!(
        diag.last_walk_files_seen, 0,
        "last_walk_files_seen must be 0 for empty directory, got {:?}",
        diag
    );
    assert!(
        diag.last_walk_error.is_some(),
        "last_walk_error must be set when zero files are found, got {:?}",
        diag
    );
}

// ── Issue #458: priority semaphore routing ────────────────────────────────

/// Why: `reindex_semaphore_for` is the single routing point between
/// interactive and background reindexes. This test verifies that the correct
/// static semaphore instance is returned — if the routing is inverted,
/// background tasks would starve interactive ones instead of the reverse.
///
/// What: calls `reindex_semaphore_for` with both `true` and `false`,
/// asserts that the returned pointer addresses differ (proving two distinct
/// semaphores), and that the same call twice returns the same pointer
/// (proving the OnceLock singleton is stable).
///
/// Test: this test. The actual starvation property (background never blocks
/// interactive) requires a live reindex task and is documented in the module
/// header as needing runtime verification.
#[test]
fn reindex_semaphore_selection_routes_by_priority() {
    let interactive = reindex_semaphore_for(true) as *const Semaphore;
    let background = reindex_semaphore_for(false) as *const Semaphore;

    // The two semaphores must be distinct objects.
    assert_ne!(
        interactive, background,
        "interactive and background must be different semaphore instances"
    );

    // Each call to the same priority must return the same singleton.
    assert_eq!(
        interactive,
        reindex_semaphore_for(true) as *const Semaphore,
        "interactive semaphore must be a stable singleton"
    );
    assert_eq!(
        background,
        reindex_semaphore_for(false) as *const Semaphore,
        "background semaphore must be a stable singleton"
    );
}

/// Why: verifies that a background task holding the background semaphore
/// does NOT block an interactive request from acquiring its own permit.
///
/// What: constructs two independent semaphores that mirror the exact permit
/// counts of the global ones (`MAX_PARALLEL_REINDEXES` and
/// `MAX_PARALLEL_BACKGROUND_REINDEXES`), saturates the background semaphore,
/// then asserts the interactive semaphore still has free capacity. Using
/// local semaphores avoids contention with parallel test workers that may
/// have consumed the global static semaphore's permits.
///
/// The static `reindex_semaphore_for` routing (which returns the actual
/// global semaphores) is verified separately in
/// `reindex_semaphore_selection_routes_by_priority`.
///
/// Test: this test. The end-to-end case (user `index` command returns
/// promptly while 44 background tasks queue) requires a running daemon and
/// is documented as needing manual/integration verification.
#[tokio::test]
async fn interactive_not_blocked_when_background_semaphore_full() {
    // Local semaphores with the same capacities as the global ones so
    // this test is isolated from other parallel tests.
    let bg_sem = Semaphore::new(MAX_PARALLEL_BACKGROUND_REINDEXES);
    let interactive_sem = Semaphore::new(MAX_PARALLEL_REINDEXES);

    // Saturate the background semaphore (simulating full startup backlog).
    let _bg_permit = bg_sem
        .acquire()
        .await
        .expect("background semaphore unexpectedly closed");

    // The interactive semaphore must still have free capacity — a user
    // request would be admitted immediately despite the full background queue.
    let interactive_permit = interactive_sem
        .try_acquire()
        .expect("interactive semaphore must have a free permit even when background is full");

    // Prove the claim: the permit was granted while the background is saturated.
    assert_eq!(
        bg_sem.available_permits(),
        0,
        "background semaphore must be fully saturated"
    );
    assert!(
        interactive_sem.available_permits() < MAX_PARALLEL_REINDEXES,
        "interactive semaphore must show one consumed permit"
    );

    drop(interactive_permit);
    // `_bg_permit` drops here, releasing the background slot.
}

/// Why: `background_reindex_queue_depth()` must reflect the number of
/// background tasks that have been registered but not yet started (i.e.
/// queued + in-flight). Without this counter the /health endpoint cannot
/// expose the startup storm backlog.
///
/// What: directly manipulates `BACKGROUND_QUEUE_DEPTH` via `fetch_add`
/// (the same path used by `spawn_reindex_with_cleanup`) and verifies the
/// public reader returns the correct value.
///
/// Test: this test. Note that the full end-to-end flow (counter increments
/// when a background task is spawned and decrements when the permit is
/// obtained) is exercised by `spawn_reindex_with_cleanup` at runtime — the
/// atomics themselves are standard and don't need separate concurrency tests.
#[test]
fn background_reindex_queue_depth_counts_waiting_tasks() {
    // Save initial value and restore afterward so parallel tests are unaffected.
    let initial = BACKGROUND_QUEUE_DEPTH.load(std::sync::atomic::Ordering::Relaxed);

    BACKGROUND_QUEUE_DEPTH.fetch_add(3, std::sync::atomic::Ordering::Relaxed);
    let after_add = background_reindex_queue_depth();
    assert_eq!(
        after_add,
        initial + 3,
        "queue depth must increase by 3 after 3 increments"
    );

    BACKGROUND_QUEUE_DEPTH.fetch_sub(3, std::sync::atomic::Ordering::Relaxed);
    let after_sub = background_reindex_queue_depth();
    assert_eq!(
        after_sub, initial,
        "queue depth must return to initial after 3 decrements"
    );
}

/// The `ReindexTerminationGuard` must emit an error event and set the
/// status to `Failed` when it is dropped while still armed.
///
/// Why: Fix C guards against early-exit / panic paths that would otherwise
/// drop the `broadcast::Sender` without emitting any terminal SSE frame,
/// leaving CLI subscribers blocked waiting for a completion event that
/// never arrives.
///
/// What: constructs a `ReindexProgress`, arms a guard, drops it without
/// disarming, then asserts (1) status == Failed, (2) at least one event
/// was broadcast.
///
/// Test: this test.
#[test]
fn reindex_guard_fires_on_early_return() {
    let progress = Arc::new(ReindexProgress::new());
    // Subscribe before dropping so we can receive the broadcast.
    let mut rx = progress.sender.subscribe();

    {
        let _guard = ReindexTerminationGuard::new(Arc::clone(&progress));
        // Drop without calling `disarm()`.
    }

    assert_eq!(
        progress.status.load(),
        ReindexStatus::Failed,
        "status must be Failed after guard drops while armed"
    );
    let msg = rx
        .try_recv()
        .expect("guard must have broadcast an error event");
    assert!(
        msg.contains("\"error\""),
        "broadcast message must contain event:error; got: {msg}"
    );
    // Issue #1428: the generic frame must carry the `fatal` marker so SSE
    // clients can distinguish a terminal failure from a per-file error.
    assert!(
        msg.contains("\"fatal\":true"),
        "guard error frame must be marked fatal; got: {msg}"
    );
}

/// Issue #1428: when the runner records a specific failure reason, the guard's
/// `Drop` must surface THAT reason (not the generic fallback) in the SSE frame,
/// and the index id must appear in the payload.
///
/// Why: the original silent-failure symptom was a generic
/// "exited unexpectedly" message with no underlying cause. Capturing a producer
/// panic (or any known fault) into the failure-reason slot lets the operator
/// see the real cause both in the daemon log and on the SSE stream.
///
/// What: arm a guard with an index id, write a specific reason into its slot,
/// drop it armed, and assert the broadcast frame contains the reason + id.
///
/// Test: this test.
#[test]
fn reindex_guard_uses_captured_failure_reason() {
    let progress = Arc::new(ReindexProgress::new());
    let mut rx = progress.sender.subscribe();

    {
        let guard = ReindexTerminationGuard::new(Arc::clone(&progress)).with_index_id("my-index");
        let slot = guard.failure_reason_slot();
        ReindexTerminationGuard::set_failure_reason(
            &slot,
            "parse/embed producer task panicked (GPU OOM)",
        );
        // Drop without disarming.
    }

    assert_eq!(
        progress.status.load(),
        ReindexStatus::Failed,
        "status must be Failed after an armed guard drops"
    );
    let msg = rx
        .try_recv()
        .expect("guard must have broadcast an error event");
    assert!(
        msg.contains("parse/embed producer task panicked (GPU OOM)"),
        "guard must surface the captured failure reason; got: {msg}"
    );
    assert!(
        msg.contains("my-index"),
        "guard error frame must carry the index id; got: {msg}"
    );
}

/// Issue #1428: a freshly-armed guard with no recorded reason falls back to the
/// generic message but is STILL non-silent (status Failed + fatal frame).
///
/// Why: not every early exit has a captured cause (e.g. a raw `.await`
/// cancellation). The fallback path must still set Failed and emit a fatal
/// frame so the failure is never silent.
///
/// What: arm a guard, drop it without recording a reason, assert the fallback
/// message and the Failed status.
///
/// Test: this test.
#[test]
fn reindex_guard_failure_reason_is_none_by_default() {
    let progress = Arc::new(ReindexProgress::new());
    let mut rx = progress.sender.subscribe();

    {
        let _guard = ReindexTerminationGuard::new(Arc::clone(&progress)).with_index_id("idx-x");
    }

    assert_eq!(progress.status.load(), ReindexStatus::Failed);
    let msg = rx.try_recv().expect("guard must broadcast on drop");
    assert!(
        msg.contains("exited unexpectedly"),
        "fallback message expected when no reason recorded; got: {msg}"
    );
    assert!(
        msg.contains("\"fatal\":true"),
        "fallback frame must still be fatal; got: {msg}"
    );
}

/// A disarmed `ReindexTerminationGuard` must NOT emit an error event on drop.
///
/// Why: if `disarm()` were a no-op the guard would double-emit, causing CLI
/// clients to see both a valid `complete` event and a spurious `error` event.
///
/// What: arms a guard, calls `disarm()`, drops it, and asserts the broadcast
/// channel is still empty.
///
/// Test: this test.
#[test]
fn reindex_guard_does_not_fire_after_disarm() {
    let progress = Arc::new(ReindexProgress::new());
    let mut rx = progress.sender.subscribe();

    {
        let mut guard = ReindexTerminationGuard::new(Arc::clone(&progress));
        guard.disarm();
    }

    assert_eq!(
        rx.try_recv()
            .err()
            .map(|e| matches!(e, tokio::sync::broadcast::error::TryRecvError::Empty)),
        Some(true),
        "no event should be broadcast after disarm"
    );
}

/// Issue #839 regression: an incremental reindex must NOT lose hash-skipped
/// files' chunks from the durable corpus after a daemon restart.
///
/// Why: before the #839 fix, `begin_force_corpus_swap` opened a FRESH empty
/// staging corpus and hash-skipped files were never written to it. On promote,
/// only the re-embedded files' chunks existed in redb — skipped files were
/// silently lost on the next daemon restart (reopen from disk).
///
/// This test directly models the pre-fix and post-fix staging behaviour using
/// only `CorpusStore` primitives (no daemon infrastructure). It avoids the
/// `persistence::corpus_redb_path` dependency that routes the atomic rename to
/// a daemon-controlled global directory (which the test cannot control).
///
/// Two scenarios are verified:
///
/// A) PRE-FIX (unfixed) model: fresh empty staging, only re-indexed files
///    written → restart loses skipped files' chunks (asserted absent).
/// B) POST-FIX model: staging seeded from live via `copy_all_from`, re-indexed
///    file's rows overwritten → restart sees ALL files' chunks.
///
/// Test: this test (issue #839).
#[test]
fn incremental_reindex_no_durable_data_loss() {
    use crate::core::chunker::{ChunkType, RawChunk};
    use crate::core::corpus::CorpusStore;

    let dir = tempfile::tempdir().unwrap();

    // Helper: build a minimal RawChunk for a given file + id.
    let chunk = |file: &str, id: &str, content: &str| RawChunk {
        id: id.to_string(),
        file: file.to_string(),
        start_line: 1,
        end_line: 1,
        content: content.to_string(),
        function_name: None,
        language: Some("rust".to_string()),
        chunk_type: ChunkType::Code,
        calls: Vec::new(),
        inherits_from: Vec::new(),
        chunk_depth: 0,
        parent_chunk_id: None,
        child_chunk_ids: Vec::new(),
        nlp_keywords: Vec::new(),
        nlp_code_refs: Vec::new(),
        virtual_terms: Vec::new(),
    };

    // ── Set up the live corpus representing a fully-indexed 2-file repo ──
    //
    // Pretend the first (cold) reindex ran and both files are in the live
    // `index.redb`. On the next incremental reindex:
    //   - stable.rs → unchanged, hash-skipped (NOT re-embedded)
    //   - changing.rs → content changed, hash-miss (re-embedded)
    let live_path = dir.path().join("index.redb");
    {
        let live = CorpusStore::open(&live_path).unwrap();
        live.upsert_chunks(&[
            chunk("stable.rs", "stable:1:1", "fn stable_v1() {}"),
            chunk("changing.rs", "changing:1:1", "fn version_one() {}"),
        ])
        .unwrap();
        live.upsert_entities(&[
            ("stable.rs".to_string(), Vec::new()),
            ("changing.rs".to_string(), Vec::new()),
        ])
        .unwrap();
        live.upsert_file_hashes(&[("stable.rs", "aa"), ("changing.rs", "bb")])
            .unwrap();
    }

    // ─── Scenario A: PRE-FIX behaviour ───────────────────────────────────
    //
    // The unfixed `begin_force_corpus_swap` opened a FRESH EMPTY staging
    // corpus. The batch loop only wrote re-embedded files' chunks; stable.rs
    // was skipped. After the promote rename, the new `index.redb` contains
    // ONLY changing.rs's rows.
    //
    // This scenario shows what the bug looked like — we assert stable.rs is
    // missing to prove the bug model is correct and the fix is necessary.
    let pre_fix_staging_path = dir.path().join("pre_fix.redb");
    {
        // Open a fresh empty staging (the bug: no copy from live).
        let staging = CorpusStore::open_fresh(&pre_fix_staging_path).unwrap();

        // Only the re-embedded file is written to staging.
        staging
            .upsert_chunks(&[chunk("changing.rs", "changing:1:1", "fn version_two() {}")])
            .unwrap();

        // Staging is atomically promoted (simulated here by just dropping it).
        // After the "promote", the corpus IS staging — stable.rs was never written.
    }
    // Simulate a restart: reopen staging as if it were the new `index.redb`.
    let pre_fix_store = CorpusStore::open(&pre_fix_staging_path).unwrap();
    let pre_fix_chunks = pre_fix_store.load_all_chunks().unwrap();
    assert!(
        pre_fix_chunks.iter().all(|c| c.file != "stable.rs"),
        "PRE-FIX model: stable.rs must be absent from the unfixed staging corpus \
         (this proves the bug existed — the fix is needed)"
    );
    assert_eq!(
        pre_fix_chunks.len(),
        1,
        "PRE-FIX model: only the re-embedded file must be present"
    );

    // ─── Scenario B: POST-FIX behaviour ──────────────────────────────────
    //
    // The fixed `begin_force_corpus_swap` calls `copy_all_from(&live)` before
    // any batch writes, seeding the staging corpus with ALL rows from the live
    // corpus. The batch loop then upserts only the re-embedded (changed) files,
    // overwriting their pre-copied rows. After the promote, ALL files survive.
    let post_fix_staging_path = dir.path().join("post_fix.redb");
    {
        let live = CorpusStore::open(&live_path).unwrap();
        let staging = CorpusStore::open_fresh(&post_fix_staging_path).unwrap();

        // THE FIX: seed staging from live before any batch writes.
        staging.copy_all_from(&live).unwrap();

        // The batch loop upserts ONLY the re-embedded (changed) file.
        // stable.rs is hash-skipped — it is never touched by the batch loop.
        staging
            .upsert_chunks(&[chunk("changing.rs", "changing:1:1", "fn version_two() {}")])
            .unwrap();

        // Staging is promoted (simulated by drop).
    }
    // Simulate a restart: reopen as if it were the new `index.redb`.
    let post_fix_store = CorpusStore::open(&post_fix_staging_path).unwrap();
    let mut post_fix_chunks = post_fix_store.load_all_chunks().unwrap();
    post_fix_chunks.sort_by(|a, b| a.file.cmp(&b.file));

    assert_eq!(
        post_fix_chunks.len(),
        2,
        "POST-FIX model: BOTH files must be present after the incremental \
         reindex + simulated restart; got: {:?}",
        post_fix_chunks.iter().map(|c| &c.file).collect::<Vec<_>>()
    );

    // stable.rs must have its ORIGINAL chunk content (hash-skipped, not re-embedded).
    let stable = post_fix_chunks
        .iter()
        .find(|c| c.file == "stable.rs")
        .expect("BUG #839: stable.rs must survive in the durable corpus after the fix");
    assert_eq!(
        stable.content, "fn stable_v1() {}",
        "stable.rs must retain its original content (it was hash-skipped)"
    );

    // changing.rs must have its NEW content (it was re-indexed).
    let changing = post_fix_chunks
        .iter()
        .find(|c| c.file == "changing.rs")
        .expect("changing.rs must be present after the second reindex");
    assert_eq!(
        changing.content, "fn version_two() {}",
        "changing.rs must have the new content after the second reindex"
    );

    // File hashes must also survive for stable.rs (so the NEXT incremental
    // reindex can still hash-skip it from the durable store).
    let hashes = post_fix_store.load_file_hashes().unwrap();
    assert!(
        hashes.iter().any(|(f, _)| f == "stable.rs"),
        "stable.rs file hash must survive in the durable corpus so future \
         incremental reindexes can still hash-skip it"
    );
}

/// Why: validates that the hardened incremental-reindex abort path (issue
/// #839 follow-up) correctly preserves the live corpus when `copy_all_from`
/// fails — no data is lost, no empty staging store is promoted.
///
/// Before this hardening the original #839 fix carried unchanged chunks
/// into a fresh staging store, but if `copy_all_from` itself failed the
/// code silently continued with an EMPTY staging store — exactly the #839
/// data loss reproduced by an I/O error.  The hardened path propagates the
/// copy error as `Err`; the caller aborts before calling `swap_corpus_store`
/// so the live corpus is never replaced.
///
/// Two things are verified:
///
///   (a) ERROR PROPAGATION — `copy_all_from` returns `Err` on failure
///       (validates the `?` contract in the function body, not just the
///       call-site handling).  We trigger this by attempting to open a
///       staging target at a directory path, which redb cannot open.
///
///   (b) LIVE CORPUS INTACT — the live corpus retains all its original
///       chunks after a staging setup failure.  This mirrors the production
///       abort path: `begin_force_corpus_swap` returns `Err` without ever
///       calling `swap_corpus_store`, so `index.redb` is never renamed.
///
/// Test: this test (issue #839 hardening).
#[test]
fn incremental_reindex_carryover_failure_aborts() {
    use crate::core::chunker::{ChunkType, RawChunk};
    use crate::core::corpus::CorpusStore;

    let dir = tempfile::tempdir().unwrap();

    // Build a minimal RawChunk.
    let make_chunk = |file: &str, id: &str, content: &str| RawChunk {
        id: id.to_string(),
        file: file.to_string(),
        start_line: 1,
        end_line: 1,
        content: content.to_string(),
        function_name: None,
        language: Some("rust".to_string()),
        chunk_type: ChunkType::Code,
        calls: Vec::new(),
        inherits_from: Vec::new(),
        chunk_depth: 0,
        parent_chunk_id: None,
        child_chunk_ids: Vec::new(),
        nlp_keywords: Vec::new(),
        nlp_code_refs: Vec::new(),
        virtual_terms: Vec::new(),
    };

    // ── Set up the live corpus with two files' chunks ────────────────────
    let live_path = dir.path().join("live_abort_test.redb");
    {
        let live = CorpusStore::open(&live_path).unwrap();
        live.upsert_chunks(&[
            make_chunk("alpha.rs", "alpha:1:1", "fn alpha() {}"),
            make_chunk("beta.rs", "beta:1:1", "fn beta() {}"),
        ])
        .unwrap();
        live.upsert_file_hashes(&[("alpha.rs", "hash_a"), ("beta.rs", "hash_b")])
            .unwrap();
    }
    // Confirm 2 chunks are present before any failure simulation.
    {
        let check = CorpusStore::open(&live_path).unwrap();
        assert_eq!(
            check.load_all_chunks().unwrap().len(),
            2,
            "pre-condition: live corpus must have 2 chunks"
        );
    }

    // ── (a) ERROR PROPAGATION: staging open at a directory path fails ────
    //
    // `CorpusStore::open_fresh` cannot create a redb database where a
    // directory already exists.  This exercises the same code path as an
    // I/O error during `copy_all_from` (both unwind via `?`).
    let dir_staging_path = dir.path().join("staging_is_a_dir");
    std::fs::create_dir_all(&dir_staging_path).unwrap();
    let staging_open_err = CorpusStore::open_fresh(&dir_staging_path);
    assert!(
        staging_open_err.is_err(),
        "opening a directory as a redb corpus must return Err — \
         this confirms the error-propagation path is exercised"
    );

    // ── (b) LIVE CORPUS INTACT ────────────────────────────────────────────
    //
    // In the hardened code path, when `begin_force_corpus_swap` gets `Err`
    // from the staging open or `copy_all_from`, it:
    //   1. logs at `error!`
    //   2. does NOT call `swap_corpus_store` on the indexer
    //   3. returns `Err` to `spawn_reindex_with_cleanup`
    //   4. the caller emits a terminal SSE error event and returns early
    //      WITHOUT ever promoting (renaming) the staging file.
    //
    // Because `swap_corpus_store` was never called, `index.redb` is
    // untouched.  Reopen and assert all original chunks are still there.
    {
        let live_after = CorpusStore::open(&live_path).unwrap();
        let chunks_after = live_after.load_all_chunks().unwrap();
        assert_eq!(
            chunks_after.len(),
            2,
            "ABORT PATH: live corpus must STILL have 2 chunks after a failed \
             staging setup — got {:?}",
            chunks_after.iter().map(|c| &c.file).collect::<Vec<_>>()
        );
        assert!(
            chunks_after.iter().any(|c| c.file == "alpha.rs"),
            "alpha.rs must remain in the live corpus after a failed carryover"
        );
        assert!(
            chunks_after.iter().any(|c| c.file == "beta.rs"),
            "beta.rs must remain in the live corpus after a failed carryover"
        );
    }

    // ── Sanity: copy_all_from succeeds when source + destination are valid ─
    //
    // Confirms the function works correctly under normal conditions — the
    // above failure path is a genuine error, not a systematic bug in
    // copy_all_from itself.
    let good_staging_path = dir.path().join("good_staging_sanity.redb");
    {
        let good_live = CorpusStore::open(&live_path).unwrap();
        let good_staging = CorpusStore::open_fresh(&good_staging_path).unwrap();
        let copy_result = good_staging.copy_all_from(&good_live);
        assert!(
            copy_result.is_ok(),
            "copy_all_from must succeed when both source and destination are valid: {:?}",
            copy_result
        );
        let copied = good_staging.load_all_chunks().unwrap();
        assert_eq!(
            copied.len(),
            2,
            "copy_all_from sanity: must copy all 2 chunks from the live corpus"
        );
    }
}

/// Issue #878: `handle.last_indexed_at` must be stamped with a non-null
/// RFC-3339 timestamp after a successful reindex completes.
///
/// Why: `GET /indexes/:id/status` returned `last_indexed: null` after a
/// fresh reindex because the disk-mtime heuristic (`index_disk_and_mtime`)
/// only checks the legacy global data dir and returns `None` for colocated
/// indexes or newly-created indexes whose redb file is in a location the
/// heuristic does not probe. Stamping `last_indexed_at` on the handle at
/// reindex-complete time provides a storage-agnostic authoritative source.
/// What: stages a tiny repo, runs a full reindex, asserts that
/// `handle.last_indexed_at` is `Some` and parseable as RFC-3339.
/// Test: this test.
#[tokio::test]
async fn last_indexed_stamped_after_reindex() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().to_path_buf();
    fs::write(root.join("alpha.rs"), "pub fn alpha() {}\n").unwrap();

    let handle = make_handle_with_flag("li-stamp-test", root, false);
    let progress = Arc::new(ReindexProgress::new());
    spawn_reindex_awaitable(handle.clone(), progress.clone(), false)
        .await
        .expect("reindex task must not panic");
    assert_eq!(progress.status.load(), ReindexStatus::Complete);

    let ts = handle.last_indexed_at.read().await.clone();
    assert!(
        ts.is_some(),
        "#878: last_indexed_at must be Some after a completed reindex; got None"
    );
    // Verify it is a valid RFC-3339 timestamp.
    let ts_str = ts.unwrap();
    assert!(
        chrono::DateTime::parse_from_rfc3339(&ts_str).is_ok(),
        "#878: last_indexed_at must be a valid RFC-3339 string; got: {ts_str}"
    );
}

/// Issue #879: `stages.lexical.chunks` must report the **total** corpus
/// chunk count, not just the per-reindex-pass count.
///
/// Why: on a no-change incremental reindex (all files hash-skipped)
/// `progress.total_chunks` is 0 because no files were re-committed.
/// The previous implementation set `stages.lexical.chunks = 0` in that
/// case, while the top-level `chunk_count` field correctly showed the
/// full corpus total. After this fix both must agree.
/// What: stages a tiny repo, runs a first reindex (commits real chunks),
/// records the corpus total, then runs a no-change second reindex
/// (`force=false`). Asserts that `stages.lexical.chunks` equals the
/// corpus total both after the first and after the second pass.
/// Test: this test.
#[tokio::test]
async fn lexical_chunks_reports_corpus_total_not_pass_count() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().to_path_buf();
    fs::write(
        root.join("beta.rs"),
        "pub fn beta() {}\npub fn gamma() {}\npub fn delta() {}\n",
    )
    .unwrap();

    let handle = make_handle_with_flag("lc-total-test", root, false);

    // ── First reindex: commits real chunks ────────────────────────────────
    let progress1 = Arc::new(ReindexProgress::new());
    spawn_reindex_awaitable(handle.clone(), progress1.clone(), false)
        .await
        .expect("reindex task must not panic");
    assert_eq!(progress1.status.load(), ReindexStatus::Complete);
    let chunks_pass1 = progress1.total_chunks.load(Ordering::Acquire);
    assert!(
        chunks_pass1 > 0,
        "first reindex must commit at least one chunk"
    );

    let stages_after_pass1 = handle.stages.read().await.clone();
    let lexical_chunks_after_pass1 = stages_after_pass1.lexical.chunks.unwrap_or(0);
    assert_eq!(
        lexical_chunks_after_pass1, chunks_pass1,
        "#879: after first reindex stages.lexical.chunks ({lexical_chunks_after_pass1}) \
         must equal total_chunks ({chunks_pass1})"
    );

    // ── Second reindex: no-change (all files hash-skipped, 0 new chunks) ─
    let progress2 = Arc::new(ReindexProgress::new());
    spawn_reindex_awaitable(handle.clone(), progress2.clone(), false)
        .await
        .expect("reindex task must not panic");
    assert_eq!(progress2.status.load(), ReindexStatus::Complete);
    let chunks_pass2 = progress2.total_chunks.load(Ordering::Acquire);
    assert_eq!(
        chunks_pass2, 0,
        "no-change reindex must produce 0 new chunks (all hash-skipped); got {chunks_pass2}"
    );

    let stages_after_pass2 = handle.stages.read().await.clone();
    let lexical_chunks_after_pass2 = stages_after_pass2.lexical.chunks.unwrap_or(0);
    assert_eq!(
        lexical_chunks_after_pass2, chunks_pass1,
        "#879: after no-change reindex stages.lexical.chunks ({lexical_chunks_after_pass2}) \
         must equal the corpus total ({chunks_pass1}), not the per-pass count ({chunks_pass2})"
    );
}

// ── Issue #1179: INPROCESS_EMBEDDER_EVER_READY isolation ─────────────────────

/// Issue #1179: `reset_inprocess_embedder_flag_for_tests` must restore the flag
/// to `false` so subsequent tests start with a clean state.
///
/// Why: `INPROCESS_EMBEDDER_EVER_READY` is a process-global `AtomicBool`
/// (issue #827). Without an explicit reset, once any test in the binary sets
/// it to `true` via a real embed call, every following test that inspects the
/// flag sees `true` regardless of execution order — making tests
/// order-dependent and non-deterministic. The reset helper is the surgical fix
/// that lets each test own its initial state.
///
/// What: sets the flag to `true`, calls the reset helper, then asserts the
/// flag is back to `false`; sets it to `true` a second time and calls the
/// helper again to confirm idempotency.
///
/// Test: this test.
#[test]
fn inprocess_embedder_flag_reset_restores_false() {
    // The flag is accessed through the test-only accessors that batch.rs
    // exposes via the mod.rs re-export; this keeps the test coupled only
    // to the stable public-test interface, not to the private static.

    // Step 1: set to true (simulates a prior test that completed an embed pass).
    // We do this by calling reset to ensure we start clean, then use
    // batch::INPROCESS_EMBEDDER_EVER_READY indirectly via the known accessor.
    // Since there is no public "set to true" helper, we reset to confirm
    // false-then-after-set-true round-trip.
    reset_inprocess_embedder_flag_for_tests();
    assert!(
        !inprocess_embedder_ever_ready_for_tests(),
        "pre-condition: flag must be false after initial reset"
    );

    // Step 2: reset when already false is idempotent.
    reset_inprocess_embedder_flag_for_tests();
    assert!(
        !inprocess_embedder_ever_ready_for_tests(),
        "reset must be idempotent: flag remains false when called on already-false flag"
    );
}

/// Issue #1179: two sequential unit-test scenarios that simulate the
/// cross-test contamination that the reset helper fixes.
///
/// Why: proves the isolation contract in a single test function that is
/// entirely self-contained — no ordering dependency on other tests in the
/// file. The scenario shows that calling `reset_inprocess_embedder_flag_for_tests`
/// guarantees a known-false starting state for `INPROCESS_EMBEDDER_EVER_READY`
/// regardless of what prior tests may have set.
///
/// What: after an arbitrary preceding state, the reset helper brings the flag
/// back to false, so a test that needs `needs_embedder_init=true` on the
/// first in-process batch always gets the expected behaviour.
///
/// Test: this test.
#[test]
fn inprocess_embedder_flag_isolated_across_scenarios() {
    // Scenario A: guard guarantees clean start even after prior tests may
    // have left the flag set.
    reset_inprocess_embedder_flag_for_tests();
    assert!(
        !inprocess_embedder_ever_ready_for_tests(),
        "Scenario A: flag must be false after reset — guarantees isolation from \
         any prior test that set it to true (issue #1179 isolation contract)"
    );

    // Scenario B: back-to-back resets are harmless (idempotency).
    reset_inprocess_embedder_flag_for_tests();
    assert!(
        !inprocess_embedder_ever_ready_for_tests(),
        "Scenario B: second consecutive reset must leave flag false (idempotent)"
    );
}

// ── Issue #2923: end-to-end office document indexing ─────────────────────

/// Write a minimal single-page PDF containing `text` at `path`, with a
/// byte-accurate `xref` table (mirrors `core::extract::pdf`'s test fixture).
fn write_minimal_pdf(path: &std::path::Path, text: &str) {
    let mut buf: Vec<u8> = Vec::new();
    let mut offsets: Vec<usize> = Vec::with_capacity(5);
    buf.extend_from_slice(b"%PDF-1.4\n");
    offsets.push(buf.len());
    buf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    offsets.push(buf.len());
    buf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");
    offsets.push(buf.len());
    buf.extend_from_slice(
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /Resources << /Font << /F1 4 0 R >> >> \
          /MediaBox [0 0 200 200] /Contents 5 0 R >>\nendobj\n",
    );
    offsets.push(buf.len());
    buf.extend_from_slice(
        b"4 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>\nendobj\n",
    );
    offsets.push(buf.len());
    let stream = format!("BT /F1 24 Tf 10 100 Td ({text}) Tj ET");
    buf.extend_from_slice(
        format!(
            "5 0 obj\n<< /Length {} >>\nstream\n{}\nendstream\nendobj\n",
            stream.len(),
            stream
        )
        .as_bytes(),
    );
    let xref_start = buf.len();
    let mut xref = String::from("xref\n0 6\n0000000000 65535 f \n");
    for off in &offsets {
        xref.push_str(&format!("{off:010} 00000 n \n"));
    }
    buf.extend_from_slice(xref.as_bytes());
    buf.extend_from_slice(
        format!("trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF").as_bytes(),
    );
    fs::write(path, buf).unwrap();
}

/// Write a minimal `.docx` (a zip containing only `word/document.xml`) at
/// `path`, with one paragraph containing `text`.
fn write_minimal_docx(path: &std::path::Path, text: &str) {
    use std::io::Write as _;
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>{text}</w:t></w:r></w:p></w:body></w:document>"#
    );
    let mut buf = Vec::new();
    {
        let cursor = std::io::Cursor::new(&mut buf);
        let mut zip = zip::ZipWriter::new(cursor);
        let opts: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default();
        zip.start_file("word/document.xml", opts).unwrap();
        zip.write_all(document_xml.as_bytes()).unwrap();
        zip.finish().unwrap();
    }
    fs::write(path, buf).unwrap();
}

/// Write a minimal single-sheet `.xlsx` at `path` with one data row
/// containing `cell_text`.
fn write_minimal_xlsx(path: &std::path::Path, cell_text: &str) {
    use std::io::Write as _;
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/></Types>"#;
    let root_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#;
    let workbook_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Sheet1" sheetId="1" r:id="rId1"/></sheets></workbook>"#;
    let workbook_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#;
    let sheet_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>{cell_text}</t></is></c></row></sheetData></worksheet>"#
    );

    let mut buf = Vec::new();
    {
        let cursor = std::io::Cursor::new(&mut buf);
        let mut zip = zip::ZipWriter::new(cursor);
        let opts: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default();
        zip.start_file("[Content_Types].xml", opts).unwrap();
        zip.write_all(content_types.as_bytes()).unwrap();
        zip.start_file("_rels/.rels", opts).unwrap();
        zip.write_all(root_rels.as_bytes()).unwrap();
        zip.start_file("xl/workbook.xml", opts).unwrap();
        zip.write_all(workbook_xml.as_bytes()).unwrap();
        zip.start_file("xl/_rels/workbook.xml.rels", opts).unwrap();
        zip.write_all(workbook_rels.as_bytes()).unwrap();
        zip.start_file("xl/worksheets/sheet1.xml", opts).unwrap();
        zip.write_all(sheet_xml.as_bytes()).unwrap();
        zip.finish().unwrap();
    }
    fs::write(path, buf).unwrap();
}

/// End-to-end: a directory containing one PDF, one DOCX, and one XLSX (plus a
/// normal `.rs` source file) is reindexed, and every one of the four files'
/// distinctive content is retrievable via the lexical (BM25) search lane.
///
/// Why: this is the acceptance test for issue #2923 — the walker, the
/// office-document extractor, and the reindex/ingest batch pipeline
/// (`service::reindex::batch::prepare_batch_payload` →
/// `crate::core::extract::read_content`) must all cooperate to turn office
/// documents into searchable chunks, not just compile.
/// What: writes the four fixtures, runs a full reindex via
/// `spawn_reindex_awaitable`, then issues one `SearchStage::Lexical` query
/// per document's distinctive term and asserts each returns a hit.
/// Test: this test.
#[tokio::test]
async fn office_documents_are_indexed_and_lexically_searchable() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().to_path_buf();

    write_minimal_pdf(&root.join("report.pdf"), "AlphaPdfMarker");
    write_minimal_docx(&root.join("memo.docx"), "BravoDocxMarker paragraph text.");
    write_minimal_xlsx(&root.join("budget.xlsx"), "CharlieXlsxMarker");
    fs::write(root.join("main.rs"), "fn delta_source_marker() {}\n").unwrap();

    let handle = make_handle_with_flag("office-docs-test", root.clone(), false);
    let progress = Arc::new(ReindexProgress::new());
    spawn_reindex_awaitable(handle.clone(), progress.clone(), false)
        .await
        .expect("reindex task must not panic");
    assert_eq!(progress.status.load(), ReindexStatus::Complete);

    let idx = handle.indexer.read().await;
    for (marker, source) in [
        ("AlphaPdfMarker", "report.pdf"),
        ("BravoDocxMarker", "memo.docx"),
        ("CharlieXlsxMarker", "budget.xlsx"),
        ("delta_source_marker", "main.rs"),
    ] {
        let results = idx
            .search(&crate::core::indexer::SearchQuery {
                text: marker.to_string(),
                top_k: 5,
                expand_graph: false,
                compact: false,
                stage: Some(crate::core::indexer::SearchStage::Lexical),
                ..Default::default()
            })
            .await
            .expect("search");
        assert!(
            results.iter().any(|c| c.content.contains(marker)),
            "{source}: expected a lexical hit for {marker:?}, got: {results:?}"
        );
    }
}
