//! Handler-level regression for #5357 — a FAILED read of either trust-gate
//! input must refuse the root override, not wave it through.
//!
//! Why: `reindex_handler`'s #2178 mirror read both of its inputs with
//! `unwrap_or(None)` / `.ok()`, and `root_move_is_trusted(None, _)` is `true`.
//! So a redb fault on the corpus's `_meta` table skipped the gate entirely, and
//! an unparseable `indexes.toml` — the exact state #4317/#4871 already found in
//! the wild — made the gate answer "no persisted entry, therefore trusted". The
//! override then re-pointed the index at an unvalidated root. Refusing costs a
//! retry; proceeding costs the corpus.
//!
//! What: two tests, one per failing input, each driving the real
//! `POST /indexes/:id/reindex` handler and asserting a `409` plus an untouched
//! pair of roots.
//!
//! Test: this module IS the test.

use std::sync::Arc;
use tokio::sync::RwLock;

use super::reindex_handlers::{reindex_handler, ReindexRequest};
use crate::core::corpus::CorpusStore;
use crate::core::embed::{Embedder, MockEmbedder};
use crate::core::indexer::CodeIndexer;
use crate::core::registry::{IndexHandle, IndexId, IndexRegistry};
use crate::core::store::{UsearchStore, VectorStore};
use crate::service::server::state::SearchAppState;
use axum::extract::{Json, Path, State};

/// Everything the two tests share: a corpus-backed index rooted at `root_old`
/// with one seeded chunk, plus the sub-root the override will target.
struct Fixture {
    _dir: tempfile::TempDir,
    root_old: std::path::PathBuf,
    root_new: std::path::PathBuf,
    corpus: Arc<CorpusStore>,
    state: Arc<SearchAppState>,
}

/// Build the fixture, stamping the corpus's `indexed_root` at `root_old` and
/// pointing the handler's persisted-root read at `registry_toml` (#2717's
/// injection seam, so no test here touches the process-wide data dir).
async fn fixture(id: &str, prefix: &str) -> Fixture {
    let (dir, root_old) = super::test_support::allowlisted_index_root(prefix);
    let root_new = root_old.join("Jira");
    std::fs::create_dir_all(root_new.join("src")).expect("create the sub-root");
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
    corpus
        .write_indexed_root_sync(&root_old)
        .expect("stamp indexed_root");
    let mut indexer =
        CodeIndexer::new(id, &root_old).with_components(Arc::clone(&embedder), Arc::clone(&store));
    indexer.set_corpus_store(Arc::clone(&corpus));
    indexer
        .index_files_batch(&[(seeded_relative.to_string(), contents.to_string())])
        .await
        .expect("seed one chunk");

    let registry = IndexRegistry::new();
    registry.register(IndexHandle::bare(
        IndexId::new(id),
        Arc::new(RwLock::new(indexer)),
        root_old.clone(),
    ));
    let registry_toml = root_old.join("indexes.toml");
    let state = Arc::new(SearchAppState::new(registry).with_registry_path(registry_toml.clone()));
    state.install_embedder(Arc::clone(&embedder)).await;

    Fixture {
        _dir: dir,
        root_old,
        root_new,
        corpus,
        state,
    }
}

/// Drive `POST /indexes/:id/reindex` with a `root_path` override and return the
/// refusal, failing the test when the handler accepted it.
async fn expect_refusal(
    fx: &Fixture,
    id: &str,
    context: &str,
) -> (axum::http::StatusCode, serde_json::Value) {
    let result = reindex_handler(
        State(Arc::clone(&fx.state)),
        Path(id.to_string()),
        Some(Json(ReindexRequest {
            root_path: Some(fx.root_new.clone()),
            force: None,
            background: None,
        })),
    )
    .await;
    let refused = result.err().map(|(status, Json(body))| (status, body));
    refused.unwrap_or_else(|| panic!("{context}"))
}

/// Assert the refusal left no half-applied override behind.
async fn assert_roots_untouched(fx: &Fixture, id: &str) {
    let handle = fx
        .state
        .registry
        .get(&IndexId::new(id.to_string()))
        .expect("still registered");
    assert_eq!(handle.root_path, fx.root_old, "handle root must not move");
    assert_eq!(
        handle.indexer.read().await.root_path,
        fx.root_old,
        "indexer root must not move either — a half-applied override is the \
         divergence this whole cluster is about"
    );
}

/// #5357: an unparseable `indexes.toml` must refuse the override.
///
/// Why: `load_index_registry_at` returns `Err` for a registry it cannot parse
/// (#4317/#4871 made that an error precisely so no caller writes on top of a
/// view it never really read). The handler dropped that `Err` with `.ok()`,
/// leaving `persisted_root: None` — the one value `root_move_is_trusted`
/// answers `true` for. The override then landed against a root nothing durable
/// had ever agreed to.
/// What: seeds a registry file that is not TOML, drives the override, and
/// asserts `409` naming `indexes.toml`.
/// Test: this test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reindex_override_is_refused_when_the_registry_cannot_be_parsed() {
    let id = "atlassian-5357-registry";
    let fx = fixture(id, "ts-5357-registry-").await;
    std::fs::write(
        fx.root_old.join("indexes.toml"),
        "this file is not valid toml = = =\n",
    )
    .expect("seed an unparseable registry");

    let (status, body) = expect_refusal(
        &fx,
        id,
        "#5357: an override whose persisted root_path cannot be read must be \
         refused — a read failure is not evidence that this index has no \
         persisted entry",
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::CONFLICT);
    assert!(
        body["error"]
            .as_str()
            .unwrap_or_default()
            .contains("indexes.toml"),
        "the refusal must name the input that failed; got {body}"
    );
    assert_roots_untouched(&fx, id).await;
}

/// #5357: an unreadable corpus `_meta` table must refuse the override.
///
/// Why: `handle.read_indexed_root().await.unwrap_or(None)` collapsed "this
/// index has no prior root" and "the prior root could not be read" into the
/// same value, and the first of those legitimately skips the gate. A redb fault
/// therefore disabled the #2178 check outright — no move detected, no registry
/// cross-check, override accepted.
/// What: breaks the corpus's `_meta` table so every read of it errors, drives
/// the same override, and asserts `409`.
/// Test: this test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reindex_override_is_refused_when_the_indexed_root_cannot_be_read() {
    let id = "atlassian-5357-corpus";
    let fx = fixture(id, "ts-5357-corpus-").await;
    crate::service::persistence::save_index_registry_at(
        &fx.root_old.join("indexes.toml"),
        &[crate::service::persistence::PersistedIndex {
            id: id.to_string(),
            root_path: fx.root_old.clone(),
            ..Default::default()
        }],
    )
    .expect("seed a valid registry — the corpus read is the input under test");
    fx.corpus
        .break_meta_table_for_tests()
        .expect("inject the _meta read fault");

    let (status, body) = expect_refusal(
        &fx,
        id,
        "#5357: an override must be refused when the corpus's last-indexed root \
         cannot be read — skipping the gate on a read failure is how an \
         untrusted root gets walked and the real corpus pruned against it",
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::CONFLICT);
    assert!(
        body["error"]
            .as_str()
            .unwrap_or_default()
            .contains("last-indexed root failed"),
        "the refusal must name the input that failed; got {body}"
    );
    assert_roots_untouched(&fx, id).await;
}
