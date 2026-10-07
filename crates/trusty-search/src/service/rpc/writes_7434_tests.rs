//! Multi-root writes over both transports (#7434).
//!
//! Why: `POST /indexes/:id/roots` and `search.index.roots.add` must be one
//! body, refused the same way, and an absolute `remove-file` path under an
//! additional root must remove that root's `@root<n>/…` key and forget the
//! same key's hash (#9236).
//! What: drives the real axum router and the real RPC router from the
//! parent's harness.
//! Test: this file.

use super::*;

/// A planted index over `primary` that also covers `extra`.
fn planted_multi_root(id: &str, primary: &Path, extra: &Path) -> IndexRegistry {
    let registry = planted_registry(id, primary);
    let old = registry.get(&IndexId::new(id)).expect("planted");
    let mut handle = IndexHandle::bare(
        old.id.clone(),
        Arc::clone(&old.indexer),
        primary.to_path_buf(),
    );
    handle.additional_roots = vec![extra.to_path_buf()];
    old.indexer
        .try_write()
        .expect("uncontended")
        .set_additional_roots(vec![extra.to_path_buf()]);
    registry.register(handle);
    registry
}

/// The `roots` array a body reports, as strings.
fn roots_of(body: &serde_json::Value) -> Vec<String> {
    body["roots"]
        .as_array()
        .unwrap_or_else(|| panic!("no roots array: {body}"))
        .iter()
        .map(|v| v.as_str().unwrap_or_default().to_string())
        .collect()
}

/// Why: the add route and its socket twin must be one core — the same table,
/// persisted, watched and walked. On the pre-#7434 daemon neither exists.
/// What: two subjects built identically; one widened over HTTP, one over the
/// socket. Each answer names the full table and a queued reindex, and each
/// handle and `indexes.toml` row carries the new root afterwards.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial]
async fn add_root_over_http_and_socket_returns_the_full_table() {
    let isolated = crate::service::server::tests_components::IsolatedDataDir::new();
    let (http_root, http_extra) = (safe_root("roots-http-a"), safe_root("roots-http-b"));
    let (sock_root, sock_extra) = (safe_root("roots-sock-a"), safe_root("roots-sock-b"));
    let approved = [&*http_root, &*http_extra, &*sock_root, &*sock_extra];
    let (state, http, rpc) = empty_state(isolated.path(), &approved).await;
    http_ok(
        &http,
        "POST",
        "/indexes",
        create_body("roots-http", &http_root),
    )
    .await;
    http_ok(
        &http,
        "POST",
        "/indexes",
        create_body("roots-sock", &sock_root),
    )
    .await;

    let over_http = http_ok(
        &http,
        "POST",
        "/indexes/roots-http/roots",
        serde_json::json!({ "roots": [http_extra] }),
    )
    .await;
    let over_socket = rpc_ok(
        &rpc,
        writes::METHOD_INDEX_ROOTS_ADD,
        serde_json::json!({ "index_id": "roots-sock", "body": { "roots": [sock_extra] } }),
    )
    .await;

    for (id, body, primary, extra) in [
        ("roots-http", &over_http, &http_root, &http_extra),
        ("roots-sock", &over_socket, &sock_root, &sock_extra),
    ] {
        let want = vec![primary.display().to_string(), extra.display().to_string()];
        assert_eq!(roots_of(body), want, "{id}: the full table, primary first");
        assert_eq!(body["added"], serde_json::json!([extra]), "{id}");
        assert_eq!(body["reindex_queued"], serde_json::json!(true), "{id}");
        let handle = state.registry.get(&IndexId::new(id)).expect("registered");
        assert_eq!(handle.additional_roots, vec![extra.clone()], "{id}: handle");
        assert_eq!(
            handle.indexer.read().await.additional_roots,
            vec![extra.clone()],
            "{id}: the indexer decodes against the same table"
        );
        let row = persistence::load_index_registry()
            .expect("load registry")
            .into_iter()
            .find(|e| e.id == id)
            .expect("row");
        assert_eq!(row.additional_roots, vec![extra.clone()], "{id}: persisted");
    }
}

