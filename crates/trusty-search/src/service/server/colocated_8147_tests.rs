//! Regression tests for #8147: `POST /indexes` honours `colocated: false`.
//!
//! Why: since #8499 a new index lives in the data dir, but registration still
//! adopts any `<root>/.trusty-search/` holding a corpus, and the request had no
//! field to opt out. On a root-owned root that adoption answered `500 corpus
//! open failed …; refusing to register a broken index handle`, so an index
//! deliberately kept in the data-dir store could not be registered.
//! What: drives the real `create_index_handler` against roots holding a
//! read-only or writable colocated corpus, a read-only root with none, a
//! colocated registration (resident and row-only), and an unreadable registry.
//! Unix-only (permission bits); the read-only cases skip under euid 0, where
//! permission bits do not refuse.
//! Test: this module. Run with `cargo test -p trusty-search colocated_8147`.

use super::tests_components::IsolatedDataDir;
use super::*;
use crate::core::embed::Embedder;
use crate::core::registry::{IndexId, IndexRegistry};
use crate::service::colocated_storage::COLOCATED_DIR_NAME;
use crate::service::persistence::{find_index_registry_entry, PersistedIndex};
use crate::service::storage_layout::{layout_of, StorageLayout};
use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// A `CreateIndexRequest` with every optional field defaulted but `colocated`.
fn create_req(
    id: &str,
    root_path: PathBuf,
    colocated: Option<bool>,
) -> super::router::CreateIndexRequest {
    super::router::CreateIndexRequest {
        roots: None,
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
        colocated,
        extra_skip_dirs: None,
        data_file_max_bytes: None,
        allow_sensitive_path: false,
    }
}

async fn mock_state() -> Arc<SearchAppState> {
    let state = SearchAppState::new(IndexRegistry::new());
    let embedder: Arc<dyn Embedder> = Arc::new(crate::core::embed::MockEmbedder::new(8));
    state.install_embedder(embedder).await;
    Arc::new(state)
}

/// POST a create through the real handler; returns status and JSON body.
async fn create(
    state: &Arc<SearchAppState>,
    id: &str,
    root: &Path,
    colocated: Option<bool>,
) -> (StatusCode, serde_json::Value) {
    let resp = super::indexes::create_index_handler(
        State(Arc::clone(state)),
        Json(create_req(id, root.to_path_buf(), colocated)),
    )
    .await;
    let status = resp.status();
    (status, super::tests_components::body_json(resp).await)
}

/// Seed `<root>/.trusty-search/index.redb`: the corpus a pre-#8499
/// registration or an off-box delivery (#8135) leaves behind.
fn seed_colocated_corpus(root: &Path) -> PathBuf {
    let dir = root.join(COLOCATED_DIR_NAME);
    std::fs::create_dir(&dir).expect("create .trusty-search");
    drop(crate::core::corpus::CorpusStore::open(&dir.join("index.redb")).expect("corpus"));
    dir
}

/// Sorted entry names of `dir`.
fn listing(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .expect("readable")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// Makes paths read-only and restores `0o755` on drop, so a failed assertion
/// never leaves `TempDir::drop` a tree it cannot remove. Declare it after the
/// `TempDir` so it drops first.
struct ReadOnly(Vec<PathBuf>);

impl ReadOnly {
    fn new(paths: &[&Path]) -> Self {
        for path in paths {
            let mode = if path.is_dir() { 0o555 } else { 0o444 };
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
                .expect("make read-only");
        }
        Self(paths.iter().map(|p| p.to_path_buf()).collect())
    }
}

impl Drop for ReadOnly {
    fn drop(&mut self) {
        for path in self.0.iter().rev() {
            if let Err(e) = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)) {
                eprintln!("could not restore {}: {e}", path.display());
            }
        }
    }
}

/// Permission bits do not refuse euid 0, so the read-only cases skip there.
fn running_as_root() -> bool {
    let root = nix::unistd::geteuid().is_root();
    if root {
        eprintln!("skipping: permission bits do not refuse euid 0");
    }
    root
}

/// The data-dir store directory of `id`.
fn data_dir_store(id: &str) -> PathBuf {
    crate::service::persistence::data_dir()
        .expect("data dir")
        .join("indexes")
        .join(crate::service::persistence::sanitize_id_for_path(id))
}

