//! Handler-level regression test for issue #313 / #2984 (Phase 0 follow-up):
//! `create_index_handler` must honor `skip_kg` on the re-register door, not
//! just via `build_indexer_from_entry` in isolation.
//!
//! Why: the code-critic re-review of PR #2988 flagged that the existing
//! `skip_kg` regression coverage (`persistence_loader::tests::
//! skip_kg_true_entry_never_loads_persisted_graph_via_build_indexer_from_entry`)
//! calls `build_indexer_from_entry` directly — the shared helper that was
//! ALREADY correct before this PR's delta (it has always honored
//! `entry.skip_kg`). The actual bug (and actual fix, `indexes.rs:314-324`)
//! lived one layer up: `create_index_handler` built its `init_entry` with
//! `..Default::default()` (`skip_kg = false`) BEFORE the real `skip_kg`
//! value was computed further down the handler. A revert of that ordering
//! fix (e.g. someone moving the `skip_kg` computation back below
//! `init_entry`'s construction) would slip past every existing test, because
//! none of them call `create_index_handler` itself with `skip_kg: Some(true)`.
//! What: registers an index via `create_index_handler` with `skip_kg: Some(false)`,
//! indexes real content with a caller→callee edge so a non-empty symbol
//! graph is built and persisted to the colocated redb corpus, unregisters
//! the in-memory handle (simulating the id becoming eligible for the
//! create/re-register door again — the in-memory "already exists" idempotent
//! branch is checked before `skip_kg` ever comes into play, so this step is
//! required to reach the code path under test), then re-POSTs the SAME
//! `id`/`root_path` through `create_index_handler` with `skip_kg: Some(true)`
//! and asserts the newly registered handle's symbol graph is empty despite
//! the persisted graph on disk.
//! Test: this module. Run with `cargo test -p trusty-search tests_2984`.

use super::*;
use crate::core::embed::Embedder;
use crate::core::registry::{IndexId, IndexRegistry};
use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use std::sync::Arc;

/// Build a `CreateIndexRequest` with every optional field defaulted except
/// `id`, `root_path`, and `skip_kg`.
///
/// Why: mirrors `tests_2336::create_req`, extended with a `skip_kg` argument
/// since this suite's whole point is exercising that field through the
/// handler.
fn create_req_with_skip_kg(
    id: &str,
    root_path: std::path::PathBuf,
    skip_kg: Option<bool>,
) -> super::router::CreateIndexRequest {
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
        skip_kg,
        skip_vector: None,
        defer_embed: None,
        extra_skip_dirs: None,
        data_file_max_bytes: None,
        allow_sensitive_path: false,
    }
}

/// Create a temp directory under `target/` (never in the hard denylist) with
/// RAII cleanup, returning its canonical path.
///
/// Why: identical helper to `tests_2336::temp_root` — duplicated rather than
/// shared because `tests_2336` is itself a `#[cfg(test)]`-only sibling
/// module with no public surface to import from.
fn temp_root(prefix: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let cwd = std::env::current_dir().expect("cwd");
    let base = cwd.join("target");
    std::fs::create_dir_all(&base).expect("create target/");
    let dir = tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in(&base)
        .expect("create tempdir");
    let canonical = dir.path().canonicalize().expect("canonicalize tempdir");
    (dir, canonical)
}

/// Build a fresh, empty registry with a mock embedder installed — enough for
/// `create_index_handler` to run without a live daemon or network.
async fn mock_state_async() -> Arc<SearchAppState> {
    let state = SearchAppState::new(IndexRegistry::new());
    let embedder: Arc<dyn Embedder> = Arc::new(crate::core::embed::MockEmbedder::new(8));
    state.install_embedder(embedder).await;
    Arc::new(state)
}

/// The actual #2984 Phase-0 follow-up regression: `create_index_handler`
/// itself must not load a persisted symbol graph when re-registering an id
/// with `skip_kg: Some(true)`.
#[tokio::test]
async fn create_index_handler_honors_skip_kg_on_reregister() {
    let state = mock_state_async().await;
    let (_dir, root) = temp_root("ts-2984-skip-kg-");
    let id = IndexId::new("skip-kg-reregister");

    // Phase 1: create the index with skip_kg=false and index real content
    // with a caller -> callee edge, so a genuinely non-empty symbol graph is
    // built and persisted to the colocated redb corpus.
    let first = super::indexes::create_index_handler(
        State(Arc::clone(&state)),
        Json(create_req_with_skip_kg(&id.0, root.clone(), Some(false))),
    )
    .await;
    assert_eq!(
        first.status(),
        StatusCode::OK,
        "first create (skip_kg=false) must succeed"
    );

    {
        let handle = state
            .registry
            .get(&id)
            .expect("index must be registered after create");
        handle
            .indexer
            .read()
            .await
            .index_files_batch(&[
                ("src/caller.rs".into(), "fn caller() { callee(); }".into()),
                ("src/callee.rs".into(), "fn callee() {}".into()),
            ])
            .await
            .expect("index batch");
        let graph = handle.indexer.read().await.snapshot_symbol_graph().await;
        assert!(
            graph.node_count() > 0,
            "sanity: normal indexing must build a non-empty symbol graph"
        );
    }
    // Stop the filesystem watcher `create_index_handler` spawned for this
    // index (`state.watcher_manager.spawn_for_index`, indexes.rs) — it holds
    // its own `Arc<IndexHandle>` clone in a detached background task, so
    // without stopping it first the corpus Arc (and the redb file lock it
    // holds) outlives `unregister` below and the re-register phase's redb
    // reopen fails with `DatabaseAlreadyOpen`.
    state.watcher_manager.stop_for_index(&id).await;
    // Drop the in-memory handle so its corpus Arc (and the redb file lock it
    // holds) is released before the re-register phase reopens the same
    // colocated redb file. Unregistering also clears the "already exists"
    // idempotent short-circuit at the top of `create_index_handler`, which
    // is checked BEFORE `skip_kg` is ever consulted — without this step the
    // re-POST below would return early and never exercise the fix.
    assert!(
        state.registry.unregister(&id),
        "index must have been registered to unregister"
    );

    // Phase 2: re-POST the SAME id/root_path through create_index_handler
    // with skip_kg=true. This is the exact door the code-critic flagged:
    // `init_entry` must carry the real skip_kg value, not the
    // `..Default::default()` false it would get if the ordering fix were
    // reverted.
    let second = super::indexes::create_index_handler(
        State(Arc::clone(&state)),
        Json(create_req_with_skip_kg(&id.0, root.clone(), Some(true))),
    )
    .await;
    assert_eq!(
        second.status(),
        StatusCode::OK,
        "re-register (skip_kg=true) must succeed"
    );

    let restored = state
        .registry
        .get(&id)
        .expect("index must be registered after re-create");
    let graph = restored.indexer.read().await.snapshot_symbol_graph().await;
    assert_eq!(
        graph.node_count(),
        0,
        "#2984: create_index_handler with skip_kg=true must not load a \
         persisted symbol graph on re-register, even when one already \
         exists on disk"
    );
    assert_eq!(
        graph.edge_count(),
        0,
        "#2984: create_index_handler with skip_kg=true must not load \
         persisted graph edges either"
    );
}