/// Why: the refusal arm — a root another index already covers — must be
/// refused identically on both transports and change nothing (fail-open
/// check: neither the handle nor the `indexes.toml` row may move).
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial]
async fn add_root_refuses_another_indexes_additional_root() {
    let isolated = crate::service::server::tests_components::IsolatedDataDir::new();
    let (a, shared, b) = (
        safe_root("own-a"),
        safe_root("own-shared"),
        safe_root("own-b"),
    );
    let (state, http, rpc) = empty_state(isolated.path(), &[&a, &shared, &b]).await;
    http_ok(&http, "POST", "/indexes", create_body("own-a", &a)).await;
    http_ok(&http, "POST", "/indexes", create_body("own-b", &b)).await;
    http_ok(
        &http,
        "POST",
        "/indexes/own-a/roots",
        serde_json::json!({ "roots": [shared] }),
    )
    .await;

    let body = serde_json::json!({ "roots": [shared] });
    let over_http = http_err(&http, "POST", "/indexes/own-b/roots", body.clone()).await;
    assert_eq!(over_http.0, StatusCode::CONFLICT, "{}", over_http.1);
    let over_socket = rpc_err(
        &rpc,
        writes::METHOD_INDEX_ROOTS_ADD,
        serde_json::json!({ "index_id": "own-b", "body": body }),
    )
    .await;
    assert_same_refusal(&over_http, &over_socket, CODE_CONFLICT, "shared root");
    let handle = state.registry.get(&IndexId::new("own-b")).expect("own-b");
    assert!(handle.additional_roots.is_empty(), "nothing was added");
    let row = persistence::load_index_registry()
        .expect("load")
        .into_iter()
        .find(|e| e.id == "own-b")
        .expect("row");
    assert!(row.additional_roots.is_empty(), "nothing was persisted");
}

