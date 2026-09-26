//! Regression tests for #8147: `POST /indexes` must honour `colocated: false`.
//!
//! Why: registration hardcoded `colocated: true`, so it opened — and created —
//! `<root>/.trusty-search/` on every call. On a read-only or root-owned root
//! that corpus open fails and the handler answers `500 corpus open failed for
//! root_path …; refusing to register a broken index handle`, which is what
//! made an index deliberately built into the data-dir store deletable but
//! un-re-registerable without a daemon restart. The persistence layer has
//! always routed every path on `PersistedIndex::colocated`; only this door
//! ignored it, and the request struct had no field to ignore.
//! What: drives the real `create_index_handler` with `colocated: Some(false)`
//! and asserts NOTHING was created under the root while the data-dir corpus
//! was, plus the read-only-root and read-only-`.trusty-search/` refusal and
//! retry, the fatal registry write for a data-dir index, and the omitted
//! field's unchanged default. Unix-only (permission bits); the read-only cases
//! skip under euid 0, where permission bits do not refuse.
//! Test: this module. Run with `cargo test -p trusty-search tests_8147`.

use super::*;
use crate::core::embed::Embedder;
use crate::core::registry::{IndexId, IndexRegistry};
use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use std::sync::Arc;

/// A `CreateIndexRequest` with every optional field defaulted except
/// `colocated` — the single input this suite varies.
fn create_req_with_colocated(
    id: &str,
    root_path: std::path::PathBuf,
    colocated: Option<bool>,
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
        skip_kg: None,
        skip_vector: None,
        defer_embed: None,
        colocated,
        extra_skip_dirs: None,
        data_file_max_bytes: None,
        allow_sensitive_path: false,
    }
}

/// Whether `roots.toml` lists `root`, compared canonically.
///
/// Why: `TRUSTY_DATA_DIR` is process-global, so a concurrent non-serial test
/// can add its own rows to this test's file; only this root's row is ours.
fn roots_toml_lists(root: &std::path::Path) -> bool {
    let want = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    crate::service::roots_registry::load_roots()
        .expect("roots.toml readable")
        .iter()
        .any(|r| std::fs::canonicalize(&r.path).unwrap_or_else(|_| r.path.clone()) == want)
}

/// Fresh registry with a mock embedder — enough for `create_index_handler`.
async fn mock_state() -> Arc<SearchAppState> {
    let state = SearchAppState::new(IndexRegistry::new());
    let embedder: Arc<dyn Embedder> = Arc::new(crate::core::embed::MockEmbedder::new(8));
    state.install_embedder(embedder).await;
    Arc::new(state)
}

/// #8147: `colocated: false` puts the corpus in the data-dir store and creates
/// nothing under `root_path`.
///
/// Why: this is the reported defect. With the flag ignored, registration
/// created `<root>/.trusty-search/` unconditionally — impossible on a
/// root-owned root, which is why the delivery attempt got a 500.
/// What: registers with `colocated: Some(false)` into an isolated
/// `TRUSTY_DATA_DIR`, then asserts the root is untouched, the data-dir corpus
/// exists, and `indexes.toml` recorded the layout so warm boot agrees.
/// Test: this test.
///
/// `#[serial]` because it sets `TRUSTY_DATA_DIR` process-wide.
#[tokio::test]
#[serial_test::serial]
async fn create_index_honours_colocated_false() {
    let _data_dir = super::tests_components::IsolatedDataDir::new();
    let state = mock_state().await;
    let (_dir, root) = super::test_support::allowlisted_index_root("ts-8147-nocolo-");
    let id = IndexId::new("ts-8147-nocolo");

    let created = super::indexes::create_index_handler(
        State(Arc::clone(&state)),
        Json(create_req_with_colocated(&id.0, root.clone(), Some(false))),
    )
    .await;
    assert_eq!(
        created.status(),
        StatusCode::OK,
        "#8147: colocated=false must register, not 500"
    );

    assert!(
        !root.join(".trusty-search").exists(),
        "#8147: colocated=false must create NOTHING under the root — found {}",
        root.join(".trusty-search").display(),
    );

    let entry = crate::service::persistence::find_index_registry_entry(&id.0)
        .expect("registry readable")
        .expect("the registration must be persisted");
    assert!(
        !entry.colocated,
        "#8147: indexes.toml must record colocated=false so warm boot resolves \
         the same corpus path"
    );

    let data_dir_corpus =
        crate::service::persistence::corpus_redb_path(&id.0).expect("data-dir corpus path");
    assert!(
        data_dir_corpus.exists(),
        "#8147: the corpus must have been opened in the data-dir store at {}",
        data_dir_corpus.display(),
    );
    assert!(
        !roots_toml_lists(&root),
        "#8147: roots.toml lists colocated roots for the scanner; a data-dir index \
         must add none"
    );

    state.watcher_manager.stop_for_index(&id).await;
}

