//! A key rewrite that lands two ids on one never leaves an orphan vector
//! (#8778).
//!
//! Why: M003 and M005 rename ids through `rewrite_keys`. A rename onto an id
//! that was still mapped replaced its `id_to_key` entry but left the replaced
//! key's vector in the graph, so the saved binary held more vectors than its
//! sidecar described. `load_from`'s #3970 torn-pair guard then discarded the
//! snapshot on every boot and the index served BM25 only.
//! What: collapses ids through a rewrite, saves, and reloads; the reload must
//! keep the snapshot, and the graph must hold exactly one vector per mapped id.
//! Test: this module.

use super::types::VectorStore;
use super::usearch_store::UsearchStore;

/// Seed a store with one vector per id, in id order along the unit axes.
async fn seeded(ids: &[&str]) -> UsearchStore {
    let store = UsearchStore::new(4).expect("store init");
    for (i, id) in ids.iter().enumerate() {
        let mut v = vec![0.0_f32; 4];
        v[i % 4] = 1.0;
        v[(i + 1) % 4] = 0.5;
        store.upsert(id, v).await.expect("upsert");
    }
    store
}

/// #8778: renaming `a` onto the still-mapped `b` used to save a 2-vector
/// binary beside a 1-entry sidecar, which `load_from` refuses as torn.
#[tokio::test]
async fn a_rewrite_onto_a_mapped_id_saves_a_pair_that_reloads() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("hnsw.usearch");
    let store = seeded(&["a", "b"]).await;

    let remap = |id: &str| -> Option<String> { (id == "a").then(|| "b".to_string()) };
    assert_eq!(store.rewrite_keys(&remap).await.expect("rewrite"), 1);
    assert_eq!(
        store.len().await.expect("len"),
        1,
        "the graph keeps one vector per mapped id; the replaced key's vector is dropped"
    );
    store.save(&path).await.expect("save");

    let reloaded = UsearchStore::load_from(&path)
        .await
        .expect("load")
        .expect("#8778: the saved pair must not be discarded as torn");
    assert_eq!(reloaded.len().await.expect("len"), 1);
    assert!(reloaded.contains("b").await);
    assert!(!reloaded.contains("a").await);
}

/// #8778: two ids renamed onto one target, plus a chain `c → d`, `d → e`.
/// The chain must not count `d` as taken, so only the collapse drops a vector.
#[tokio::test]
async fn collapsing_and_chained_rewrites_keep_graph_and_sidecar_equal() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("hnsw.usearch");
    let store = seeded(&["a1", "a2", "c", "d"]).await;

    let remap = |id: &str| -> Option<String> {
        match id {
            "a1" | "a2" => Some("a".to_string()),
            "c" => Some("d".to_string()),
            "d" => Some("e".to_string()),
            _ => None,
        }
    };
    assert_eq!(store.rewrite_keys(&remap).await.expect("rewrite"), 4);
    assert_eq!(store.len().await.expect("len"), 3, "a, d, e");
    for id in ["a", "d", "e"] {
        assert!(store.contains(id).await, "{id} stays mapped");
    }
    store.save(&path).await.expect("save");

    let reloaded = UsearchStore::load_from(&path)
        .await
        .expect("load")
        .expect("#8778: graph and sidecar agree, so the pair reloads");
    assert_eq!(reloaded.len().await.expect("len"), 3);
}