/// Why: a root nested in the index's own primary would be watched twice and
/// stored under two keys. Second error arm; nothing changes.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial]
async fn add_root_refuses_a_root_nested_in_its_own_primary() {
    let isolated = crate::service::server::tests_components::IsolatedDataDir::new();
    let primary = safe_root("nest-primary");
    let nested = primary.join("vendor");
    std::fs::create_dir_all(&nested).expect("mkdir");
    let (state, http, _rpc) = empty_state(isolated.path(), &[&primary, &nested]).await;
    http_ok(&http, "POST", "/indexes", create_body("nest", &primary)).await;

    let (status, body) = http_err(
        &http,
        "POST",
        "/indexes/nest/roots",
        serde_json::json!({ "roots": [nested] }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"], "index_root_overlap", "{body}");
    let handle = state.registry.get(&IndexId::new("nest")).expect("nest");
    assert!(handle.additional_roots.is_empty());
}

/// Why (#8889): the table must not change under a running reindex, which
/// walks and prunes against the table it started with. Error arm.
/// What: a held claim makes the add answer `409 reindex_already_running`
/// with nothing added or persisted.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial]
async fn add_root_is_refused_while_a_reindex_runs() {
    let isolated = crate::service::server::tests_components::IsolatedDataDir::new();
    let (primary, extra) = (safe_root("busy-a"), safe_root("busy-b"));
    let (state, http, _rpc) = empty_state(isolated.path(), &[&primary, &extra]).await;
    http_ok(&http, "POST", "/indexes", create_body("busy", &primary)).await;
    let claim = loop {
        // The create's own catch-up reindex may still hold the claim.
        match crate::service::reindex::try_claim_reindex(&IndexId::new("busy"), "test", false) {
            Ok(claim) => break claim,
            Err(_) => tokio::time::sleep(std::time::Duration::from_millis(20)).await,
        }
    };

    let (status, body) = http_err(
        &http,
        "POST",
        "/indexes/busy/roots",
        serde_json::json!({ "roots": [extra] }),
    )
    .await;
    drop(claim);
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"], "reindex_already_running", "{body}");
    let handle = state.registry.get(&IndexId::new("busy")).expect("busy");
    assert!(handle.additional_roots.is_empty(), "nothing was added");
}

/// Why: naming a root the index already holds must be an idempotent no-op,
/// not a second table slot that stores one file under two keys.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial]
async fn add_root_of_own_root_is_a_no_op() {
    let isolated = crate::service::server::tests_components::IsolatedDataDir::new();
    let primary = safe_root("noop-a");
    let (state, http, _rpc) = empty_state(isolated.path(), &[&primary]).await;
    http_ok(&http, "POST", "/indexes", create_body("noop", &primary)).await;

    let body = http_ok(
        &http,
        "POST",
        "/indexes/noop/roots",
        serde_json::json!({ "roots": [primary] }),
    )
    .await;
    assert_eq!(body["added"], serde_json::json!([]), "{body}");
    assert_eq!(body["reindex_queued"], serde_json::json!(false), "{body}");
    let handle = state.registry.get(&IndexId::new("noop")).expect("noop");
    assert!(handle.additional_roots.is_empty());
}

/// Why (#7434, criterion 3): a consumer such as trusty-agents' `covers()`
/// (#7429) learns an index's full root list from its status.
/// What: `GET /indexes/:id/status` and the socket status name every root,
/// primary first.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial]
async fn status_lists_every_root_and_its_watch() {
    let isolated = crate::service::server::tests_components::IsolatedDataDir::new();
    let (primary, extra) = (safe_root("status-a"), safe_root("status-b"));
    let (state, http, _rpc) = empty_state(isolated.path(), &[&primary, &extra]).await;
    let mut create = create_body("status", &primary);
    create["roots"] = serde_json::json!([extra]);
    http_ok(&http, "POST", "/indexes", create).await;

    let request = Request::builder()
        .uri("/indexes/status/status")
        .body(Body::empty())
        .expect("request");
    let (status, body) = http_raw(&http, request).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        roots_of(&body),
        vec![primary.display().to_string(), extra.display().to_string()],
        "{body}"
    );
    let reads = crate::service::rpc::reads::register(RpcRouter::new(), &state);
    let over_socket = rpc_ok(
        &reads,
        crate::service::rpc::reads::METHOD_INDEX_STATUS,
        serde_json::json!({ "index_id": "status" }),
    )
    .await;
    assert_eq!(roots_of(&over_socket), roots_of(&body), "both transports");
    assert!(body["watcher"]["roots"].is_array(), "{body}");
}

/// Why (#9236 × #7434): an absolute path under an ADDITIONAL root answered
/// `400 remove_file_path_outside_root` — it is outside the primary root — and
/// the file kept every chunk. It must map to its `@root<n>/…` key, remove those
/// chunks, and forget that same key's content hash.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
async fn remove_file_maps_an_additional_root_path_to_its_stored_key() {
    let primary = tempfile::tempdir().expect("tempdir");
    let extra = tempfile::tempdir().expect("tempdir");
    let registry = planted_multi_root("mr", primary.path(), extra.path());
    let (state, http, rpc) = routers(SearchAppState::new(registry)).await;
    let key = format!("@root1/{FILE}");
    let id = IndexId::new("mr");
    let seed = || async {
        let handle = state.registry.get(&id).expect("mr");
        let indexer = handle.indexer.read().await;
        indexer.index_file(&key, CONTENT).await.expect("seed");
        crate::service::reindex::hash::hashes_for(&id).insert(PathBuf::from(&key), "h".to_string());
    };

    seed().await;
    let absolute = extra.path().join(FILE);
    let reply = http_ok(
        &http,
        "POST",
        "/indexes/mr/remove-file",
        serde_json::json!({ "path": absolute }),
    )
    .await;
    assert!(reply["removed_chunks"].as_u64().unwrap_or(0) > 0, "{reply}");
    assert!(chunks_for(&state, "mr", &key).await.is_empty());
    assert!(
        !crate::service::reindex::hash::hashes_for(&id).contains_key(Path::new(&key)),
        "the hash forget must use the same @root1 key"
    );

    seed().await;
    let over_socket = rpc_ok(
        &rpc,
        writes::METHOD_INDEX_FILE_REMOVE,
        serde_json::json!({ "index_id": "mr", "body": { "path": absolute } }),
    )
    .await;
    assert!(over_socket["removed_chunks"].as_u64().unwrap_or(0) > 0);

    // Error arm: a path under neither root is still refused, and keeps chunks.
    seed().await;
    let outside = serde_json::json!({ "path": "/definitely/elsewhere/src/auth.rs" });
    let (status, body) = http_err(&http, "POST", "/indexes/mr/remove-file", outside).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"], "remove_file_path_outside_root");
    assert!(!chunks_for(&state, "mr", &key).await.is_empty());
}

/// Plant a multi-root index whose additional root holds `src/extra.rs`, both
/// on disk and as `@root1/src/extra.rs` chunks.
async fn planted_with_extra_file(
    primary: &Path,
    extra: &Path,
) -> (Arc<SearchAppState>, Router, PathBuf) {
    let on_disk = extra.join("src/extra.rs");
    std::fs::create_dir_all(on_disk.parent().expect("parent")).expect("mkdir");
    std::fs::write(&on_disk, "fn zebra_unicorn_marker() {}\n").expect("write");
    let registry = planted_multi_root("mrs", primary, extra);
    let (state, http, _rpc) = routers(SearchAppState::new(registry)).await;
    {
        let handle = state.registry.get(&IndexId::new("mrs")).expect("mrs");
        let indexer = handle.indexer.read().await;
        indexer
            .index_file("@root1/src/extra.rs", "fn zebra_unicorn_marker() {}\n")
            .await
            .expect("seed");
    }
    (state, http, on_disk)
}

/// Why (#64 × #7434): the search post-filter and the result's `file` must
/// both honour the additional root. On the single-root code the hit resolved
/// to `<primary>/@root1/src/extra.rs`, a path that does not exist.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
async fn search_keeps_a_hit_under_an_additional_root() {
    let primary = tempfile::tempdir().expect("tempdir");
    let extra = tempfile::tempdir().expect("tempdir");
    let (_state, http, on_disk) = planted_with_extra_file(primary.path(), extra.path()).await;

    let body = http_ok(
        &http,
        "POST",
        "/indexes/mrs/search",
        serde_json::json!({ "text": "zebra_unicorn_marker", "top_k": 5 }),
    )
    .await;
    let files: Vec<String> = body["results"]
        .as_array()
        .unwrap_or_else(|| panic!("no results array: {body}"))
        .iter()
        .map(|r| r["file"].as_str().unwrap_or_default().to_string())
        .collect();
    assert!(
        files.contains(&on_disk.display().to_string()),
        "#7434: the hit must resolve under its own root; got {files:?}"
    );
}

/// Why (#7434, item c): grep reads each indexed file from disk; an
/// `@root1/…` key joined to the primary root reads nothing, so every match
/// under an additional root was silently dropped.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
async fn grep_finds_a_match_under_an_additional_root() {
    let primary = tempfile::tempdir().expect("tempdir");
    let extra = tempfile::tempdir().expect("tempdir");
    let (_state, http, _on_disk) = planted_with_extra_file(primary.path(), extra.path()).await;

    let body = http_ok(
        &http,
        "POST",
        "/indexes/mrs/grep",
        serde_json::json!({ "pattern": "zebra_unicorn_marker" }),
    )
    .await;
    let files: Vec<&str> = body["matches"]
        .as_array()
        .unwrap_or_else(|| panic!("no matches array: {body}"))
        .iter()
        .filter_map(|m| m["file"].as_str())
        .collect();
    assert_eq!(files, vec!["@root1/src/extra.rs"], "{body}");
}

/// Why (#7434 review, fail-open check): an unreadable `indexes.toml` used to
/// be read as "no row" and replaced by a default record carrying only the
/// roots, dropping `colocated`, the timestamps and the serve-only mark. It
/// must answer `500 roots_not_persisted` with nothing added, on either
/// transport, and leave the file as it was.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial]
async fn add_root_with_an_unreadable_registry_adds_nothing() {
    let isolated = crate::service::server::tests_components::IsolatedDataDir::new();
    let (primary, extra) = (safe_root("unread-a"), safe_root("unread-b"));
    let (state, http, rpc) = empty_state(isolated.path(), &[&primary, &extra]).await;
    http_ok(&http, "POST", "/indexes", create_body("unread", &primary)).await;
    let toml = persistence::indexes_toml_path().expect("registry path");
    std::fs::write(&toml, "this is [[ not toml").expect("corrupt the registry");

    let body = serde_json::json!({ "roots": [extra] });
    let over_http = http_err(&http, "POST", "/indexes/unread/roots", body.clone()).await;
    assert_eq!(
        over_http.0,
        StatusCode::INTERNAL_SERVER_ERROR,
        "{}",
        over_http.1
    );
    assert_eq!(
        over_http.1["error"], "roots_not_persisted",
        "{}",
        over_http.1
    );
    let over_socket = rpc_err(
        &rpc,
        writes::METHOD_INDEX_ROOTS_ADD,
        serde_json::json!({ "index_id": "unread", "body": body }),
    )
    .await;
    assert_same_refusal(
        &over_http,
        &over_socket,
        CODE_INTERNAL_ERROR,
        "unreadable registry",
    );
    let handle = state.registry.get(&IndexId::new("unread")).expect("unread");
    assert!(handle.additional_roots.is_empty(), "nothing was added");
    assert_eq!(
        std::fs::read_to_string(&toml).expect("read"),
        "this is [[ not toml",
        "the registry file is left as it was"
    );
}