/// Restores a read-only directory's write bit on drop, so a failed assertion
/// never leaves `TempDir::drop` a directory it cannot remove.
struct ReadOnlyDir(std::path::PathBuf);

impl ReadOnlyDir {
    fn new(path: &std::path::Path) -> Self {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o555))
            .expect("make root read-only");
        Self(path.to_path_buf())
    }
}

impl Drop for ReadOnlyDir {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
    }
}

/// Permission bits do not refuse euid 0, so the read-only cases cannot fail
/// there; they skip rather than report a false red.
fn running_as_root() -> bool {
    if nix::unistd::geteuid().is_root() {
        eprintln!("skipping: permission bits do not refuse euid 0");
        return true;
    }
    false
}

/// #8147: a colocated request on a read-only root is refused with an error
/// that names the permission problem, leaves nothing behind, and a retry with
/// `colocated: false` then registers the index in the data-dir store.
///
/// Why: this is the reported defect. The operator got `500 corpus open
/// failed`, and `colocated: false` did not help because it was ignored.
/// What: makes the root `0o555`, registers with `colocated` omitted and
/// asserts `403` + "permission denied", no registry row (memory or
/// `indexes.toml`), no `.trusty-search/` and no data-dir directory; then
/// retries with `Some(false)` and asserts `200`, `colocated = false` in
/// `indexes.toml`, and the corpus in the data dir.
/// Test: this test.
#[tokio::test]
#[serial_test::serial]
async fn create_index_colocated_on_read_only_root_names_the_permission_problem() {
    if running_as_root() {
        return;
    }
    let _data_dir = super::tests_components::IsolatedDataDir::new();
    let state = mock_state().await;
    let (_dir, root) = super::test_support::allowlisted_index_root("ts-8147-ro-");
    // Declared after `_dir`, so it drops first and the tempdir is writable again.
    let _read_only = ReadOnlyDir::new(&root);
    let id = IndexId::new("ts-8147-ro");

    let refused = super::indexes::create_index_handler(
        State(Arc::clone(&state)),
        Json(create_req_with_colocated(&id.0, root.clone(), None)),
    )
    .await;
    assert_eq!(
        refused.status(),
        StatusCode::FORBIDDEN,
        "#8147: a colocated registration the daemon cannot write must be refused \
         as a permission problem, not a generic 500"
    );
    let body = axum::body::to_bytes(refused.into_body(), usize::MAX)
        .await
        .expect("read body");
    let body: serde_json::Value = serde_json::from_slice(&body).expect("json body");
    let error = body["error"].as_str().unwrap_or_default();
    assert!(
        error.contains("permission denied") && error.contains("colocated=false"),
        "the error must name the permission problem and the way out. Body: {body}"
    );

    assert!(
        state.registry.get(&id).is_none(),
        "no half-registered handle"
    );
    assert!(
        crate::service::persistence::find_index_registry_entry(&id.0)
            .expect("registry readable")
            .is_none(),
        "no half-registered indexes.toml row"
    );
    assert!(
        !root.join(".trusty-search").exists(),
        "nothing created under the root"
    );
    let data_dir_index = crate::service::persistence::data_dir()
        .expect("data dir")
        .join("indexes")
        .join(crate::service::persistence::sanitize_id_for_path(&id.0));
    assert!(
        !data_dir_index.exists(),
        "no partial data-dir directory at {}",
        data_dir_index.display()
    );

    let retried = super::indexes::create_index_handler(
        State(Arc::clone(&state)),
        Json(create_req_with_colocated(&id.0, root.clone(), Some(false))),
    )
    .await;
    assert_eq!(
        retried.status(),
        StatusCode::OK,
        "#8147: colocated=false on a read-only root must register"
    );
    let entry = crate::service::persistence::find_index_registry_entry(&id.0)
        .expect("registry readable")
        .expect("the retry must be persisted");
    assert!(!entry.colocated, "indexes.toml must record colocated=false");
    assert!(
        data_dir_index.join("index.redb").exists(),
        "the corpus must live in the data dir at {}",
        data_dir_index.display()
    );

    state.watcher_manager.stop_for_index(&id).await;
}

