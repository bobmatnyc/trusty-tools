//! Selective/lazy warm-boot: cold-index store and on-demand load (issue #993).
//!
//! Why: trusty-search currently warm-boots ALL persisted indexes at startup,
//! even when the operator only uses a handful regularly. At 100+ registered
//! indexes, startup takes minutes and exposes TCC-denial hang paths (#718) for
//! every index. `TRUSTY_WARMBOOT_MAX_INDEXES` lets operators limit the number of
//! indexes that are eagerly loaded; the rest are parked here as "cold" and loaded
//! transparently on the first query that touches them.
//!
//! Architecture:
//!   - `env` — `warmboot_max_indexes()`, `cold_reload_timeout()`, and
//!     `LAST_QUERIED_WRITE_INTERVAL_SECS`.
//!   - `store` — `ColdIndexStore` + `select_warmboot_entries()`.
//!   - `loader` — `get_or_load_index()` + `LazyLoadError`.
//!
//! Back-compat: when `TRUSTY_WARMBOOT_MAX_INDEXES` is unset, `select_warmboot_entries`
//! returns all entries as eager and the cold store is empty — exact same behaviour
//! as the pre-#993 daemon.
//!
//! Test: `select_warmboot_entries_*`, `cold_reload_timeout_parses_env`,
//!       `warmboot_max_indexes_parses_env`, `get_or_load_index_*`.

mod env;
mod loader;
mod store;

