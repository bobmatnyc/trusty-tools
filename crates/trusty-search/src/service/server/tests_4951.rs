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

/// #4951 review HIGH-1 reproduction: when the corpus's `indexed_root` disagrees
/// with the override target, the #2178 guard aborts the walk — so syncing the
/// indexer root alone leaves chunks relative to the OLD root resolving against
/// the NEW one, and the post-filter no longer reports it.
///
/// Why: `file_is_within_root` is a lexical prefix test with no existence check.
/// Once both roots agree, `<new>/<old-relative>` passes it unconditionally, so
/// `stale_index_root` reads `false` while every `file` points at a path that
/// does not exist — or, worse, at a DIFFERENT real file that happens to sit at
/// the same relative path under the new root. That trades a loud failure for a
/// silent wrong answer, which is the fail-open shape this cluster exists to
/// remove.
/// What: seeds a corpus-backed index whose `indexed_root` is `root_old` with a
/// chunk relative to `root_old`, drives the override to `root_new`, and asserts
/// the handler REFUSES rather than producing unresolvable results. The
/// existence assertion is the point — a lexical check is what let this through.
/// Test: this test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reindex_root_override_is_refused_when_the_corpus_disagrees() {
    use crate::core::corpus::CorpusStore;

    let (_dir, root_old) = super::test_support::allowlisted_index_root("ts-4951-guard-");
    let root_new = root_old.join("Jira");
    std::fs::create_dir_all(root_new.join("src")).expect("create the sub-root");
    // The real file lives under the OLD root — the corpus was built there.
    std::fs::create_dir_all(root_old.join("src")).expect("create old src");
    let seeded_relative = "src/auth.rs";
    let contents = "fn onboarding_handler() { /* onboarding */ }";
    std::fs::write(root_old.join(seeded_relative), contents).expect("write source file");

    let dim = 16;
    let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::new(dim));
    let store: Arc<dyn VectorStore> = Arc::new(UsearchStore::new(dim).expect("usearch"));
    let corpus = Arc::new(
        CorpusStore::open(&root_old.join(".trusty-search").join("index.redb")).expect("corpus"),
    );
    // The corpus records the root its chunk paths are relative to.
    corpus
        .write_indexed_root_sync(&root_old)
        .expect("stamp indexed_root");
    let mut indexer = CodeIndexer::new("atlassian-guard", &root_old)
        .with_components(Arc::clone(&embedder), Arc::clone(&store));
    indexer.set_corpus_store(Arc::clone(&corpus));
    indexer
        .index_files_batch(&[(seeded_relative.to_string(), contents.to_string())])
        .await
        .expect("seed one chunk");

    let registry = IndexRegistry::new();
    registry.register(IndexHandle::bare(
        IndexId::new("atlassian-guard"),
        Arc::new(RwLock::new(indexer)),
        root_old.clone(),
    ));
    // Point the handler's persisted-root read at an explicit file so this test
    // never touches the process-wide data dir (#2717's injection seam). The
    // registry names this index at its ORIGINAL root — the durable disagreement
    // the #2178 guard keys on.
    let registry_toml = root_old.join("indexes.toml");
    crate::service::persistence::save_index_registry_at(
        &registry_toml,
        &[crate::service::persistence::PersistedIndex {
            id: "atlassian-guard".to_string(),
            root_path: root_old.clone(),
            ..Default::default()
        }],
    )
    .expect("seed registry");
    let state = Arc::new(SearchAppState::new(registry).with_registry_path(registry_toml.clone()));
    state.install_embedder(Arc::clone(&embedder)).await;

    let result = reindex_handler(
        State(Arc::clone(&state)),
        Path("atlassian-guard".to_string()),
        Some(Json(ReindexRequest {
            root_path: Some(root_new.clone()),
            force: None,
            background: None,
        })),
    )
    .await;

    let err = result.err().map(|(status, Json(body))| (status, body));
    let (status, body) = err.expect(
        "#4951 HIGH-1: an override whose target disagrees with the corpus's \
         indexed_root must be refused — accepting it re-points the indexer at a \
         root the corpus was never relativized against, and the #2178 guard \
         then aborts the walk that would have fixed it",
    );
    assert_eq!(status, axum::http::StatusCode::CONFLICT);
    assert!(
        body["error"]
            .as_str()
            .unwrap_or_default()
            .contains("relocate"),
        "the refusal must name the durable alternative; got {body}"
    );

    // The refusal must leave BOTH roots untouched — no half-applied override.
    let handle = state
        .registry
        .get(&IndexId::new("atlassian-guard".to_string()))
        .expect("still registered");
    assert_eq!(handle.root_path, root_old, "handle root must not move");
    assert_eq!(
        handle.indexer.read().await.root_path,
        root_old,
        "indexer root must not move either — a half-applied override is the \
         divergence this whole issue is about"
    );

    // And search still resolves to a file that EXISTS. A lexical containment
    // check cannot tell a real hit from a dangling one, so assert the disk.
    let Json(after) = search_handler(
        State(Arc::clone(&state)),
        Path("atlassian-guard".to_string()),
        Json(probe_query("onboarding")),
    )
    .await
    .expect("search must still work");
    let results = after["results"].as_array().expect("results array");
    assert!(!results.is_empty(), "index must still serve: {after}");
    let file = results[0]["file"].as_str().expect("file field");
    assert!(
        std::path::Path::new(file).exists(),
        "#4951 HIGH-1: the resolved path must exist on disk, got {file} \
         (response: {after})"
    );
}