/// #8147: a request body without `colocated` behaves exactly as before — the
/// corpus is colocated under the root and the registry records it so.
///
/// Why: the field is additive; every existing caller omits it.
/// What: deserializes a JSON body that has no `colocated` key, registers it,
/// and asserts `colocated = true` in `indexes.toml` and
/// `<root>/.trusty-search/index.redb` on disk.
/// Test: this test.
#[tokio::test]
#[serial_test::serial]
async fn create_index_omitted_colocated_keeps_the_colocated_default() {
    let _data_dir = super::tests_components::IsolatedDataDir::new();
    let state = mock_state().await;
    let (_dir, root) = super::test_support::allowlisted_index_root("ts-8147-default-");
    let id = IndexId::new("ts-8147-default");

    let req: super::router::CreateIndexRequest =
        serde_json::from_value(serde_json::json!({ "id": id.0, "root_path": root }))
            .expect("a body without `colocated` must deserialize");
    assert_eq!(req.colocated, None);

    let created = super::indexes::create_index_handler(State(Arc::clone(&state)), Json(req)).await;
    assert_eq!(created.status(), StatusCode::OK);

    let entry = crate::service::persistence::find_index_registry_entry(&id.0)
        .expect("registry readable")
        .expect("the registration must be persisted");
    assert!(entry.colocated, "an omitted field keeps the #403 default");
    assert!(
        root.join(".trusty-search").join("index.redb").exists(),
        "the corpus must be colocated under the root"
    );
    assert!(
        roots_toml_lists(&root),
        "a colocated root is listed in roots.toml for the startup scanner"
    );

    state.watcher_manager.stop_for_index(&id).await;
}