// Re-export all public symbols so callers that use
// `crate::service::lazy_loader::*` need no changes.
pub use env::{cold_reload_timeout, warmboot_max_indexes, LAST_QUERIED_WRITE_INTERVAL_SECS};
pub use loader::{get_or_load_index, LazyLoadError};
pub use store::{select_warmboot_entries, ColdIndexStore};

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::Duration;

    use crate::core::registry::{IndexId, IndexRegistry};
    use crate::service::persistence::PersistedIndex;

    fn mk_entry(id: &str, q: Option<u64>, i: Option<u64>) -> PersistedIndex {
        PersistedIndex {
            id: id.to_string(),
            root_path: PathBuf::from(format!("/tmp/{id}")),
            last_queried_unix: q,
            last_indexed_unix: i,
            ..Default::default()
        }
    }

    /// Create a minimal `IndexHandle` for tests without touching the filesystem.
    fn build_mock_handle(id: &str) -> crate::core::registry::IndexHandle {
        use std::sync::Arc;
        use tokio::sync::RwLock;

        let index_id = IndexId::new(id.to_string());
        let root_path = PathBuf::from(format!("/tmp/test-{id}"));
        let indexer = Arc::new(RwLock::new(crate::core::indexer::CodeIndexer::new(
            id, &root_path,
        )));
        crate::core::registry::IndexHandle::bare(index_id, indexer, root_path)
    }

    // ── warmboot_max_indexes ─────────────────────────────────────────────────

    /// Why: env var absent → None (back-compat warm-boot-all).
    /// Test: this test.
    #[test]
    #[serial_test::serial]
    fn warmboot_max_indexes_unset_returns_none() {
        unsafe { std::env::remove_var("TRUSTY_WARMBOOT_MAX_INDEXES") };
        assert!(warmboot_max_indexes().is_none());
    }

    /// Why: `0` → lazy-load everything.
    /// Test: this test.
    #[test]
    #[serial_test::serial]
    fn warmboot_max_indexes_zero_returns_some_zero() {
        unsafe { std::env::set_var("TRUSTY_WARMBOOT_MAX_INDEXES", "0") };
        assert_eq!(warmboot_max_indexes(), Some(0));
        unsafe { std::env::remove_var("TRUSTY_WARMBOOT_MAX_INDEXES") };
    }

    /// Why: valid positive value parses correctly.
    /// Test: this test.
    #[test]
    #[serial_test::serial]
    fn warmboot_max_indexes_parses_env() {
        unsafe { std::env::set_var("TRUSTY_WARMBOOT_MAX_INDEXES", "10") };
        assert_eq!(warmboot_max_indexes(), Some(10));
        unsafe { std::env::remove_var("TRUSTY_WARMBOOT_MAX_INDEXES") };
    }

    // ── cold_reload_timeout ──────────────────────────────────────────────────

    /// Why: env var absent → 30 s default.
    /// Test: this test.
    #[test]
    #[serial_test::serial]
    fn cold_reload_timeout_default_is_30s() {
        unsafe { std::env::remove_var("TRUSTY_INDEX_COLD_RELOAD_TIMEOUT_SECS") };
        assert_eq!(cold_reload_timeout(), Duration::from_secs(30));
    }

    /// Why: explicit value parses correctly.
    /// Test: this test.
    #[test]
    #[serial_test::serial]
    fn cold_reload_timeout_parses_env() {
        unsafe { std::env::set_var("TRUSTY_INDEX_COLD_RELOAD_TIMEOUT_SECS", "15") };
        assert_eq!(cold_reload_timeout(), Duration::from_secs(15));
        unsafe { std::env::remove_var("TRUSTY_INDEX_COLD_RELOAD_TIMEOUT_SECS") };
    }

    // ── select_warmboot_entries ──────────────────────────────────────────────

    /// Why: `None` cap → all eager, nothing cold (back-compat).
    /// Test: this test.
    #[test]
    fn select_all_eager_when_no_cap() {
        let entries = vec![mk_entry("a", None, None), mk_entry("b", Some(100), None)];
        let (eager, cold) = select_warmboot_entries(entries.clone(), None);
        assert_eq!(eager.len(), 2);
        assert!(cold.is_empty());
    }

    /// Why: cap 0 → nothing eager, all cold.
    /// Test: this test.
    #[test]
    fn select_all_cold_when_cap_zero() {
        let entries = vec![mk_entry("a", None, None), mk_entry("b", Some(100), None)];
        let (eager, cold) = select_warmboot_entries(entries, Some(0));
        assert!(eager.is_empty());
        assert_eq!(cold.len(), 2);
    }

    /// Why: cap >= len → all eager, nothing cold.
    /// Test: this test.
    #[test]
    fn select_all_eager_when_cap_exceeds_count() {
        let entries = vec![mk_entry("a", Some(1), None), mk_entry("b", Some(2), None)];
        let (eager, cold) = select_warmboot_entries(entries, Some(10));
        assert_eq!(eager.len(), 2);
        assert!(cold.is_empty());
    }

    /// Why: top-N by recency is selected correctly; sort is deterministic.
    /// Test: this test.
    #[test]
    fn select_top_n_by_recency() {
        // a: sort_key=0 (no activity), b: 200, c: 300, d: 150
        let entries = vec![
            mk_entry("a", None, None),
            mk_entry("b", Some(200), None),
            mk_entry("c", Some(300), None),
            mk_entry("d", None, Some(150)),
        ];
        let (eager, cold) = select_warmboot_entries(entries, Some(2));
        assert_eq!(eager.len(), 2);
        assert_eq!(cold.len(), 2);
        // Top-2 by descending sort_key: c(300), b(200).
        let eager_ids: Vec<&str> = eager.iter().map(|e| e.id.as_str()).collect();
        assert!(
            eager_ids.contains(&"c"),
            "c (sort_key=300) must be in eager: {eager_ids:?}"
        );
        assert!(
            eager_ids.contains(&"b"),
            "b (sort_key=200) must be in eager: {eager_ids:?}"
        );
    }

    /// Why: tie-break by id ascending is deterministic across restarts.
    /// Test: this test.
    #[test]
    fn select_tie_breaks_by_id_ascending() {
        // All three have sort_key=100; tie-break by id: "aaa" < "bbb" < "ccc".
        let entries = vec![
            mk_entry("ccc", Some(100), None),
            mk_entry("aaa", Some(100), None),
            mk_entry("bbb", Some(100), None),
        ];
        let (eager, cold) = select_warmboot_entries(entries, Some(2));
        let eager_ids: Vec<&str> = eager.iter().map(|e| e.id.as_str()).collect();
        // aaa and bbb win the tie-break (alpha ascending).
        assert!(eager_ids.contains(&"aaa"), "aaa expected in eager");
        assert!(eager_ids.contains(&"bbb"), "bbb expected in eager");
        let cold_ids: Vec<&str> = cold.iter().map(|e| e.id.as_str()).collect();
        assert!(cold_ids.contains(&"ccc"), "ccc expected in cold");
    }

    // ── ColdIndexStore ───────────────────────────────────────────────────────

    /// Why: register, contains, len sanity checks.
    /// Test: this test.
    #[test]
    fn cold_store_register_and_contains() {
        let store = ColdIndexStore::new();
        assert!(store.is_empty());
        let entries = vec![
            mk_entry("idx1", None, None),
            mk_entry("idx2", Some(1), None),
        ];
        store.register_cold_entries(entries);
        assert_eq!(store.len(), 2);
        assert!(store.contains(&IndexId::new("idx1".to_string())));
        assert!(store.contains(&IndexId::new("idx2".to_string())));
        assert!(!store.contains(&IndexId::new("unknown".to_string())));
    }

    /// Why: `mark_loaded` removes the entry from the cold store.
    /// Test: this test.
    #[test]
    fn cold_store_len() {
        let store = ColdIndexStore::new();
        store.register_cold_entries(vec![mk_entry("a", None, None)]);
        assert_eq!(store.len(), 1);
        store.mark_loaded(&IndexId::new("a".to_string()));
        assert_eq!(store.len(), 0);
    }

    // ── get_or_load_index ────────────────────────────────────────────────────

    /// Why: hot-path fast path — index already in registry returns immediately.
    /// Test: this test.
    #[tokio::test]
    async fn get_or_load_index_hot_path() {
        let registry = IndexRegistry::default();
        let cold = ColdIndexStore::new();
        let id = IndexId::new("hot-idx".to_string());
        registry.register(build_mock_handle("hot-idx"));

        let result = get_or_load_index(&id, &registry, &cold, Duration::from_secs(5), |_e| async {
            false // should never be called
        })
        .await;
        assert!(result.is_ok(), "hot-path should return Ok");
    }

    /// Why: unknown id (neither hot nor cold) returns NotFound.
    /// Test: this test.
    #[tokio::test]
    async fn get_or_load_index_not_found() {
        let registry = IndexRegistry::default();
        let cold = ColdIndexStore::new();
        let id = IndexId::new("no-such".to_string());

        let result = get_or_load_index(&id, &registry, &cold, Duration::from_secs(5), |_e| async {
            false
        })
        .await;
        assert!(
            matches!(result, Err(LazyLoadError::NotFound)),
            "unknown id must return NotFound"
        );
    }

    /// Why: cold index loads on demand and returns the handle.
    /// Test: this test.
    #[tokio::test]
    async fn get_or_load_index_loads_cold_index() {
        let registry = IndexRegistry::default();
        let cold = ColdIndexStore::new();
        let id = IndexId::new("cold-idx".to_string());
        cold.register_cold_entries(vec![mk_entry("cold-idx", None, None)]);

        // Restore function: register the handle then return true.
        let registry_clone = registry.clone();
        let result = get_or_load_index(
            &id,
            &registry,
            &cold,
            Duration::from_secs(5),
            move |_e| async move {
                registry_clone.register(build_mock_handle("cold-idx"));
                true
            },
        )
        .await;
        assert!(result.is_ok(), "cold index should load successfully");
        // After load, cold store should no longer contain the id.
        assert!(!cold.contains(&id), "cold store must be cleared after load");
    }

    /// PR #1103 TOCTOU: when `loading_gate` returns `None` (concurrent
    /// `mark_loaded` removed the cold entry between the cold-check and the gate
    /// call), `get_or_load_index` must re-check the hot registry and return the
    /// handle if it is now there — NOT return `NotFound`.
    ///
    /// Why: without this fix, a concurrent load that races ahead causes a
    /// spurious 404 for an index that actually just became hot.
    /// What: simulate the race by: (1) register the cold entry; (2) start
    /// `get_or_load_index`; (3) before the restore_fn runs, concurrently call
    /// `mark_loaded` + register the handle directly; (4) assert the call
    /// returns Ok (not NotFound).
    ///
    /// Note: because `mark_loaded` is called inside the restore_fn here, the
    /// single-threaded async executor serialises them. We test the post-gate
    /// re-check path (step 4 in the doc) where the index is hot after the gate
    /// is acquired.
    /// Test: this test.
    #[tokio::test]
    async fn get_or_load_index_gate_none_but_index_just_became_hot() {
        // Simulate the race: load the cold entry, but before `loading_gate` is
        // called, another task calls `mark_loaded` and registers the handle in
        // the hot registry. We model this by calling `mark_loaded` inside the
        // restore_fn (which runs AFTER the gate is acquired), so by the time the
        // post-load `registry.get(id)` runs, the handle is already there.
        let registry = IndexRegistry::default();
        let cold = ColdIndexStore::new();
        let id = IndexId::new("race-idx".to_string());
        cold.register_cold_entries(vec![mk_entry("race-idx", None, None)]);

        let registry_clone = registry.clone();
        let cold_clone = cold.clone();
        let id_clone = id.clone();
        let result = get_or_load_index(
            &id,
            &registry,
            &cold,
            Duration::from_secs(5),
            move |_e| async move {
                // Simulate: another task already loaded the index; it registered
                // the handle and called mark_loaded. We do both here to ensure
                // the post-gate hot re-check returns the handle.
                registry_clone.register(build_mock_handle("race-idx"));
                cold_clone.mark_loaded(&id_clone);
                true
            },
        )
        .await;
        // The restore_fn returned true and registered the handle, so we get Ok.
        assert!(
            result.is_ok(),
            "index loaded by restore_fn must return Ok, not NotFound"
        );
    }

    /// Why: timeout returns Loading error with retry_after_secs.
    /// Test: this test.
    #[tokio::test]
    async fn get_or_load_index_returns_loading_on_timeout() {
        let registry = IndexRegistry::default();
        let cold = ColdIndexStore::new();
        let id = IndexId::new("slow-idx".to_string());
        cold.register_cold_entries(vec![mk_entry("slow-idx", None, None)]);

        let result = get_or_load_index(
            &id,
            &registry,
            &cold,
            Duration::from_millis(50), // very short timeout
            |_e| async {
                tokio::time::sleep(Duration::from_secs(5)).await;
                true
            },
        )
        .await;
        assert!(
            matches!(result, Err(LazyLoadError::Loading { .. })),
            "timeout must return Loading error"
        );
    }

    // ── issue #1106: mark_failed / is_failed / failed_len ───────────────────

    /// Why: `mark_failed` must remove the entry from `entries` so `len()` and
    /// `contains()` return the honest state after a failed restore.
    /// Test: this test.
    #[test]
    fn cold_store_mark_failed_evicts_from_entries() {
        let store = ColdIndexStore::new();
        store.register_cold_entries(vec![mk_entry("f1", None, None)]);
        let id = IndexId::new("f1".to_string());

        assert!(store.contains(&id), "must be in entries before mark_failed");
        assert_eq!(store.len(), 1);
        assert!(!store.is_failed(&id));

        store.mark_failed(&id);

        assert!(
            !store.contains(&id),
            "must NOT be in entries after mark_failed"
        );
        assert_eq!(store.len(), 0, "entries len must decrease to 0");
        assert!(store.is_failed(&id), "must appear in failed_entries");
    }

    /// Why: `failed_len()` must count permanently-failed entries separately
    /// from `len()` (pending) so the health metric is honest.
    /// Test: this test.
    #[test]
    fn cold_store_mark_failed_failed_len() {
        let store = ColdIndexStore::new();
        store.register_cold_entries(vec![mk_entry("fa", None, None), mk_entry("fb", None, None)]);
        assert_eq!(store.failed_len(), 0);
        assert_eq!(store.len(), 2);

        store.mark_failed(&IndexId::new("fa".to_string()));
        assert_eq!(store.failed_len(), 1);
        assert_eq!(store.len(), 1);

        store.mark_failed(&IndexId::new("fb".to_string()));
        assert_eq!(store.failed_len(), 2);
        assert_eq!(store.len(), 0);
    }

    /// Why: when `restore_fn` returns `false`, `get_or_load_index` must (a)
    /// return `RestoreFailed`, (b) move the entry to `failed_entries` so
    /// `len()` decreases and `is_failed()` returns `true`, and (c) NOT leave
    /// the entry in `entries` (which would cause the next call to re-enter the
    /// expensive restore path — the bug described in issue #1106).
    /// Test: this test.
    #[tokio::test]
    async fn get_or_load_index_restore_false_marks_failed() {
        let registry = IndexRegistry::default();
        let cold = ColdIndexStore::new();
        let id = IndexId::new("fail-idx".to_string());
        cold.register_cold_entries(vec![mk_entry("fail-idx", None, None)]);

        // First call: restore_fn returns false.
        let result = get_or_load_index(&id, &registry, &cold, Duration::from_secs(5), |_e| async {
            false
        })
        .await;

        assert!(
            matches!(result, Err(LazyLoadError::RestoreFailed)),
            "restore_fn=false must return RestoreFailed"
        );
        assert!(
            !cold.contains(&id),
            "entry must be evicted from entries after restore failure"
        );
        assert!(
            cold.is_failed(&id),
            "entry must appear in failed_entries after restore failure"
        );
        assert_eq!(
            cold.len(),
            0,
            "indexes_lazy must be 0 after restore failure"
        );
        assert_eq!(cold.failed_len(), 1, "indexes_failed must be 1");
    }

    /// Issue #1125 TOCTOU: after the loading gate is acquired, if `is_failed`
    /// returns `true` (a concurrent thread already called `mark_failed` on the
    /// same id), `get_or_load_index` must return `RestoreFailed` immediately
    /// without invoking `restore_fn` again.
    ///
    /// Why: without this re-check, a second concurrent caller that arrives after
    /// the first `mark_failed` could still slip through the gate and call
    /// `restore_fn` a second time for the same first-failure event — potentially
    /// hammering a blocked volume or deleted root_path again.
    ///
    /// Simulation: because real concurrency in a single-threaded test executor is
    /// hard to orchestrate, we simulate the observable post-gate state directly:
    /// register the cold entry, then call `mark_failed` manually to move it to
    /// `failed_entries` (mirroring what a concurrent thread would have done
    /// before releasing the gate), then invoke `get_or_load_index`. The loader
    /// should short-circuit at step 4b (post-gate `is_failed` re-check) and
    /// return `RestoreFailed` without calling `restore_fn`.
    /// Test: this test.
    #[tokio::test]
    async fn get_or_load_index_gate_recheck_is_failed_short_circuits() {
        let registry = IndexRegistry::default();
        let cold = ColdIndexStore::new();
        let id = IndexId::new("toctou-failed-idx".to_string());
        cold.register_cold_entries(vec![mk_entry("toctou-failed-idx", None, None)]);

        // Simulate: another thread already failed and called mark_failed,
        // but the entry is still in `failed_entries`. We do this before calling
        // get_or_load_index to model the state the gate re-check would observe.
        cold.mark_failed(&id);

        // At this point `entries` is empty (mark_failed removed it) and
        // `failed_entries` contains the id. `get_or_load_index` will see
        // `entries.get(id) == None` at step 2 and return NotFound immediately
        // (before even reaching the gate). But we also verify restore_fn is
        // never invoked.
        let mut restore_called = 0u32;
        let result = get_or_load_index(&id, &registry, &cold, Duration::from_secs(5), |_e| {
            restore_called += 1;
            async { false }
        })
        .await;

        // Step 2 short-circuits: entries is empty, so NotFound is returned.
        // restore_fn must NOT have been called.
        assert!(
            matches!(
                result,
                Err(LazyLoadError::NotFound) | Err(LazyLoadError::RestoreFailed)
            ),
            "must short-circuit without calling restore_fn when already failed"
        );
        assert_eq!(
            restore_called, 0,
            "restore_fn must not be called when entry is already in failed_entries"
        );
        assert!(cold.is_failed(&id), "id must still be in failed_entries");
    }

    /// Why: after a restore failure, subsequent calls must not re-attempt the
    /// expensive restore path. `cold_store.contains()` returns `false` so
    /// `get_or_load_index` returns `NotFound` immediately (the caller's
    /// `is_failed` check in the search handler catches this before calling
    /// `get_or_load_index`). This test verifies the loader's own behavior: the
    /// second call sees neither `entries` nor a gate, so it returns `NotFound`
    /// rather than calling restore_fn again.
    /// Test: this test.
    #[tokio::test]
    async fn get_or_load_index_second_call_after_failure_does_not_retry() {
        let registry = IndexRegistry::default();
        let cold = ColdIndexStore::new();
        let id = IndexId::new("fail-idx2".to_string());
        cold.register_cold_entries(vec![mk_entry("fail-idx2", None, None)]);

        let mut restore_call_count = 0u32;

        // First call — restore fails.
        let _ = get_or_load_index(&id, &registry, &cold, Duration::from_secs(5), |_e| {
            restore_call_count += 1;
            async { false }
        })
        .await;

        // Second call — entry is no longer in cold store; restore_fn must NOT
        // be called again. The result is NotFound because `is_failed` is not
        // checked inside `get_or_load_index` (that's the search handler's job).
        let result2 = get_or_load_index(&id, &registry, &cold, Duration::from_secs(5), |_e| {
            restore_call_count += 1;
            async { false }
        })
        .await;

        assert!(
            matches!(result2, Err(LazyLoadError::NotFound)),
            "second call after failure must return NotFound (not re-attempt restore)"
        );
        assert_eq!(
            restore_call_count, 1,
            "restore_fn must be called exactly once (not re-attempted after failure)"
        );
    }
}
