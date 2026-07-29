//! Regression tests for restore-derived stage status on `POST /indexes` (#4110).
//!
//! Why: `create_index_handler` doubles as the "adopt an existing colocated
//! corpus" door — `build_indexer_from_entry` synchronously restores the redb
//! corpus, the HNSW snapshot and the symbol graph — but the handler then
//! asserted `lexical: pending(), semantic: pending()` unconditionally,
//! throwing that outcome away. `search_capabilities` is derived from `stages`,
//! so a fully-intact index came up advertising no vector lane: semantic search
//! hard-errored with "requires Stage 2 (embeddings), which is not yet ready"
//! and `search_all` silently degraded to BM25-only, every hit reporting
//! `match_reason="bm25"` — indistinguishable from a genuinely dead lane. Only
//! a daemon restart cleared it, because the warm-boot path already classified
//! correctly from the same signals.
//! What: both tests drive the REAL `create_index_handler` through a
//! create → index content → unregister → re-register cycle, so the second
//! registration performs a genuine colocated restore. The pair differs in ONE
//! input — whether an HNSW snapshot was persisted before the re-register — so
//! what is under test is the MAPPING from restore outcome to stage status, not
//! a literal: the same handler must report `semantic: Ready` for one and
//! `semantic: Pending` for the other. Both fail before the fix, which reports
//! `Pending` for every stage regardless of what restored.
//! Test: this module. Run with `cargo test -p trusty-search tests_4110`.

use super::*;
use crate::core::embed::Embedder;
use crate::core::registry::{IndexId, IndexRegistry, StageStatus};
use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use std::sync::Arc;

/// A `CreateIndexRequest` with every optional field defaulted.
///
/// Why: mirrors `tests_2984::create_req_with_skip_kg` — this suite needs no
/// per-field variation, only the full-pipeline defaults, so that the stage
/// status observed comes from the restore and nothing else.
fn create_req(id: &str, root_path: std::path::PathBuf) -> super::router::CreateIndexRequest {
    super::router::CreateIndexRequest {
        id: id.to_string(),
        root_path,
        include_paths: None,
        exclude_globs: None,
        extensions: None,
        domain_terms: None,
        path_filter: None,
        include_docs: None,
        respect_gitignore: None,
        follow_links: None,
        lexical_only: None,
        skip_kg: None,
        skip_vector: None,
        defer_embed: None,
        extra_skip_dirs: None,
        data_file_max_bytes: None,
        allow_sensitive_path: false,
    }
}

/// Fresh registry with a mock embedder installed — enough for
/// `create_index_handler` to run without a live daemon or network. The mock's
/// 8 dimensions are used for BOTH registrations in a test, so a persisted
/// snapshot always reloads at a matching dimension (a mismatch would set
/// `hnsw_load_failed` and defeat the scenario under test).
async fn mock_state() -> Arc<SearchAppState> {
    let state = SearchAppState::new(IndexRegistry::new());
    let embedder: Arc<dyn Embedder> = Arc::new(crate::core::embed::MockEmbedder::new(8));
    state.install_embedder(embedder).await;
    Arc::new(state)
}

/// Run the create → index-content → (optionally persist HNSW) → unregister →
/// re-register cycle and return the stages the SECOND registration produced.
///
/// Why: the second `create_index_handler` call is the code path under test —
/// the first exists only to lay down a real colocated corpus for it to
/// restore. Sharing the driver between both tests is what makes the pair a
/// controlled experiment: `persist_hnsw` is the single differing input.
/// What: indexes two files with a caller→callee edge so the corpus is
/// genuinely non-empty, optionally writes the HNSW snapshot to
/// `<root>/.trusty-search/hnsw.usearch` via `save_vector_store`, then stops
/// the watcher and unregisters (both required to release the redb file lock
/// and clear the handler's "already exists" short-circuit — see `tests_2984`
/// for the full rationale) before re-POSTing the same id/root.
async fn stages_after_restore(
    prefix: &str,
    persist_hnsw: bool,
) -> crate::core::registry::IndexStages {
    let state = mock_state().await;
    let (_dir, root) = super::test_support::allowlisted_index_root(prefix);
    let id = IndexId::new(format!("{}restore", prefix));

    let first = super::indexes::create_index_handler(
        State(Arc::clone(&state)),
        Json(create_req(&id.0, root.clone())),
    )
    .await;
    assert_eq!(first.status(), StatusCode::OK, "first create must succeed");

    {
        let handle = state
            .registry
            .get(&id)
            .expect("index must be registered after create");
        let indexer = handle.indexer.read().await;
        indexer
            .index_files_batch(&[
                ("src/caller.rs".into(), "fn caller() { callee(); }".into()),
                ("src/callee.rs".into(), "fn callee() {}".into()),
            ])
            .await
            .expect("index batch");
        if persist_hnsw {
            let hnsw_path = crate::service::colocated_storage::colocated_hnsw_path(&root)
                .expect("colocated hnsw path");
            assert!(
                indexer
                    .save_vector_store(&hnsw_path)
                    .await
                    .expect("save vector store"),
                "sanity: a store must be wired, otherwise no snapshot is written \
                 and the 'successful restore' scenario is vacuous"
            );
            assert!(
                hnsw_path.exists(),
                "sanity: the HNSW snapshot must exist on disk before re-register"
            );
        }
    }

    // Release the corpus Arc (and its redb file lock) and clear the handler's
    // in-memory "already exists" early return, so the re-POST below actually
    // reaches the restore path. See `tests_2984` for why both steps are
    // required.
    state.watcher_manager.stop_for_index(&id).await;
    assert!(
        state.registry.unregister(&id),
        "index must have been registered to unregister"
    );

    let second = super::indexes::create_index_handler(
        State(Arc::clone(&state)),
        Json(create_req(&id.0, root.clone())),
    )
    .await;
    assert_eq!(
        second.status(),
        StatusCode::OK,
        "re-register over the existing colocated corpus must succeed"
    );

    let restored = state
        .registry
        .get(&id)
        .expect("index must be registered after re-create");
    let stages = restored.stages.read().await.clone();
    state.watcher_manager.stop_for_index(&id).await;
    stages
}