/// #8147 round 2, finding 1: a root whose `.trusty-search/` the daemon cannot
/// write is registrable with `colocated: false`.
///
/// Why: the `403` for such a root says to retry with `colocated=false`, and a
/// `400` guard then refused that retry because the directory exists: a
/// root-owned `.trusty-search/` could never be registered.
/// What: pre-creates `<root>/.trusty-search/` at `0o555`; the omitted-field
/// request is a `403`, the `colocated: false` retry is a `200` whose corpus is
/// in the data dir, and the repo directory stays empty.
/// Test: this test. With the `400` guard restored the retry answers `400`.
#[tokio::test]
#[serial_test::serial]
async fn create_index_colocated_false_over_a_read_only_colocated_dir_registers() {
    if running_as_root() {
        return;
    }
    let _data_dir = super::tests_components::IsolatedDataDir::new();
    let state = mock_state().await;
    let (_dir, root) = super::test_support::allowlisted_index_root("ts-8147-rodir-");
    let repo_dir = root.join(".trusty-search");
    std::fs::create_dir_all(&repo_dir).expect("pre-create colocated dir");
    let _read_only = ReadOnlyDir::new(&repo_dir);
    let id = IndexId::new("ts-8147-rodir");

    let refused = super::indexes::create_index_handler(
        State(Arc::clone(&state)),
        Json(create_req_with_colocated(&id.0, root.clone(), None)),
    )
    .await;
    assert_eq!(refused.status(), StatusCode::FORBIDDEN, "precondition: 403");

    let retried = super::indexes::create_index_handler(
        State(Arc::clone(&state)),
        Json(create_req_with_colocated(&id.0, root.clone(), Some(false))),
    )
    .await;
    assert_eq!(
        retried.status(),
        StatusCode::OK,
        "#8147: the retry the 403 names must register"
    );
    assert!(state.registry.get(&id).is_some(), "the retry is registered");
    let entry = crate::service::persistence::find_index_registry_entry(&id.0)
        .expect("registry readable")
        .expect("the retry must be persisted");
    assert!(!entry.colocated, "indexes.toml must record colocated=false");
    let data_dir_corpus =
        crate::service::persistence::corpus_redb_path(&id.0).expect("data-dir corpus path");
    assert!(
        data_dir_corpus.exists(),
        "the corpus must live in the data dir at {}",
        data_dir_corpus.display()
    );
    let written: Vec<_> = std::fs::read_dir(&repo_dir)
        .expect("repo dir readable")
        .collect();
    assert!(written.is_empty(), "nothing written to the repo dir");

    state.watcher_manager.stop_for_index(&id).await;
}

/// #8147 round 2, finding 2: a data-dir registration whose `indexes.toml` row
/// cannot be written is a `500`, and nothing is registered.
///
/// Why: warm boot rediscovers a colocated index from `roots.toml`, but a
/// data-dir index lives only in `indexes.toml`. A warn-only write failure
/// answered `200` for an index that vanished on restart.
/// What: the per-id `RegistryFault` seam fails the upsert without touching the
/// shared `indexes.toml` (round 3, finding 4: a planted unparseable file broke
/// concurrent non-serial tests). The request is a `500` naming `indexes.toml`;
/// no handle, no `indexes.toml` row and no `roots.toml` row. With the fault
/// disarmed, the same request registers.
/// Test: this test. Before the fix the first request answers `200`.
#[tokio::test]
#[serial_test::serial]
async fn create_index_colocated_false_with_an_unwritable_registry_is_a_500() {
    let _data_dir = super::tests_components::IsolatedDataDir::new();
    let state = mock_state().await;
    let (_dir, root) = super::test_support::allowlisted_index_root("ts-8147-noreg-");
    let id = IndexId::new("ts-8147-noreg");
    let fault = super::create_layout::registry_fault::RegistryFault::arm(&id.0);

    let refused = super::indexes::create_index_handler(
        State(Arc::clone(&state)),
        Json(create_req_with_colocated(&id.0, root.clone(), Some(false))),
    )
    .await;
    assert_eq!(
        refused.status(),
        StatusCode::INTERNAL_SERVER_ERROR,
        "#8147: a data-dir index that cannot be recorded must not answer 200"
    );
    let body = axum::body::to_bytes(refused.into_body(), usize::MAX)
        .await
        .expect("read body");
    let body: serde_json::Value = serde_json::from_slice(&body).expect("json body");
    assert!(
        body["error"]
            .as_str()
            .unwrap_or_default()
            .contains("indexes.toml"),
        "the error must name the registry. Body: {body}"
    );
    assert!(
        state.registry.get(&id).is_none(),
        "no half-registered handle"
    );
    assert!(
        crate::service::persistence::find_index_registry_entry(&id.0)
            .expect("registry readable")
            .is_none(),
        "no half-registered indexes.toml row"
    );
    assert!(!roots_toml_lists(&root), "no roots.toml row either");

    drop(fault);
    let retried = super::indexes::create_index_handler(
        State(Arc::clone(&state)),
        Json(create_req_with_colocated(&id.0, root.clone(), Some(false))),
    )
    .await;
    assert_eq!(retried.status(), StatusCode::OK, "a clean retry registers");
    assert!(state.registry.get(&id).is_some());

    state.watcher_manager.stop_for_index(&id).await;
}

