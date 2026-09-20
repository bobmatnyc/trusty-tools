use super::*;
use std::sync::{Arc, Mutex};
#[tokio::test]
async fn registration_is_required_and_files_are_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let registry = ProjectRegistry::with_registry_path(root.join("registry.json"));
    assert_eq!(
        registered_path(&registry, root.to_str().unwrap())
            .await
            .unwrap_err()
            .0,
        StatusCode::NOT_FOUND
    );
    registry.register_pm_start(&root).await.unwrap();
    assert_eq!(
        registered_path(&registry, root.to_str().unwrap())
            .await
            .unwrap(),
        root
    );
    assert_eq!(
        registered_path(&registry, ".").await.unwrap_err().0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        registered_path(&registry, root.join("registry.json").to_str().unwrap())
            .await
            .unwrap_err()
            .0,
        StatusCode::BAD_REQUEST
    );
}
#[cfg(unix)]
#[tokio::test]
async fn registered_alias_resolves_canonical_target_and_missing_alias_fails_closed() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("project");
    std::fs::create_dir(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let alias = tmp.path().join("project-alias");
    std::os::unix::fs::symlink(&root, &alias).unwrap();
    let registry = ProjectRegistry::with_registry_path(tmp.path().join("registry.json"));
    registry.register_pm_start(&alias).await.unwrap();
    assert_eq!(
        registered_path(&registry, root.to_str().unwrap())
            .await
            .unwrap(),
        root
    );
    std::fs::remove_file(&alias).unwrap();
    assert_eq!(
        registered_path(&registry, root.to_str().unwrap())
            .await
            .unwrap_err()
            .0,
        StatusCode::NOT_FOUND
    );
}
#[test]
fn destination_uses_bound_tree_and_refuses_missing_or_invalid_binding() {
    let tmp = tempfile::tempdir().unwrap();
    let agents = tmp.path().join("agents");
    std::fs::create_dir(&agents).unwrap();
    std::fs::write(
        agents.join("fixture.toml"),
        "[[stores]]\nname='fixture-index'\ntree='okg://shared-fixture'\n",
    )
    .unwrap();
    let knowledge = tmp.path().join("knowledge");
    let found = destination(std::slice::from_ref(&agents), &knowledge, "fixture").unwrap();
    assert_eq!(found.root, knowledge.join("shared-fixture"));
    assert_eq!(found.index, "fixture-index");
    assert!(destination(std::slice::from_ref(&agents), &knowledge, "../fixture").is_err());
    std::fs::write(agents.join("fixture.toml"), "[agent]\nname='fixture'\n").unwrap();
    assert_eq!(
        destination(&[agents], &knowledge, "fixture").unwrap_err().0,
        StatusCode::CONFLICT
    );
}
#[tokio::test]
async fn index_reuses_exact_root_instead_of_basename_collision() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let captured = Arc::new(Mutex::new(vec![]));
    let calls = captured.clone();
    let root2 = root.clone();
    let daemon=crate::uds_mock::spawn(move |method,params| {
        calls.lock().unwrap().push((method.to_owned(),params));
        let reply=if method==search_rpc::METHOD_INDEXES_LIST {json!({"indexes":[{"id":"different","root_path":"/missing"},{"id":"correct","root_path":root2}]})}else{json!({"status":"started"})};
        Box::pin(async move {Ok(reply)})
    }).await;
    assert_eq!(
        start_index(daemon.socket(), &root).await.unwrap()["index_id"],
        "correct"
    );
    let calls = captured.lock().unwrap();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[1].1["index_id"], "correct");
}
#[tokio::test]
async fn index_creates_then_verifies_registration_before_reindex() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let calls = Arc::new(Mutex::new(vec![]));
    let captured = calls.clone();
    let root2 = root.clone();
    let daemon = crate::uds_mock::spawn(move |method, params| {
        let mut calls = captured.lock().unwrap();
        calls.push((method.to_owned(), params));
        let reply = if method == search_rpc::METHOD_INDEXES_LIST && calls.len() == 1 {
            json!({"indexes":[]})
        } else if method == search_rpc::METHOD_INDEXES_LIST {
            json!({"indexes":[{"id":"created","root_path":root2}]})
        } else {
            json!({"status":"started"})
        };
        Box::pin(async move { Ok(reply) })
    })
    .await;
    assert_eq!(
        start_index(daemon.socket(), &root).await.unwrap()["index_id"],
        "created"
    );
    let calls = calls.lock().unwrap();
    assert_eq!(calls[1].0, search_rpc::METHOD_INDEX_CREATE);
    assert_eq!(calls[1].1["follow_links"], false);
    assert_eq!(calls[3].0, search_rpc::METHOD_INDEX_REINDEX);
}
/// Spawn a mock search daemon that reports `indexes` and counts every call.
///
/// Why: the #4289 guard is only proved by what it does NOT send — no
/// `index.create`, no `index.reindex` — so the tests need the call log.
/// Test: `index_refuses_a_root_inside_an_existing_index`.
async fn daemon_listing(
    indexes: Value,
) -> (crate::uds_mock::MockMemoryDaemon, Arc<Mutex<Vec<String>>>) {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let captured = calls.clone();
    let mock = crate::uds_mock::spawn(move |method, _params| {
        captured.lock().unwrap().push(method.to_owned());
        let reply = if method == search_rpc::METHOD_INDEXES_LIST {
            indexes.clone()
        } else {
            json!({"status":"started"})
        };
        Box::pin(async move { Ok(reply) })
    })
    .await;
    (mock, calls)
}

