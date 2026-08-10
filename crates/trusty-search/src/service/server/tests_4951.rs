//! Handler-level regression for #4951 — a reindex `root_path` override must
//! not silently empty every search result.
//!
//! Why: the unit coverage in `tests_search.rs` pins the two halves of the
//! mechanism (`resolve_chunk_file` against a stale root, `set_root_path`), but
//! neither reproduces the DEFECT: one exercises pure helpers that behave
//! identically before and after the fix, and the other fails at the parent
//! commit only because the symbol did not exist. A missing symbol is not a
//! reproduction. This module drives the real `POST /indexes/:id/reindex`
//! override and then the real `POST /indexes/:id/search`, so the assertion that
//! fails at the parent commit is the production symptom itself — `results: []`
//! with `stale_index_root: true` on a healthy, populated index.
//!
//! What: one test, `reindex_root_override_still_returns_search_results`.
//!
//! Test: this module IS the test.

use std::sync::Arc;
use tokio::sync::RwLock;

use super::reindex_handlers::{reindex_handler, ReindexRequest};
use super::search::search_handler;
use crate::core::embed::{Embedder, MockEmbedder};
use crate::core::indexer::{CodeIndexer, SearchMode, SearchQuery, SearchStage};
use crate::core::registry::{IndexHandle, IndexId, IndexRegistry};
use crate::core::store::{UsearchStore, VectorStore};
use crate::service::server::state::SearchAppState;
use axum::extract::{Json, Path, State};

/// A lexical query for the seeded chunk, with every optional knob at its
/// documented default so the test asserts the ordinary search path.
fn probe_query(text: &str) -> SearchQuery {
    SearchQuery {
        text: text.to_string(),
        top_k: 5,
        expand_graph: false,
        compact: false,
        branch_files: None,
        branch_boost: 1.5,
        branch: None,
        // The seeded index has no vectors; the lexical lane is what production
        // fell back to as well (`search_capabilities` was empty in the report).
        stage: Some(SearchStage::Lexical),
        mode: SearchMode::Code,
        exclude_archived: false,
        refine_query: None,
        path_prefix: None,
        repos: Vec::new(),
    }
}

/// #4951: after `POST /indexes/:id/reindex` re-points an index one level down,
/// search must still return its chunks.
///
/// Why: this is the whole defect, reproduced end to end. The override rebuilds
/// the handle around the SAME indexer `Arc`. The indexer's `root_path` is the
/// base every stored root-relative chunk path is joined against to build the
/// absolute `CodeChunk::file`; `search_handler` then post-filters those
/// absolute paths against the HANDLE's `root_path` (`file_is_within_root`,
/// added for #64/#541). Leaving the indexer on the old root put every
/// materialized path outside the new root, so 100% of candidates were dropped
/// and callers got `results: []` with `stale_index_root: true` — on an index
/// whose `/status` read `ready` with 85,642 chunks and a populated
/// `search_capabilities`. JIRA context was missing from every PR review for
/// 40+ days behind exactly this.
///
/// What: seeds a chunk stored relative to the SUBDIRECTORY the index is about
/// to be re-pointed at (the post-reindex state — the walker relativizes against
/// the current root), backs it with a real on-disk file so the override's
/// background reindex converges on the same chunk rather than pruning it, runs
/// the override, then searches. Asserts a non-empty result set, `file` resolved
/// under the NEW root, and `stale_index_root: false`.
///
/// At the parent commit this fails on the first assertion with an empty result
/// set — the daemon's exact production behaviour.
///
/// Multi-threaded flavor is required, not cosmetic: `file_is_within_root`'s
/// canonicalize fallback (#541) uses `tokio::task::block_in_place`, which
/// panics on the single-threaded test runtime. That fallback runs only for an
/// absolute path that failed the cheap prefix check — i.e. exactly on the
/// defect path — so a single-threaded test would abort before it could assert
/// the symptom.
///
/// Test: this test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reindex_root_override_still_returns_search_results() {
    let (_dir, root_old) = super::test_support::allowlisted_index_root("ts-4951-");
    // The production shape: the new root is one level BELOW the registered one
    // (`/mnt/data/knowledge` → `/mnt/data/knowledge/Jira`).
    let root_new = root_old.join("Jira");
    std::fs::create_dir_all(root_new.join("src")).expect("create the sub-root");
    let seeded_relative = "src/auth.rs";
    let contents = "fn onboarding_handler() { /* onboarding */ }";
    std::fs::write(root_new.join(seeded_relative), contents).expect("write source file");

    let dim = 16;
    let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::new(dim));
    let store: Arc<dyn VectorStore> = Arc::new(UsearchStore::new(dim).expect("usearch"));
    let indexer = CodeIndexer::new("atlassian", &root_old)
        .with_components(Arc::clone(&embedder), Arc::clone(&store));
    // Chunk paths are stored relative to the root that was current when they
    // were written — after the reindex that is the SUB-root, which is why the
    // portable `path` field stayed correct while `file` did not.
    indexer
        .index_files_batch(&[(seeded_relative.to_string(), contents.to_string())])
        .await
        .expect("seed one chunk");

    let registry = IndexRegistry::new();
    registry.register(IndexHandle::bare(
        IndexId::new("atlassian"),
        Arc::new(RwLock::new(indexer)),
        root_old.clone(),
    ));
    let state = Arc::new(SearchAppState::new(registry));
    state.install_embedder(Arc::clone(&embedder)).await;

    // Sanity: the index answers BEFORE the override. If this ever fails the
    // test is broken, not the fix — the assertions below would be vacuous.
    let Json(before) = search_handler(
        State(Arc::clone(&state)),
        Path("atlassian".to_string()),
        Json(probe_query("onboarding")),
    )
    .await
    .expect("search before the override must succeed");
    assert!(
        !before["results"]
            .as_array()
            .expect("results array")
            .is_empty(),
        "test precondition: the seeded index must return results before the \
         override, otherwise this test proves nothing. Got: {before}"
    );

    // The real override path.
    let Json(queued) = reindex_handler(
        State(Arc::clone(&state)),
        Path("atlassian".to_string()),
        Some(Json(ReindexRequest {
            root_path: Some(root_new.clone()),
            force: None,
            background: None,
        })),
    )
    .await
    .expect("override onto an unclaimed sub-root must be accepted");
    assert_eq!(
        queued["queued"],
        serde_json::Value::Bool(true),
        "the override must be accepted — the defect is silent, not a rejection"
    );

    let Json(after) = search_handler(
        State(Arc::clone(&state)),
        Path("atlassian".to_string()),
        Json(probe_query("onboarding")),
    )
    .await
    .expect("search after the override must succeed");

    let results = after["results"].as_array().expect("results array");
    assert!(
        !results.is_empty(),
        "#4951: search returned nothing after a root_path override. Every \
         candidate was found and then discarded by the `file_is_within_root` \
         post-filter because the indexer kept building absolute paths against \
         the OLD root. Response: {after}"
    );
    assert_eq!(
        after["meta"]["stale_index_root"],
        serde_json::Value::Bool(false),
        "#4951: nothing may be dropped as out-of-root — the roots are in \
         lockstep. Response: {after}"
    );

    let file = results[0]["file"].as_str().expect("file field");
    assert!(
        std::path::Path::new(file).starts_with(&root_new),
        "#4951: the resolved absolute path must sit under the NEW root {}, \
         got {file}",
        root_new.display(),
    );
    assert_eq!(
        results[0]["path"].as_str(),
        Some(seeded_relative),
        "the portable root-relative `path` was correct throughout — that is why \
         the mismatch was invisible on /status and surfaced only as empty results"
    );
}
