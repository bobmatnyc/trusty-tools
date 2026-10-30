//! Load-time reconcile of the key sidecar against the HNSW graph (#10301).
//!
//! Why: `publish_snapshot` renames the sidecar before the binary. A kill
//! between the two renames leaves `id_to_key` naming keys the graph does not
//! hold, and `contains_many` would report those ids as embedded forever.
//! What: drops every sidecar entry whose key the graph lacks, then checks
//! that the kept entries cover the graph exactly.
//! Test: `core::indexer::tests::load_reconcile_10301`.

use std::collections::{HashMap, HashSet};

use anyhow::{anyhow, Result};

/// Drop the `id_to_key` entries whose key `graph_has` rejects.
///
/// Why: a sidecar ahead of its graph must not claim vectors the graph lacks,
/// so the next catch-up pass embeds those ids again.
/// What: one `graph_has(key)` call per entry; read-only on the graph, so a
/// view-mode index stays mapped. Returns the number of dropped entries.
/// Postcondition on `Ok`: every kept key is in the graph, and the kept keys
/// number exactly `graph_size`.
/// Errors: when the kept keys do not cover the graph. Either the graph holds
/// keys no entry names (a torn pairing in the #3970 direction) or the
/// membership check itself is wrong; the caller discards the snapshot.
/// Test: `core::indexer::tests::load_reconcile_10301::a_sidecar_ahead_of_the_graph_owes_and_reembeds_the_missing_ids`,
/// `tests::a_membership_check_that_misses_graph_keys_is_an_error`,
/// `tests::a_view_load_reconciles_without_promoting_or_writing`,
/// `core::indexer::tests::load_reconcile_10301::a_sidecar_that_also_dropped_graph_keys_discards_the_snapshot`.
pub(super) fn drop_keys_missing_from_graph(
    id_to_key: &mut HashMap<String, u64>,
    graph_has: impl Fn(u64) -> bool,
    graph_size: usize,
) -> Result<usize> {
    let before = id_to_key.len();
    id_to_key.retain(|_, key| graph_has(*key));
    let dropped = before - id_to_key.len();
    let kept_keys = id_to_key.values().collect::<HashSet<_>>().len();
    if kept_keys != graph_size {
        return Err(anyhow!(
            "graph holds {graph_size} vectors but the sidecar names {kept_keys} of them \
             after dropping {dropped} missing keys"
        ));
    }
    Ok(dropped)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The error arm: a check that misses keys the graph holds must not
    /// report a healthy, reconciled map.
    #[test]
    fn a_membership_check_that_misses_graph_keys_is_an_error() {
        let mut map: HashMap<String, u64> = [("a".to_string(), 1), ("b".to_string(), 2)].into();
        let err = drop_keys_missing_from_graph(&mut map, |_| false, 2)
            .expect_err("a check that finds nothing in a 2-vector graph is not a reconcile");
        assert!(err.to_string().contains("graph holds 2 vectors"), "{err}");
    }

    /// Entries whose key the graph lacks are dropped and counted.
    #[test]
    fn entries_the_graph_lacks_are_dropped_and_counted() {
        let mut map: HashMap<String, u64> = [
            ("a".to_string(), 1),
            ("b".to_string(), 2),
            ("c".to_string(), 3),
        ]
        .into();
        let dropped = drop_keys_missing_from_graph(&mut map, |k| k != 3, 2).expect("reconcile");
        assert_eq!(dropped, 1);
        assert!(!map.contains_key("c"));
    }

    /// The kill window through the real save path: the sidecar publishes, the
    /// binary rename never runs. The load reconciles on the mmap view without
    /// promoting it and without rewriting either file.
    #[tokio::test]
    async fn a_view_load_reconciles_without_promoting_or_writing() {
        use super::super::types::VectorStore;
        use super::super::usearch_store::{staging_path, UsearchStore};
        use std::sync::atomic::Ordering;

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("hnsw.usearch");
        let store = UsearchStore::new(4).expect("new");
        store
            .upsert("a", vec![1.0, 0.0, 0.0, 0.0])
            .await
            .expect("a");
        store.save_to(&path).await.expect("save");
        store
            .upsert("b", vec![0.0, 1.0, 0.0, 0.0])
            .await
            .expect("b");
        let killed = store
            .save_with_publisher(&path, |path, key_map| {
                let sidecar = path.with_extension("keys.json");
                let tmp = staging_path(&sidecar, "json");
                std::fs::write(&tmp, serde_json::to_vec(key_map)?)?;
                std::fs::rename(&tmp, &sidecar)?;
                Err(anyhow!("killed before the binary rename"))
            })
            .await;
        assert!(killed.is_err());
        drop(store);
        let sidecar = path.with_extension("keys.json");
        let before = (
            std::fs::read(&path).expect("graph"),
            std::fs::read(&sidecar).expect("sidecar"),
        );

        let loaded = UsearchStore::load_from(&path)
            .await
            .expect("load")
            .expect("loads");
        assert!(
            loaded.is_view.load(Ordering::Acquire),
            "the reconcile must not promote the view"
        );
        assert_eq!(
            loaded.contains_many(&["a".into(), "b".into()]).await,
            vec![true, false]
        );
        assert_eq!(
            loaded.key_to_id.read().await.len(),
            1,
            "the reverse map matches"
        );
        drop(loaded);
        let after = (
            std::fs::read(&path).expect("graph"),
            std::fs::read(&sidecar).expect("sidecar"),
        );
        assert!(before == after, "a load writes neither file");
    }
}