/// #8147: `colocated: false` over a root that holds a colocated corpus
/// registers in the data dir and leaves that corpus untouched — read-only
/// (the reported root-owned case) or writable.
///
/// Why: registration adopted the in-repo corpus whatever the request said. On
/// a read-only one that is the reported `500`; on a writable one the opt-out
/// was silently ignored.
/// What: for each variant, seeds `.trusty-search/index.redb`, POSTs
/// `colocated: false`, and asserts `200`, a data-dir layout in `indexes.toml`
/// and on the live handle, the corpus in the data dir, and the in-repo
/// directory unchanged.
/// Test: this test. On origin/main the read-only variant answers `500` and the
/// writable one records `colocated = true`.
#[tokio::test]
#[serial_test::serial]
async fn colocated_false_over_a_read_only_colocated_corpus_registers_in_the_data_dir() {
    if running_as_root() {
        return;
    }
    let _data = IsolatedDataDir::new();
    let state = mock_state().await;
    for read_only in [true, false] {
        let id = format!("ts-8147-optout-{read_only}");
        let index = IndexId::new(&id);
        let (_dir, root) = super::test_support::allowlisted_index_root(&format!("{id}-"));
        let repo_dir = seed_colocated_corpus(&root);
        let before = listing(&repo_dir);
        let corpus = repo_dir.join("index.redb");
        let paths = [corpus.as_path(), repo_dir.as_path(), root.as_path()];
        let _guard = read_only.then(|| ReadOnly::new(&paths));

        let (status, body) = create(&state, &id, &root, Some(false)).await;
        assert_eq!(status, StatusCode::OK, "#8147 ro={read_only}: {body}");
        assert_eq!(body["created"], true, "{body}");
        let entry = find_index_registry_entry(&id)
            .expect("registry readable")
            .expect("registration persisted");
        assert!(!entry.colocated, "#8147: colocated=false recorded");
        let handle = state.registry.get(&index).expect("registered");
        assert_eq!(layout_of(&handle).await, StorageLayout::DataDir);
        assert!(
            data_dir_store(&id).join("index.redb").is_file(),
            "#8147: the corpus lives in the data dir"
        );
        assert_eq!(
            listing(&repo_dir),
            before,
            "#8147 read_only={read_only}: nothing written to {}",
            repo_dir.display()
        );
        state.watcher_manager.stop_for_index(&index).await;
    }
}

/// #8147: an omitted `colocated` over a read-only colocated corpus is a `403`
/// naming the permission problem and the opt-out, and registers nothing.
///
/// Why: the default path still adopts the in-repo corpus (#8499); when the
/// daemon cannot write it the caller got a generic `500 corpus open failed`.
/// What: read-only corpus, directory and root; asserts `403`, an `error`
/// starting `permission denied` that names `colocated=false`, no handle, no
/// `indexes.toml` row, no data-dir store and the in-repo directory unchanged.
/// Test: this test. On origin/main it answers `500`.
#[tokio::test]
#[serial_test::serial]
async fn omitted_colocated_over_a_read_only_colocated_corpus_is_a_403() {
    if running_as_root() {
        return;
    }
    let _data = IsolatedDataDir::new();
    let state = mock_state().await;
    let id = "ts-8147-ro-default";
    let (_dir, root) = super::test_support::allowlisted_index_root("ts-8147-ro-default-");
    let repo_dir = seed_colocated_corpus(&root);
    let before = listing(&repo_dir);
    let corpus = repo_dir.join("index.redb");
    let _guard = ReadOnly::new(&[corpus.as_path(), repo_dir.as_path(), root.as_path()]);

    let (status, body) = create(&state, id, &root, None).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "#8147: {body}");
    let error = body["error"].as_str().unwrap_or_default();
    assert!(
        error.starts_with("permission denied") && error.contains("colocated=false"),
        "#8147: the error names the problem and the way out. Body: {body}"
    );
    assert!(state.registry.get(&IndexId::new(id)).is_none(), "no handle");
    assert!(
        find_index_registry_entry(id)
            .expect("registry readable")
            .is_none(),
        "no indexes.toml row"
    );
    assert!(!data_dir_store(id).exists(), "no data-dir store");
    assert_eq!(listing(&repo_dir), before, "repo dir untouched");
}

/// #8147: on a read-only root with no colocated corpus, every value of
/// `colocated` registers in the data dir and creates nothing under the root.
///
/// Why: the reported request was `colocated: false` against a root-owned root.
/// Omitted and `true` are the #8499 default, which must not change.
/// What: one fresh read-only root per value; asserts `200`, `colocated =
/// false` in `indexes.toml`, and an empty root.
/// Test: this test. #8499 already placed this case in the data dir, so it
/// passes on origin/main too; it pins the default and the opt-out together.
#[tokio::test]
#[serial_test::serial]
async fn colocated_values_on_a_read_only_root_create_nothing_under_it() {
    if running_as_root() {
        return;
    }
    let _data = IsolatedDataDir::new();
    let state = mock_state().await;
    for (name, colocated) in [("none", None), ("true", Some(true)), ("false", Some(false))] {
        let id = format!("ts-8147-roroot-{name}");
        let index = IndexId::new(&id);
        let (_dir, root) = super::test_support::allowlisted_index_root(&format!("{id}-"));
        let _guard = ReadOnly::new(&[root.as_path()]);

        let (status, body) = create(&state, &id, &root, colocated).await;
        assert_eq!(status, StatusCode::OK, "#8147 colocated={name}: {body}");
        let entry = find_index_registry_entry(&id)
            .expect("registry readable")
            .expect("registration persisted");
        assert!(!entry.colocated, "colocated={name}: data-dir layout");
        assert!(
            listing(&root).is_empty(),
            "#8147 colocated={name}: nothing under the root, found {:?}",
            listing(&root)
        );
        state.watcher_manager.stop_for_index(&index).await;
    }
}