/// Status and JSON body of a handler response.
async fn status_and_body(resp: axum::response::Response) -> (StatusCode, serde_json::Value) {
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("read body");
    (status, serde_json::from_slice(&bytes).expect("json body"))
}

/// POST a create for `id` at `root` with `colocated`.
async fn create(
    state: &Arc<SearchAppState>,
    id: &IndexId,
    root: &std::path::Path,
    colocated: Option<bool>,
) -> (StatusCode, serde_json::Value) {
    let resp = super::indexes::create_index_handler(
        State(Arc::clone(state)),
        Json(create_req_with_colocated(
            &id.0,
            root.to_path_buf(),
            colocated,
        )),
    )
    .await;
    status_and_body(resp).await
}

/// Register `id` with `colocated`, then cold-park it through the residency
/// sweep's own `cold_park_index`. Returns the parked `indexes.toml` entry.
async fn register_then_park(
    state: &Arc<SearchAppState>,
    id: &IndexId,
    root: &std::path::Path,
    colocated: Option<bool>,
) -> crate::service::persistence::PersistedIndex {
    let (status, body) = create(state, id, root, colocated).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "precondition: register. Body: {body}"
    );
    state.watcher_manager.stop_for_index(id).await;
    let entry = crate::service::persistence::find_index_registry_entry(&id.0)
        .expect("registry readable")
        .expect("registered");
    let parked = crate::service::lazy_loader::cold_park_index(
        id,
        &state.registry,
        &state.cold_store,
        entry.clone(),
        || false,
    )
    .await;
    assert!(parked, "precondition: the index is cold-parked");
    assert!(
        state.registry.get(id).is_none(),
        "precondition: not resident"
    );
    entry
}

/// #8147 round 3, finding 1: a cold-parked `colocated=false` index keeps its
/// layout when a re-POST omits `colocated`.
///
/// Why: a cold id misses the live-handle early return, so the omitted field
/// defaulted to colocated. The daemon created `<root>/.trusty-search/`, built
/// an empty corpus there, rewrote `indexes.toml` to `colocated=true` and
/// answered `created:true`, orphaning the data-dir corpus. The CLI, MCP
/// `create_index` and session launch all omit the field.
/// What: registers `colocated: false`, parks it, re-POSTs with the field
/// omitted, and asserts `indexes.toml` still says `colocated=false`, the new
/// handle serves the data-dir layout, and nothing exists under the root.
/// Test: this test. At 27160dfe5 the root gains `.trusty-search/`.
#[tokio::test]
#[serial_test::serial]
async fn create_index_cold_data_dir_index_keeps_its_layout_when_colocated_is_omitted() {
    let _data_dir = super::tests_components::IsolatedDataDir::new();
    let state = mock_state().await;
    let (_dir, root) = super::test_support::allowlisted_index_root("ts-8147-cold-omit-");
    let id = IndexId::new("ts-8147-cold-omit");
    register_then_park(&state, &id, &root, Some(false)).await;

    let (status, body) = create(&state, &id, &root, None).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the re-POST reloads it. Body: {body}"
    );

    assert!(
        !root.join(".trusty-search").exists(),
        "#8147: an omitted field must not move a data-dir index into the repo"
    );
    let entry = crate::service::persistence::find_index_registry_entry(&id.0)
        .expect("registry readable")
        .expect("still registered");
    assert!(
        !entry.colocated,
        "#8147: indexes.toml must keep the recorded colocated=false"
    );
    let handle = state.registry.get(&id).expect("resident again");
    assert_eq!(
        crate::service::storage_layout::layout_of(&handle).await,
        crate::service::storage_layout::StorageLayout::DataDir,
        "the reloaded handle serves the data-dir corpus"
    );
    assert!(!roots_toml_lists(&root), "no roots.toml row");

    state.watcher_manager.stop_for_index(&id).await;
}

