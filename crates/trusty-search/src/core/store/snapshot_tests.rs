//! Snapshot capture and publication regressions for #6961.
//! Why: graph and key maps must cross a save boundary together.
//! What: drive real futures at lock boundaries, then cold-load the files.
//! Test: this module.

use std::sync::atomic::Ordering;

use super::types::VectorStore;
use super::usearch_store::{staging_path, UsearchStore};

#[tokio::test]
async fn snapshot_excludes_partial_mutations_and_reloads_vector_ids() {
    for operation in ["insert", "remove", "batch", "rewrite"] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hnsw.usearch");
        let store = UsearchStore::new(4).unwrap();
        store.upsert("a", vec![1.0, 0.0, 0.0, 0.0]).await.unwrap();
        store.upsert("b", vec![0.0, 1.0, 0.0, 0.0]).await.unwrap();
        let before = store.id_to_key.read().await.clone();
        let graph = store.index.write().await;
        let mut save = Box::pin(store.save(&path));
        assert!(futures::poll!(&mut save).is_pending());
        let mut mutation = Box::pin(async {
            match operation {
                "insert" => store.upsert("c", vec![0.0, 0.0, 1.0, 0.0]).await,
                "remove" => store.remove("a").await,
                "batch" => {
                    store
                        .upsert_batch(&[("c".into(), vec![0.0, 0.0, 1.0, 0.0])])
                        .await
                }
                "rewrite" => store
                    .rewrite_keys(&|id| Some(format!("new/{id}")))
                    .await
                    .map(|_| ()),
                _ => unreachable!(),
            }
        });
        let _ = futures::poll!(&mut mutation);
        assert_eq!(
            *store.id_to_key.read().await,
            before,
            "{operation} changed maps while save was capturing the graph"
        );
        drop(graph);
        save.await.unwrap();
        mutation.await.unwrap();
        let prior = UsearchStore::load_from(&path).await.unwrap().unwrap();
        assert_eq!(prior.len().await.unwrap(), 2);
        assert_eq!(
            prior.search(&[1.0, 0.0, 0.0, 0.0], 1).await.unwrap()[0].chunk_id,
            "a"
        );
        store.save(&path).await.unwrap();
        let loaded = UsearchStore::load_from(&path).await.unwrap().unwrap();
        let expected = store.id_to_key.read().await.clone();
        assert_eq!(*loaded.id_to_key.read().await, expected);
        assert_eq!(loaded.len().await.unwrap(), expected.len());
        let (query, id) = match operation {
            "insert" | "batch" => ([0.0, 0.0, 1.0, 0.0], "c"),
            "remove" => ([0.0, 1.0, 0.0, 0.0], "b"),
            "rewrite" => ([1.0, 0.0, 0.0, 0.0], "new/a"),
            _ => unreachable!(),
        };
        assert_eq!(loaded.search(&query, 1).await.unwrap()[0].chunk_id, id);
        loaded
            .upsert("after-reload", vec![0.0, 0.0, 0.0, 1.0])
            .await
            .unwrap();
        assert_eq!(loaded.len().await.unwrap(), expected.len() + 1);
    }
}

#[tokio::test]
async fn sidecar_staging_failure_preserves_binary_and_removal_credit() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hnsw.usearch");
    let store = UsearchStore::new(4).unwrap();
    store.upsert("a", vec![1.0, 0.0, 0.0, 0.0]).await.unwrap();
    store.upsert("b", vec![0.0, 1.0, 0.0, 0.0]).await.unwrap();
    store.save(&path).await.unwrap();
    let binary = std::fs::read(&path).unwrap();
    let sidecar = path.with_extension("keys.json");
    let keys = std::fs::read(&sidecar).unwrap();
    store.remove("a").await.unwrap();
    let tmp = staging_path(&sidecar, "json");
    std::fs::create_dir(&tmp).unwrap();
    assert!(store.save(&path).await.is_err());
    assert_eq!(std::fs::read(&path).unwrap(), binary);
    assert_eq!(std::fs::read(&sidecar).unwrap(), keys);
    assert_eq!(store.removed_since_save.load(Ordering::Acquire), 1);
    assert!(store.dirty.load(Ordering::Acquire));
    assert!(!staging_path(&path, "usearch").exists());
    let loaded = UsearchStore::load_from(&path).await.unwrap().unwrap();
    assert_eq!(loaded.len().await.unwrap(), 2);
    std::fs::remove_dir(tmp).unwrap();
    store.save(&path).await.unwrap();
    assert_eq!(store.removed_since_save.load(Ordering::Acquire), 0);
}

