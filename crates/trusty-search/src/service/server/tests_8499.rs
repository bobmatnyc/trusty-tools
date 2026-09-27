//! Regression tests for #8499: indexing a repo never modifies a tracked file,
//! and the index survives `git reset --hard` plus `git clean -fdx`.
//!
//! Why: `create_index` appended `.trusty-search/` to the indexed repo's own
//! tracked `.gitignore` and left the edit uncommitted. A repo sync that ran
//! `git reset --hard` reverted it, the following `git clean -fd` deleted the
//! colocated index directory, and a daemon with those files mmapped died with
//! SIGBUS (duettoresearch/code-intelligence#5217).
//! What: every test drives the real `create_index_handler` and a real reindex
//! against a committed temp git repo, with `TRUSTY_DATA_DIR` pinned to a
//! private tempdir (`IsolatedDataDir`) and the root on the test allowlist.
//! Test: this module. Run with `cargo test -p trusty-search tests_8499`.

use super::tests_components::IsolatedDataDir;
use super::*;
use crate::core::embed::Embedder;
use crate::core::indexer::{SearchQuery, SearchStage};
use crate::core::registry::{IndexHandle, IndexId, IndexRegistry};
use crate::service::colocated_storage::COLOCATED_DIR_NAME;
use crate::service::reindex::{spawn_reindex_awaitable, ReindexProgress, ReindexStatus};
use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Identifier only the fixture source file defines, so a hit proves the index.
const MARKER: &str = "onboarding_handler_8499";