#[tokio::test]
async fn index_refuses_a_root_inside_an_existing_index() {
    // #4289: an exact-match check accepts a subdirectory of an indexed tree,
    // and the resulting overlapping roots reindex each other (#402, #2178).
    let tmp = tempfile::tempdir().unwrap();
    let outer = tmp.path().canonicalize().unwrap();
    let inner = outer.join("crates").join("api");
    std::fs::create_dir_all(&inner).unwrap();
    let (daemon, calls) =
        daemon_listing(json!({"indexes":[{"id":"outer-index","root_path":outer}]})).await;
    let (status, body) = start_index(daemon.socket(), &inner).await.unwrap_err();
    assert_eq!(status, StatusCode::CONFLICT);
    let message = body.0["error"].as_str().unwrap().to_owned();
    assert!(
        message.contains("outer-index") && message.contains(&outer.display().to_string()),
        "refusal must name the existing index and its root: {message}"
    );
    assert_eq!(
        *calls.lock().unwrap(),
        vec![search_rpc::METHOD_INDEXES_LIST.to_string()],
        "nothing may be created or reindexed after a refusal"
    );
}

#[tokio::test]
async fn index_refuses_a_root_that_encloses_an_existing_index() {
    // #4289: the ancestor case — indexing the parent would swallow the index
    // already covering the child.
    let tmp = tempfile::tempdir().unwrap();
    let outer = tmp.path().canonicalize().unwrap();
    let inner = outer.join("crates").join("api");
    std::fs::create_dir_all(&inner).unwrap();
    let (daemon, calls) =
        daemon_listing(json!({"indexes":[{"id":"inner-index","root_path":inner}]})).await;
    let (status, body) = start_index(daemon.socket(), &outer).await.unwrap_err();
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(
        body.0["error"].as_str().unwrap().contains("inner-index"),
        "refusal must name the enclosed index: {}",
        body.0["error"]
    );
    assert_eq!(calls.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn index_creates_for_a_sibling_of_an_existing_index() {
    // Negative control: the guard must not refuse a directory that merely
    // shares a name prefix with an indexed root.
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path().canonicalize().unwrap();
    let indexed = base.join("app");
    let sibling = base.join("app2");
    std::fs::create_dir_all(&indexed).unwrap();
    std::fs::create_dir_all(&sibling).unwrap();
    let sibling2 = sibling.clone();
    let indexed2 = indexed.clone();
    let daemon = crate::uds_mock::spawn(move |method, _params| {
        let listed = json!({"indexes":[{"id":"app-index","root_path":indexed2},{"id":"new","root_path":sibling2}]});
        let reply = if method == search_rpc::METHOD_INDEXES_LIST {
            listed
        } else {
            json!({"status":"started"})
        };
        Box::pin(async move { Ok(reply) })
    })
    .await;
    assert_eq!(
        start_index(daemon.socket(), &sibling).await.unwrap()["index_id"],
        "new"
    );
}

/// Mock daemon whose `indexes.list` is empty and whose `index.create` refuses.
///
/// Why: the client-side guard cannot see an index the daemon knows about but
/// did not list, so trusty-search's own refusal is the only thing standing
/// between a stale listing and an overlapping root.
/// Test: `index_maps_the_search_overlap_conflict_to_409_with_the_existing_id`.
async fn daemon_refusing_create(message: &'static str) -> crate::uds_mock::MockMemoryDaemon {
    crate::uds_mock::spawn(move |method, _params| {
        let outcome = if method == search_rpc::METHOD_INDEX_CREATE {
            Err(crate::uds_mock::RpcError::new(
                trusty_common::search_rpc::CODE_CONFLICT,
                message,
            ))
        } else {
            Ok(json!({"indexes":[]}))
        };
        Box::pin(async move { outcome })
    })
    .await
}

#[tokio::test]
async fn index_maps_the_search_overlap_conflict_to_409_with_the_existing_id() {
    // #4289: `search_rpc::call_at` collapses the daemon's 409 body to
    // {code, message}, so the route's only route to `existing_index_id` is the
    // sentence `root_overlap_response` writes. Before this, the refusal
    // surfaced as a 503 and the GUI reported the daemon as down.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let daemon = daemon_refusing_create(
        "\"/srv/app/crates\" is inside the root of index 'srv-app' (\"/srv/app\"); \
         overlapping index roots let one reindex prune the other's corpus. \
         Attach to 'srv-app' instead, or pick a directory outside it",
    )
    .await;
    let (status, body) = start_index(daemon.socket(), &root).await.unwrap_err();
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body.0["existing_index_id"], "srv-app");
    assert!(
        body.0["error"].as_str().unwrap().contains("is inside"),
        "the daemon's own sentence is passed through: {}",
        body.0["error"]
    );
}