/// #8147 round 3, finding 1: an explicit `colocated` that differs from a
/// cold-parked index's recorded layout is a `409`, in both directions.
///
/// Why: `colocated: true` against a cold data-dir index built a colocated
/// corpus and orphaned the data-dir one; `colocated: false` against a cold
/// colocated index abandoned the live `.trusty-search/`. Both answered `200`.
/// What: for each direction, registers, parks, POSTs the other layout, and
/// asserts a `409` naming both layouts, the id still cold and not resident,
/// `indexes.toml` unchanged, and no corpus created in the other layout.
/// Test: this test. At 27160dfe5 both requests answer `200`.
#[tokio::test]
#[serial_test::serial]
async fn create_index_cold_index_refuses_an_explicit_layout_change() {
    let _data_dir = super::tests_components::IsolatedDataDir::new();
    let state = mock_state().await;
    for (name, registered) in [("to-colo", false), ("to-data", true)] {
        let (_dir, root) =
            super::test_support::allowlisted_index_root(&format!("ts-8147-cold-{name}-"));
        let id = IndexId::new(format!("ts-8147-cold-{name}"));
        register_then_park(&state, &id, &root, Some(registered)).await;
        let repo_dir_before = root.join(".trusty-search").exists();

        let (status, body) = create(&state, &id, &root, Some(!registered)).await;
        assert_eq!(
            status,
            StatusCode::CONFLICT,
            "#8147 ({name}): a request must not change a recorded layout. Body: {body}"
        );
        assert_eq!(body["registered_colocated"], registered, "{body}");
        assert_eq!(body["requested_colocated"], !registered, "{body}");

        assert!(state.registry.get(&id).is_none(), "{name}: not registered");
        assert!(state.cold_store.contains(&id), "{name}: still parked cold");
        let entry = crate::service::persistence::find_index_registry_entry(&id.0)
            .expect("registry readable")
            .expect("still registered");
        assert_eq!(
            entry.colocated, registered,
            "{name}: indexes.toml unchanged"
        );
        assert_eq!(
            root.join(".trusty-search").exists(),
            repo_dir_before,
            "{name}: nothing created or removed under the root"
        );
        if registered {
            let data_dir_corpus =
                crate::service::persistence::corpus_redb_path(&id.0).expect("data-dir path");
            assert!(
                !data_dir_corpus.exists(),
                "{name}: no data-dir corpus at {}",
                data_dir_corpus.display()
            );
        }
    }
}

/// #8147 round 3, finding 2: a resident index answers an explicit layout
/// change with `409`; a matching or omitted field keeps `200 created:false`.
///
/// Why: the same-id, same-tree early return answered `200 created:false` to
/// any `colocated`, so a caller asking for the other layout believed it got it.
/// What: registers `colocated: false`, then POSTs `true` (`409`), `false` and
/// omitted (both `200 created:false`), and asserts the root stays empty.
/// Test: this test. At 27160dfe5 the `true` request answers `200`.
#[tokio::test]
#[serial_test::serial]
async fn create_index_live_index_refuses_an_explicit_layout_change() {
    let _data_dir = super::tests_components::IsolatedDataDir::new();
    let state = mock_state().await;
    let (_dir, root) = super::test_support::allowlisted_index_root("ts-8147-live-");
    let id = IndexId::new("ts-8147-live");
    let (status, _) = create(&state, &id, &root, Some(false)).await;
    assert_eq!(status, StatusCode::OK, "precondition: register");

    let (status, body) = create(&state, &id, &root, Some(true)).await;
    assert_eq!(status, StatusCode::CONFLICT, "#8147: {body}");
    assert_eq!(body["registered_colocated"], false, "{body}");

    for colocated in [Some(false), None] {
        let (status, body) = create(&state, &id, &root, colocated).await;
        assert_eq!(status, StatusCode::OK, "{colocated:?}: {body}");
        assert_eq!(body["created"], false, "{colocated:?}: {body}");
    }
    assert!(state.registry.get(&id).is_some(), "still resident");
    assert!(
        !root.join(".trusty-search").exists(),
        "nothing under the root"
    );

    state.watcher_manager.stop_for_index(&id).await;
}