/// #8147: `colocated: false` against an id registered colocated at the same
/// root is a `409`; an omitted field still answers `200 created:false`.
///
/// Why: a registration never changes an existing index's layout, and a
/// `200` would tell the caller its opt-out took effect.
/// What: a resident index that adopted a writable colocated corpus; then a
/// row-only colocated `indexes.toml` entry for a second id (no handle, no cold
/// entry — the state a lazy load leaves mid-request). Both refuse `false` with
/// `registered_colocated: true`, register nothing new, and create no data-dir
/// store.
/// Test: this test. On origin/main the field is ignored and both answer `200`.
#[tokio::test]
#[serial_test::serial]
async fn colocated_false_against_a_colocated_registration_is_a_409() {
    let _data = IsolatedDataDir::new();
    let state = mock_state().await;
    let live = "ts-8147-live";
    let live_index = IndexId::new(live);
    let (_live_dir, live_root) = super::test_support::allowlisted_index_root("ts-8147-live-");
    seed_colocated_corpus(&live_root);
    let (status, body) = create(&state, live, &live_root, None).await;
    assert_eq!(status, StatusCode::OK, "precondition: adopt. {body}");

    let (status, body) = create(&state, live, &live_root, Some(false)).await;
    assert_eq!(status, StatusCode::CONFLICT, "#8147 resident: {body}");
    assert_eq!(body["registered_colocated"], true, "{body}");
    assert_eq!(body["requested_colocated"], false, "{body}");
    let (status, body) = create(&state, live, &live_root, None).await;
    assert_eq!(status, StatusCode::OK, "omitted still joins: {body}");
    assert_eq!(body["created"], false, "{body}");
    state.watcher_manager.stop_for_index(&live_index).await;

    let row = "ts-8147-row";
    let (_row_dir, row_root) = super::test_support::allowlisted_index_root("ts-8147-row-");
    crate::service::persistence::upsert_index_registry_entry(PersistedIndex {
        id: row.to_string(),
        root_path: row_root.clone(),
        colocated: true,
        ..Default::default()
    })
    .expect("plant indexes.toml row");
    let (status, body) = create(&state, row, &row_root, Some(false)).await;
    assert_eq!(status, StatusCode::CONFLICT, "#8147 row-only: {body}");
    assert!(
        state.registry.get(&IndexId::new(row)).is_none(),
        "409 registers nothing"
    );
    assert!(!data_dir_store(row).exists(), "no data-dir store");
}

/// #8147: the layout `409` is answered before the warming embedder's `503`.
///
/// Why: a caller retries a `503`, and would retry a request that can never
/// succeed without ever seeing the conflict.
/// What: no embedder installed, a planted colocated row, `colocated: false`
/// at the same root → `409`.
/// Test: this test. With the check after `current_embedder()` it answers `503`.
#[tokio::test]
#[serial_test::serial]
async fn colocated_false_conflict_is_a_409_while_the_embedder_warms() {
    let _data = IsolatedDataDir::new();
    let state = Arc::new(SearchAppState::new(IndexRegistry::new()));
    assert!(state.current_embedder().await.is_none(), "precondition");
    let id = "ts-8147-warm";
    let (_dir, root) = super::test_support::allowlisted_index_root("ts-8147-warm-");
    crate::service::persistence::upsert_index_registry_entry(PersistedIndex {
        id: id.to_string(),
        root_path: root.clone(),
        colocated: true,
        ..Default::default()
    })
    .expect("plant indexes.toml row");

    let (status, body) = create(&state, id, &root, Some(false)).await;
    assert_eq!(status, StatusCode::CONFLICT, "#8147: {body}");
}

/// #8147: `colocated: false` with an unreadable `indexes.toml` is a `500`
/// naming the registry, never a layout guessed from the request.
///
/// Why: an unreadable registry cannot say whether the id records a colocated
/// layout, so registering could orphan its corpus.
/// What: arms the per-id read fault (a planted bad file would break concurrent
/// tests sharing `TRUSTY_DATA_DIR`); asserts `500`, no handle, no data-dir
/// store.
/// Test: this test. Treating the read error as "no row" answers `200`.
#[tokio::test]
#[serial_test::serial]
async fn colocated_false_with_an_unreadable_registry_is_a_500() {
    let _data = IsolatedDataDir::new();
    let state = mock_state().await;
    let id = "ts-8147-noread";
    let (_dir, root) = super::test_support::allowlisted_index_root("ts-8147-noread-");
    let _fault = super::create_layout::registry_fault::ReadFault::arm(id);

    let (status, body) = create(&state, id, &root, Some(false)).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "#8147: {body}");
    assert!(
        body["error"]
            .as_str()
            .unwrap_or_default()
            .contains("indexes.toml"),
        "the error names the registry. Body: {body}"
    );
    assert!(
        state.registry.get(&IndexId::new(id)).is_none(),
        "nothing registered"
    );
    assert!(!data_dir_store(id).exists(), "no data-dir store");
}
