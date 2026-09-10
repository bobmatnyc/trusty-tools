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
    let found = destination(&[agents.clone()], &knowledge, "fixture").unwrap();
    assert_eq!(found.root, knowledge.join("shared-fixture"));
    assert_eq!(found.index, "fixture-index");
    assert!(destination(&[agents.clone()], &knowledge, "../fixture").is_err());
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