/// Plant an `indexes.toml` row for `id` at `root`, with no in-memory handle
/// and no cold entry — the state a lazy load leaves between the handler's two
/// in-memory lookups, and the state of an id held in no store at all.
fn plant_row_only(id: &IndexId, root: &std::path::Path, colocated: bool) {
    crate::service::persistence::upsert_index_registry_entry(
        crate::service::persistence::PersistedIndex {
            id: id.0.clone(),
            root_path: root.to_path_buf(),
            colocated,
            ..Default::default()
        },
    )
    .expect("plant indexes.toml row");
}

/// #8147 final round, finding 1: an id whose only record is its `indexes.toml`
/// row keeps the row's layout.
///
/// Why: the handler read the layout from the live registry and then the cold
/// store. A lazy load between the two moves the id live and removes its cold
/// entry, so both miss and the request chose the layout, building an empty
/// corpus in the other layout. The row survives that load.
/// What: plants a `colocated=false` row with nothing in memory, then asserts
/// an explicit `colocated: true` is a `409` that creates nothing, and an
/// omitted field registers the data-dir layout with the row unchanged.
/// Test: this test. With the layout read from the cold store, the `true`
/// request answers `200` and the omitted one creates `<root>/.trusty-search/`.
#[tokio::test]
#[serial_test::serial]
async fn create_index_row_only_id_inherits_its_recorded_layout() {
    let _data_dir = super::tests_components::IsolatedDataDir::new();
    let state = mock_state().await;
    let (_dir, root) = super::test_support::allowlisted_index_root("ts-8147-row-");
    let id = IndexId::new("ts-8147-row");
    plant_row_only(&id, &root, false);

    let (status, body) = create(&state, &id, &root, Some(true)).await;
    assert_eq!(status, StatusCode::CONFLICT, "#8147: {body}");
    assert_eq!(body["registered_colocated"], false, "{body}");
    assert!(state.registry.get(&id).is_none(), "409 registers nothing");
    assert!(
        !root.join(".trusty-search").exists(),
        "409 creates nothing under the root"
    );

    let (status, body) = create(&state, &id, &root, None).await;
    assert_eq!(status, StatusCode::OK, "omitted field registers. {body}");
    assert!(
        !root.join(".trusty-search").exists(),
        "#8147: an omitted field must keep the row's data-dir layout"
    );
    let entry = crate::service::persistence::find_index_registry_entry(&id.0)
        .expect("registry readable")
        .expect("still registered");
    assert!(!entry.colocated, "indexes.toml keeps colocated=false");
    let handle = state.registry.get(&id).expect("resident");
    assert_eq!(
        crate::service::storage_layout::layout_of(&handle).await,
        crate::service::storage_layout::StorageLayout::DataDir,
    );

    state.watcher_manager.stop_for_index(&id).await;
}