#[tokio::test]
async fn index_conflict_without_a_named_index_still_reports_409() {
    // A conflict whose message names no index (#6864's id collision, say) must
    // still be a 409 — just without an `existing_index_id` to attach to.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let daemon = daemon_refusing_create("that id is already registered").await;
    let (status, body) = start_index(daemon.socket(), &root).await.unwrap_err();
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(
        body.0.get("existing_index_id").is_none(),
        "no id may be invented: {}",
        body.0
    );
}

#[tokio::test]
async fn import_fixture_is_additive_idempotent_and_explicitly_targets_bound_store() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("project");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("note.md"), "# Fixture\nA durable project note.\n").unwrap();
    let store_root = tmp.path().join("bound-store");
    let destination = Destination {
        root: store_root.clone(),
        tree: "okg://fixture-destination".into(),
        index: "fixture-index".into(),
    };
    let args = import_args(&root, "fixture-project-import-test", &destination).unwrap();
    assert_eq!(args["root"], store_root.to_str().unwrap());
    let policy = trusty_kb::okg::policy::DocStorePolicy::new(vec![root.clone()]);
    for expected in [1, 0] {
        let store = trusty_kb::store::KbStore::new(
            store_root.clone(),
            trusty_kb::schema::Profile::default_profile(),
        );
        let result = crate::tools::okg::ingest_into_store(&args, store, &policy)
            .await
            .unwrap();
        assert!(!result.is_error());
        let value: Value = serde_json::from_str(result.content()).unwrap();
        assert_eq!(value["ingest"]["ingested"], expected);
        assert_eq!(value["index"]["pending"], 1);
    }
    assert!(!tmp.path().join("fixture-project-import-test").exists());
}