/// Issue #4110: a re-registration that successfully restores a colocated
/// corpus WITH an HNSW snapshot must report the semantic stage ready.
///
/// Why: this is the reported defect. Before the fix the handler ignored the
/// restore entirely and reported `Pending`, so semantic search hard-errored
/// against a fully-intact index until the daemon was restarted.
/// Test: this test.
///
/// `#[serial]` because `create_index_handler` persists to `indexes.toml` under
/// whatever `TRUSTY_DATA_DIR` resolves to at that instant. Several suites in
/// this binary (e.g. `tests_components::IsolatedDataDir`) redirect that env var
/// for the duration of a serial test, so running unserialised would write this
/// index's registry entry into THEIR sandbox and can fail their persistence
/// assertions with a 500 — observed once under heavy parallel load.
#[tokio::test]
#[serial_test::serial]
async fn create_index_successful_restore_reports_semantic_ready() {
    let stages = stages_after_restore("ts-4110-with-hnsw-", true).await;

    assert_eq!(
        stages.semantic.status,
        StageStatus::Ready,
        "#4110: a restored HNSW snapshot must map to semantic Ready, not \
         Pending — got {:?}",
        stages.semantic.status
    );
    assert_eq!(
        stages.lexical.status,
        StageStatus::Ready,
        "#4110: a restored non-empty corpus must map to lexical Ready — got {:?}",
        stages.lexical.status
    );
    assert!(
        stages.search_capabilities().contains(&"vector"),
        "#4110: the vector lane must be advertised so search_all does not \
         silently degrade to BM25-only; capabilities were {:?}",
        stages.search_capabilities()
    );
}

/// Issue #4110 (the other direction): a restore with NO HNSW snapshot must
/// leave the semantic stage pending, while still reporting the lexical stage
/// ready from the corpus that DID restore.
///
/// Why: guards the fix against over-correcting into a false-ready — the exact
/// failure mode `derive_warm_boot_stages`' `#2922` / `#2203` guards exist to
/// prevent. The lexical assertion is what makes this test non-vacuous: before
/// the fix lexical was hardcoded `Pending`, so this fails too, proving the
/// handler now genuinely reads the restore rather than emitting a constant
/// that happens to match in one branch.
/// Test: this test.
///
/// `#[serial]` for the same reason as its sibling above.
#[tokio::test]
#[serial_test::serial]
async fn create_index_restore_without_hnsw_snapshot_stays_pending() {
    let stages = stages_after_restore("ts-4110-no-hnsw-", false).await;

    assert_eq!(
        stages.semantic.status,
        StageStatus::Pending,
        "#4110: with no HNSW snapshot on disk the semantic stage must stay \
         Pending, never a false-ready — got {:?}",
        stages.semantic.status
    );
    assert_eq!(
        stages.lexical.status,
        StageStatus::Ready,
        "#4110: the corpus DID restore, so lexical must be Ready — a Pending \
         here means the handler is still ignoring the restore result entirely \
         (got {:?})",
        stages.lexical.status
    );
    assert!(
        !stages.search_capabilities().contains(&"vector"),
        "#4110: the vector lane must NOT be advertised without a snapshot; \
         capabilities were {:?}",
        stages.search_capabilities()
    );
}