pub(super) fn create_req(id: &str, root_path: PathBuf) -> super::router::CreateIndexRequest {
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

pub(super) async fn mock_state() -> Arc<SearchAppState> {
    let state = SearchAppState::new(IndexRegistry::new());
    let embedder: Arc<dyn Embedder> = Arc::new(crate::core::embed::MockEmbedder::new(8));
    state.install_embedder(embedder).await;
    Arc::new(state)
}

/// Run `git` in `root` with a fixed identity; panics with stderr on failure.
pub(super) fn git(root: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args([
            "-c",
            "user.email=t@example.com",
            "-c",
            "user.name=t",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
        ])
        .args(args)
        .output()
        .expect("spawn git");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// A committed git repo holding one source file and, optionally, a tracked
/// root `.gitignore` with `gitignore` as its exact bytes.
pub(super) fn clean_repo(prefix: &str, gitignore: Option<&str>) -> (tempfile::TempDir, PathBuf) {
    let (dir, root) = super::test_support::allowlisted_index_root(prefix);
    std::fs::create_dir_all(root.join("src")).expect("src dir");
    std::fs::write(
        root.join("src/lib.rs"),
        format!("pub fn {MARKER}() -> u32 {{ 8499 }}\n"),
    )
    .expect("write source");
    if let Some(content) = gitignore {
        std::fs::write(root.join(".gitignore"), content).expect("write .gitignore");
    }
    git(&root, &["init", "-q"]);
    git(&root, &["add", "-A"]);
    git(&root, &["commit", "-q", "-m", "init"]);
    assert_eq!(
        git(&root, &["status", "--porcelain"]),
        "",
        "fixture is clean"
    );
    (dir, root)
}

/// Register `id` at `root` through the real handler, then run a full reindex
/// to completion. Returns the live handle.
///
/// #8499 round 3: embeds inline (`defer_embed: false`). A deferred embed job
/// holds the handle, and with it the redb file, while it waits for the one
/// process-wide background permit — under full-suite load another test holds
/// that permit, so a re-registration lost the open and flaked with `500`.
async fn register_and_index(
    state: &Arc<SearchAppState>,
    id: &str,
    root: &Path,
) -> Arc<IndexHandle> {
    let mut req = create_req(id, root.to_path_buf());
    req.defer_embed = Some(false);
    let resp = super::indexes::create_index_handler(State(Arc::clone(state)), Json(req)).await;
    let status = resp.status();
    let body = super::tests_components::body_json(resp).await;
    assert_eq!(status, StatusCode::OK, "create must succeed: {body}");
    let handle = state
        .registry
        .get(&IndexId::new(id))
        .expect("registered after create");
    let progress = Arc::new(ReindexProgress::new());
    spawn_reindex_awaitable(Arc::clone(&handle), Arc::clone(&progress), false)
        .await
        .expect("reindex task must not panic");
    assert_eq!(progress.status.load(), ReindexStatus::Complete, "reindex");
    handle
}

/// Lexical hits for [`MARKER`] on `handle`.
async fn marker_hits(handle: &IndexHandle) -> usize {
    handle
        .indexer
        .read()
        .await
        .search(&SearchQuery {
            text: MARKER.to_string(),
            stage: Some(SearchStage::Lexical),
            ..Default::default()
        })
        .await
        .expect("search must succeed")
        .len()
}

/// Stop the watcher and drop the live handle, releasing the redb file lock —
/// the in-process stand-in for a daemon restart.
pub(super) async fn unregister(state: &Arc<SearchAppState>, id: &str) {
    let id = IndexId::new(id);
    state.watcher_manager.stop_for_index(&id).await;
    assert!(state.registry.unregister(&id), "was registered");
}

/// Criterion 1 (#8499): after `git reset --hard && git clean -fdx` the index
/// and the daemon both survive, and a restart restores a queryable index.
///
/// Why: `-fdx` removes ignored files too, so no ignore rule protects an index
/// inside the work tree; the on-disk index must not live there. Pre-fix the
/// index was colocated in `<root>/.trusty-search/`, which `clean -fdx` deleted,
/// so the re-registration restored an empty corpus.
/// Test: this test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial]
async fn index_survives_git_reset_hard_and_clean_fdx() {
    let _data = IsolatedDataDir::new();
    let state = mock_state().await;
    let (_dir, root) = clean_repo("c1-8499-", Some("target/\n"));
    const ID: &str = "survive-clean-8499";

    let handle = register_and_index(&state, ID, &root).await;
    assert!(marker_hits(&handle).await > 0, "sanity: indexed");

    git(&root, &["reset", "-q", "--hard"]);
    git(&root, &["clean", "-q", "-fdx"]);

    assert!(
        marker_hits(&handle).await > 0,
        "#8499: the live index must stay queryable after git clean -fdx"
    );
    let earlier = Arc::downgrade(&handle);
    drop(handle);
    unregister(&state, ID).await;
    assert!(
        earlier.upgrade().is_none(),
        "#8499: nothing may still hold the store the re-registration opens"
    );

    let restarted = register_and_index_no_reindex(&state, ID, &root).await;
    assert!(
        marker_hits(&restarted).await > 0,
        "#8499: git clean -fdx deleted the on-disk index; a restart restored nothing"
    );
    state
        .watcher_manager
        .stop_for_index(&IndexId::new(ID))
        .await;
}

/// Re-register without reindexing, so only what survived on disk is served.
async fn register_and_index_no_reindex(
    state: &Arc<SearchAppState>,
    id: &str,
    root: &Path,
) -> Arc<IndexHandle> {
    let resp = super::indexes::create_index_handler(
        State(Arc::clone(state)),
        Json(create_req(id, root.to_path_buf())),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK, "re-register must succeed");
    state
        .registry
        .get(&IndexId::new(id))
        .expect("registered after re-create")
}

/// Criterion 2 (#8499): registering and indexing a clean repo modifies no
/// tracked file and creates no untracked one — `git status --porcelain` stays
/// empty, with or without a root `.gitignore`.
///
/// Why: pre-fix `create_index` appended `.trusty-search/` to the tracked
/// `.gitignore` (` M .gitignore`), or created an untracked one (`?? .gitignore`).
/// Test: this test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial]
async fn indexing_a_clean_repo_leaves_git_status_empty() {
    let _data = IsolatedDataDir::new();
    let state = mock_state().await;
    for (n, gitignore) in [Some("target/\n"), None].into_iter().enumerate() {
        let (_dir, root) = clean_repo("c2-8499-", gitignore);
        let id = format!("status-clean-8499-{n}");
        let handle = register_and_index(&state, &id, &root).await;
        assert!(marker_hits(&handle).await > 0, "sanity: indexed");
        assert_eq!(
            git(&root, &["status", "--porcelain"]),
            "",
            "#8499: indexing must not modify or add any file git reports \
             (root .gitignore fixture: {gitignore:?})"
        );
        drop(handle);
        unregister(&state, &id).await;
    }
}

/// Criterion 3 (#8499): a `.trusty-search` entry the user already wrote in
/// the tracked `.gitignore` is left byte for byte — not duplicated, reordered
/// or removed.
///
/// Why: pre-fix the coverage check matched only the two literal spellings, so
/// an anchored `/.trusty-search/` got a second entry appended.
/// Test: this test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial]
async fn user_gitignore_entry_is_left_byte_for_byte() {
    let _data = IsolatedDataDir::new();
    let state = mock_state().await;
    let fixtures = [
        ".trusty-search/\n",
        "# local\n/.trusty-search/\ntarget/\n*.log",
    ];
    for (n, content) in fixtures.into_iter().enumerate() {
        let (_dir, root) = clean_repo("c3-8499-", Some(content));
        let id = format!("user-entry-8499-{n}");
        let handle = register_and_index(&state, &id, &root).await;
        assert_eq!(
            std::fs::read_to_string(root.join(".gitignore")).expect("read .gitignore"),
            content,
            "#8499: the user's .gitignore must be left exactly as written"
        );
        drop(handle);
        unregister(&state, &id).await;
    }
}

/// #8499 legacy layout: a repo that already holds a colocated corpus keeps it
/// (it is adopted, not abandoned), but the directory is hidden from git by an
/// untracked `.trusty-search/.gitignore` — never by the root `.gitignore` —
/// so `git status` stays empty and `git clean -fd` leaves it in place.
/// Test: this test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial]
async fn adopted_colocated_corpus_is_invisible_to_git_status_and_clean_fd() {
    let _data = IsolatedDataDir::new();
    let state = mock_state().await;
    let (_dir, root) = clean_repo("legacy-8499-", Some("target/\n"));
    const ID: &str = "legacy-coloc-8499";
    // A legacy colocated artifact: the files a pre-#8499 registration wrote.
    let colocated = root.join(COLOCATED_DIR_NAME);
    std::fs::create_dir(&colocated).expect("create .trusty-search");
    drop(crate::core::corpus::CorpusStore::open(&colocated.join("index.redb")).expect("corpus"));

    let handle = register_and_index(&state, ID, &root).await;
    assert!(
        colocated.join("index.redb").is_file(),
        "a repo that already holds a colocated artifact keeps that layout"
    );
    assert_eq!(
        std::fs::read_to_string(root.join(".gitignore")).expect("read"),
        "target/\n",
        "#8499: the root .gitignore is never the exclusion mechanism"
    );
    assert_eq!(git(&root, &["status", "--porcelain"]), "", "#8499");
    git(&root, &["reset", "-q", "--hard"]);
    git(&root, &["clean", "-q", "-fd"]);
    assert!(
        colocated.join("index.redb").is_file(),
        "#8499: git clean -fd must not delete the colocated index"
    );
    drop(handle);
    unregister(&state, ID).await;

    // The documented way out of the work tree: delete with data, re-register.
    crate::service::storage_layout::StorageLayout::Colocated
        .remove_storage(ID, Some(&root))
        .expect("delete_data");
    assert!(
        !colocated.exists(),
        "delete_data removes the self-ignore too"
    );
    let moved = register_and_index(&state, ID, &root).await;
    assert!(
        !colocated.exists(),
        "#8499: re-registration uses the data dir"
    );
    assert_eq!(git(&root, &["status", "--porcelain"]), "", "#8499");
    drop(moved);
    unregister(&state, ID).await;
}

/// #8499 fail closed: when the index store cannot be placed outside the work
/// tree, registration is refused with a clear error and nothing is written
/// into the repository.
///
/// Why: `TRUSTY_DATA_DIR` pointed into `<root>/.trusty-search` would place the
/// store inside the work tree; the registration must refuse, not fall back to
/// writing there or to editing `.gitignore`.
/// Test: this test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial]
async fn create_refuses_when_the_store_would_land_in_the_work_tree() {
    let _data = IsolatedDataDir::new();
    let state = mock_state().await;
    let (_dir, root) = clean_repo("failclosed-8499-", Some("target/\n"));
    let in_repo = root.join(COLOCATED_DIR_NAME).join("data");
    // SAFETY: #[serial]; `IsolatedDataDir`'s drop clears the variable.
    unsafe { std::env::set_var("TRUSTY_DATA_DIR", &in_repo) };

    let resp = super::indexes::create_index_handler(
        State(Arc::clone(&state)),
        Json(create_req("failclosed-8499", root.clone())),
    )
    .await;
    let status = resp.status();
    let body = super::tests_components::body_json(resp).await;

    assert_eq!(status, StatusCode::CONFLICT, "#8499: must refuse: {body}");
    assert!(
        body["error"].as_str().unwrap_or_default().contains("#8499"),
        "the refusal must name the reason: {body}"
    );
    assert!(
        state
            .registry
            .get(&IndexId::new("failclosed-8499"))
            .is_none(),
        "a refused registration registers nothing"
    );
    assert_eq!(
        std::fs::read_to_string(root.join(".gitignore")).expect("read"),
        "target/\n",
        "#8499: a refusal must not edit the tracked .gitignore"
    );
}
