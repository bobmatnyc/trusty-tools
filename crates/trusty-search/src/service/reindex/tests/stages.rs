/// Staged-pipeline tests: stage transitions, lexical_only, skip_kg, search
/// capability growth, and walk diagnostic fields.
use super::*;

/// Issue #109 Phase 1: after a BM25-only reindex (no embedder wired) Stage 1
/// must complete and search via the lexical lane must return the indexed chunk.
///
/// Why: verifies that the staged pipeline does not dead-lock when no embedder
/// is present — BM25 stage completes independently of the vector stage.
/// What: stages a fixture, runs a reindex without an embedder, asserts the
/// lexical stage is Ready and BM25 search returns results.
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
    spawn_reindex(handle.clone(), progress.clone(), false);

    for _ in 0..200 {
        if progress.status.load() == ReindexStatus::Complete {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
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
    spawn_reindex(handle.clone(), progress.clone(), false);
    for _ in 0..200 {
        if progress.status.load() == ReindexStatus::Complete {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
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
    spawn_reindex(handle.clone(), progress.clone(), false);
    for _ in 0..200 {
        if progress.status.load() == ReindexStatus::Complete {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
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
    spawn_reindex(handle.clone(), progress.clone(), false);

    for _ in 0..100 {
        if progress.status.load() == ReindexStatus::Complete {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
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
    spawn_reindex(handle.clone(), progress.clone(), false);

    for _ in 0..100 {
        if progress.status.load() == ReindexStatus::Complete {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
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