#[tokio::test]
async fn binary_publication_failure_restores_removed_and_rewritten_keys() {
    for operation in ["remove", "rewrite"] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hnsw.usearch");
        let store = UsearchStore::new(4).unwrap();
        store.upsert("a", vec![1.0, 0.0, 0.0, 0.0]).await.unwrap();
        store.upsert("b", vec![0.0, 1.0, 0.0, 0.0]).await.unwrap();
        store.save(&path).await.unwrap();
        let binary = std::fs::read(&path).unwrap();
        let sidecar = path.with_extension("keys.json");
        let keys = std::fs::read(&sidecar).unwrap();
        if operation == "remove" {
            store.remove("a").await.unwrap();
        } else {
            store
                .rewrite_keys(&|id| Some(format!("new/{id}")))
                .await
                .unwrap();
        }
        let map = super::types::StoreKeyMap {
            id_to_key: store.id_to_key.read().await.clone(),
            next_key: store.next_key.load(Ordering::Relaxed),
            dim: 4,
        };
        // The missing staged binary makes the SECOND rename fail, after the
        // new sidecar was actually published. The old live binary still exists.
        let error = super::snapshot_publish::publish_snapshot(&path, &map).unwrap_err();
        assert!(error.to_string().contains("previous sidecar restored"));
        assert_eq!(std::fs::read(&path).unwrap(), binary);
        assert_eq!(std::fs::read(&sidecar).unwrap(), keys);
        let loaded = UsearchStore::load_from(&path).await.unwrap().unwrap();
        assert_eq!(loaded.len().await.unwrap(), 2);
        assert_eq!(
            loaded.search(&[1.0, 0.0, 0.0, 0.0], 1).await.unwrap()[0].chunk_id,
            "a"
        );
    }
}

#[tokio::test]
async fn binary_publication_failure_restores_absent_sidecar() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hnsw.usearch");
    let map = super::types::StoreKeyMap {
        id_to_key: Default::default(),
        next_key: 1,
        dim: 4,
    };
    assert!(super::snapshot_publish::publish_snapshot(&path, &map).is_err());
    assert!(!path.with_extension("keys.json").exists());
    assert!(!staging_path(&path.with_extension("keys.json"), "json").exists());
}

#[tokio::test]
async fn rewritten_keys_remain_dirty_until_saved_and_demoted() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hnsw.usearch");
    let store = UsearchStore::new(4).unwrap();
    store.upsert("a", vec![1.0, 0.0, 0.0, 0.0]).await.unwrap();
    store.save(&path).await.unwrap();
    assert!(store.demote_to_view().await.unwrap());
    store
        .rewrite_keys(&|_| Some("renamed".into()))
        .await
        .unwrap();
    assert!(
        !store.demote_to_view().await.unwrap(),
        "unsaved key rewrite must prevent demotion"
    );
    store.save(&path).await.unwrap();
    assert!(store.demote_to_view().await.unwrap());
    let loaded = UsearchStore::load_from(&path).await.unwrap().unwrap();
    assert_eq!(
        loaded.search(&[1.0, 0.0, 0.0, 0.0], 1).await.unwrap()[0].chunk_id,
        "renamed"
    );
}

#[tokio::test]
async fn sidecar_rename_failure_preserves_previous_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hnsw.usearch");
    let store = UsearchStore::new(4).unwrap();
    store.upsert("a", vec![1.0, 0.0, 0.0, 0.0]).await.unwrap();
    store.upsert("b", vec![0.0, 1.0, 0.0, 0.0]).await.unwrap();
    store.save(&path).await.unwrap();
    let binary = std::fs::read(&path).unwrap();
    let sidecar = path.with_extension("keys.json");
    let keys = std::fs::read(&sidecar).unwrap();
    store.remove("a").await.unwrap();
    // Exercise real graph serialization and sidecar staging, but fail the
    // first rename independently of the test process's OS privileges.
    let error = store
        .save_with_publisher(&path, |path, map| {
            super::snapshot_publish::publish_snapshot_with_rename(path, map, |from, to| {
                assert_eq!(from, staging_path(&sidecar, "json"));
                assert_eq!(to, sidecar);
                assert!(staging_path(path, "usearch").is_file());
                let staged: super::types::StoreKeyMap =
                    serde_json::from_slice(&std::fs::read(from).unwrap()).unwrap();
                assert_eq!(staged.id_to_key.len(), 1);
                assert!(staged.id_to_key.contains_key("b"));
                Err(std::io::Error::other("injected first rename failure"))
            })
        })
        .await
        .unwrap_err();
    assert_eq!(error.to_string(), "rename hnsw key sidecar");
    assert_eq!(
        error.root_cause().to_string(),
        "injected first rename failure"
    );
    assert!(!staging_path(&path, "usearch").exists());
    assert!(!staging_path(&sidecar, "json").exists());
    assert_eq!(std::fs::read(&path).unwrap(), binary);
    assert_eq!(std::fs::read(&sidecar).unwrap(), keys);
    assert_eq!(store.removed_since_save.load(Ordering::Acquire), 1);
    assert!(store.dirty.load(Ordering::Acquire));
    let loaded = UsearchStore::load_from(&path).await.unwrap().unwrap();
    assert_eq!(loaded.len().await.unwrap(), 2);
    assert_eq!(
        loaded.search(&[1.0, 0.0, 0.0, 0.0], 1).await.unwrap()[0].chunk_id,
        "a"
    );
}
