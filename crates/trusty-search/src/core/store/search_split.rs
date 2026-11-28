//! Where one vector-lane search spent its time (#12108).
//!
//! Why: a cold first query logged `vector_ms=27150` as one number, so the
//! log could not say whether the time went to waiting for the graph (a load,
//! promotion or rebuild holds its write lock), to the HNSW traversal itself
//! (on an mmap view this is where the page fault-in lands), or to mapping the
//! hits back to chunk ids.
//! What: [`VectorSplit`], the per-sub-step durations, and a task-local
//! recorder: [`measured`] runs a future and returns the split the store's
//! searches inside it recorded through [`record`]. Outside [`measured`],
//! [`record`] is a no-op, so a search with no caller asking pays one
//! task-local miss. Nothing here locks, allocates or does I/O.
//! Test: `a_measured_view_search_records_its_split`,
//! `record_outside_a_measurement_is_a_no_op`.

use std::cell::Cell;
use std::future::Future;
use std::time::Duration;

/// Wall time per vector-lane sub-step, summed over the searches one query ran.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VectorSplit {
    /// Waiting for the tombstone set and the graph read lock.
    pub lock_wait: Duration,
    /// The usearch traversal (FFI), including page faults on an mmap view.
    pub traverse: Duration,
    /// Resolving the returned keys to chunk ids, lock wait included.
    pub map: Duration,
    /// `true` when a traversal ran on the read-only mmap view, not a heap copy.
    pub on_view: bool,
}

impl VectorSplit {
    /// Add `other` into `self`: durations sum, `on_view` ORs.
    pub fn absorb(&mut self, other: VectorSplit) {
        self.lock_wait = self.lock_wait.saturating_add(other.lock_wait);
        self.traverse = self.traverse.saturating_add(other.traverse);
        self.map = self.map.saturating_add(other.map);
        self.on_view |= other.on_view;
    }
}

tokio::task_local! {
    static VECTOR_SPLIT: Cell<VectorSplit>;
}

/// Run `fut` and return its output with the split every store search inside
/// it recorded.
pub async fn measured<F: Future>(fut: F) -> (F::Output, VectorSplit) {
    VECTOR_SPLIT
        .scope(Cell::new(VectorSplit::default()), async {
            let out = fut.await;
            let split = VECTOR_SPLIT.with(Cell::get);
            (out, split)
        })
        .await
}

/// Add one search's split to the enclosing [`measured`]; a no-op outside it.
pub(super) fn record(split: VectorSplit) {
    let _ = VECTOR_SPLIT.try_with(|c| {
        let mut sum = c.get();
        sum.absorb(split);
        c.set(sum);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::store::{UsearchStore, VectorStore};

    /// A search of a snapshot opened as an mmap view records a traversal on
    /// the view; a second search adds to the same split.
    #[tokio::test]
    async fn a_measured_view_search_records_its_split() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("hnsw.usearch");
        let store = UsearchStore::new(4).expect("store");
        store
            .upsert("src/a.rs:1:2", vec![1.0, 0.0, 0.0, 0.0])
            .await
            .expect("upsert");
        store.save(&path).await.expect("save");
        drop(store);
        let view = UsearchStore::load_from(&path)
            .await
            .expect("load")
            .expect("snapshot present");

        let q = [1.0, 0.0, 0.0, 0.0];
        let (hits, split) = measured(async {
            let a = view.search(&q, 1).await.expect("search");
            let b = view.search(&q, 1).await.expect("search");
            (a, b)
        })
        .await;
        assert_eq!(hits.0.len(), 1);
        assert!(split.on_view, "{split:?}");
        assert!(split.traverse > Duration::ZERO, "{split:?}");

        let (_, one) = measured(view.search(&q, 1)).await;
        assert!(
            split.traverse > one.traverse || split.map > one.map,
            "two searches sum"
        );
    }

    /// Outside [`measured`] a recorded split goes nowhere and does not panic.
    #[test]
    fn record_outside_a_measurement_is_a_no_op() {
        record(VectorSplit {
            traverse: Duration::from_millis(1),
            ..Default::default()
        });
    }
}