/// #8147 final round, finding 1: a create whose `indexes.toml` cannot be read
/// is refused, never decided from the request.
///
/// Why: an unreadable registry says nothing about whether the id already
/// records a layout, so choosing one could orphan the real corpus.
/// What: arms a per-id read fault (a planted bad file would break concurrent
/// tests sharing `TRUSTY_DATA_DIR`) and asserts a `500` naming `indexes.toml`,
/// with no handle registered and no `.trusty-search/` created.
/// Test: this test. Falling back to `None` on the read error answers `200`.
#[tokio::test]
#[serial_test::serial]
async fn create_index_refuses_when_the_registry_cannot_be_read() {
    let _data_dir = super::tests_components::IsolatedDataDir::new();
    let state = mock_state().await;
    let (_dir, root) = super::test_support::allowlisted_index_root("ts-8147-noread-");
    let id = IndexId::new("ts-8147-noread");
    let _fault = super::create_layout::registry_fault::RegistryFault::arm_read(&id.0);

    let (status, body) = create(&state, &id, &root, None).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
    assert!(
        body["error"]
            .as_str()
            .unwrap_or_default()
            .contains("indexes.toml"),
        "the error names the registry. Body: {body}"
    );
    assert!(state.registry.get(&id).is_none(), "nothing registered");
    assert!(
        !root.join(".trusty-search").exists(),
        "nothing created under the root"
    );
}

/// #8147 final round, finding 3: a layout `409` is not masked by the warming
/// embedder's retryable `503`.
///
/// Why: a caller that retries a `503` would retry a request that can never
/// succeed, and never see the conflict.
/// What: a state with no embedder installed, a planted `colocated=true` row,
/// and an explicit `colocated: false` at the same root → `409`.
/// Test: this test. With the layout resolved after `current_embedder()`, the
/// request answers `503 embedder_initializing`.
#[tokio::test]
#[serial_test::serial]
async fn create_index_layout_conflict_is_409_while_the_embedder_warms() {
    let _data_dir = super::tests_components::IsolatedDataDir::new();
    let state = Arc::new(SearchAppState::new(IndexRegistry::new()));
    assert!(state.current_embedder().await.is_none(), "precondition");
    let (_dir, root) = super::test_support::allowlisted_index_root("ts-8147-warm-");
    let id = IndexId::new("ts-8147-warm");
    plant_row_only(&id, &root, true);

    let (status, body) = create(&state, &id, &root, Some(false)).await;
    assert_eq!(status, StatusCode::CONFLICT, "#8147: {body}");
}

/// #8147 final round, finding 2: `resolve_layout` across request × record.
///
/// Why: an omitted field must inherit the recorded layout even at a new root,
/// as relocation keeps it (#1089); an explicit value decides at a new root.
/// What: table over (request root, `colocated`, recorded layout).
/// Test: this test. With the same-tree filter on the omitted arm, the
/// new-root omitted rows resolve to colocated.
#[test]
fn resolve_layout_inherits_or_decides_per_request_and_record() {
    let old = std::path::PathBuf::from("/tmp/ts-8147-resolve/old");
    let new = std::path::PathBuf::from("/tmp/ts-8147-resolve/new");
    let row = |colocated| crate::service::persistence::PersistedIndex {
        id: "ts-8147-resolve".into(),
        root_path: old.clone(),
        colocated,
        ..Default::default()
    };
    // (request root, requested, recorded, expected layout; `None` = 409)
    let cases = [
        (&new, None, None, Some(true)),
        (&new, Some(false), None, Some(false)),
        (&old, None, Some(false), Some(false)),
        (&new, None, Some(false), Some(false)),
        (&new, None, Some(true), Some(true)),
        (&new, Some(true), Some(false), Some(true)),
        (&new, Some(false), Some(true), Some(false)),
        (&old, Some(true), Some(true), Some(true)),
        (&old, Some(true), Some(false), None),
        (&old, Some(false), Some(true), None),
    ];
    for (root, asked, recorded, want) in cases {
        let req = create_req_with_colocated("ts-8147-resolve", root.clone(), asked);
        let recorded = recorded.map(row);
        let got = super::create_layout::resolve_layout(&req, recorded.as_ref());
        let context = format!("{root:?} asked={asked:?} recorded={recorded:?}");
        match want {
            Some(layout) => assert_eq!(got.ok(), Some(layout), "{context}"),
            None => assert_eq!(
                got.err().map(|(status, _)| status),
                Some(StatusCode::CONFLICT),
                "{context}"
            ),
        }
    }
}
