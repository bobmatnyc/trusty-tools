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
//!   - `ColdIndexStore` — a `DashMap` of `IndexId → PersistedIndex` for indexes
//!     that were discovered at startup but deferred. A `DashMap<IndexId, Mutex<()>>`
//!     prevents concurrent double-loads of the same index (double-checked-lock).
//!   - `warmboot_max_indexes()` — reads `TRUSTY_WARMBOOT_MAX_INDEXES`.
//!   - `cold_reload_timeout()` — reads `TRUSTY_INDEX_COLD_RELOAD_TIMEOUT_SECS`.
//!   - `select_warmboot_entries()` — splits a list of entries into (eager, cold).
//!   - `get_or_load_index()` — hot-path helper: tries the registry first, falls
//!     back to lazy-load from cold store if the index is registered-but-cold.
//!
//! Back-compat: when `TRUSTY_WARMBOOT_MAX_INDEXES` is unset, `select_warmboot_entries`
//! returns all entries as eager and the cold store is empty — exact same behaviour
//! as the pre-#993 daemon.
//!
//! Test: `select_warmboot_entries_*`, `cold_reload_timeout_parses_env`,
//!       `warmboot_max_indexes_parses_env`, `get_or_load_index_*`.

use std::sync::Arc;
use std::time::Duration;

use dashmap::DashMap;

use crate::core::registry::{IndexId, IndexRegistry};
use crate::service::persistence::{warmboot_sort_key, PersistedIndex};

/// Minimum number of seconds that must elapse before `last_queried_unix` is
/// persisted again for the same index (rate-limiting the write to avoid
/// excessive TOML rewrites on hot indexes).
///
/// Why: if every search query wrote to `indexes.toml`, a busy index would
/// generate constant disk I/O. 60 s is the same cadence as the BM25/chunk
/// idle-eviction ticker, which is already an accepted background write rate.
/// What: compared against `SystemTime::now()` in the search handler.
/// Test: covered indirectly — the guard prevents double-writes within the window.
pub const LAST_QUERIED_WRITE_INTERVAL_SECS: u64 = 60;

/// Read the maximum number of indexes to warm-boot eagerly from the env var
/// `TRUSTY_WARMBOOT_MAX_INDEXES` (issue #993).
///
/// Why: operators with 100+ registered indexes can bound the startup time by
/// capping how many are loaded at boot. Cold indexes are loaded on first query.
/// What: parses the env var as a `usize`. Unset → `None` (warm-boot all,
/// back-compat default); `0` → `Some(0)` (lazy-load everything); `N` →
/// `Some(N)` (warm-boot top-N most-recently-used). A parse failure is logged
/// and treated as `None` (fallback to warm-boot-all).
/// Test: `warmboot_max_indexes_parses_env`.
pub fn warmboot_max_indexes() -> Option<usize> {
    let raw = std::env::var("TRUSTY_WARMBOOT_MAX_INDEXES").ok()?;
    match raw.trim().parse::<usize>() {
        Ok(n) => Some(n),
        Err(e) => {
            tracing::warn!(
                "TRUSTY_WARMBOOT_MAX_INDEXES={raw:?} is not a valid usize ({e}); \
                 falling back to warm-boot-all"
            );
            None
        }
    }
}

/// Per-query lazy-load deadline from `TRUSTY_INDEX_COLD_RELOAD_TIMEOUT_SECS`.
///
/// Why: loading a cold index from disk can take several seconds (redb open +
/// HNSW snapshot read). We enforce a timeout so a query against a not-yet-loaded
/// index doesn't hang indefinitely — instead it returns a `503 index_loading`
/// response with a `retry_after_secs` field.
/// What: parses `TRUSTY_INDEX_COLD_RELOAD_TIMEOUT_SECS` as a positive `u64`.
/// Falls back to 30 s on parse failure or if the variable is unset.
/// `0` is treated as the default (zero-second timeouts are not useful).
/// Test: `cold_reload_timeout_parses_env`.
pub fn cold_reload_timeout() -> Duration {
    let secs = std::env::var("TRUSTY_INDEX_COLD_RELOAD_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|&s| s > 0)
        .unwrap_or(30);
    Duration::from_secs(secs)
}

/// Split `entries` into `(eager, cold)` based on `TRUSTY_WARMBOOT_MAX_INDEXES`.
///
/// Why: the warm-boot loop in `start.rs` calls `restore_indexes` for the eager
/// slice and registers the cold slice into `ColdIndexStore` without loading them.
/// What: when `max_n` is `None` (env var unset), all entries are eager and the
/// cold list is empty (back-compat). When `max_n == Some(0)`, all entries go
/// cold. Otherwise the top-N most-recently-used entries are eager (sort key:
/// `max(last_queried_unix, last_indexed_unix)` descending; ties break by id
/// ascending so the split is deterministic across restarts).
/// The sort is stable: entries with the same sort key keep their original order
/// within the sorted group, then id-alpha tie-break.
/// Test: `select_warmboot_entries_*` in this module.
pub fn select_warmboot_entries(
    entries: Vec<PersistedIndex>,
    max_n: Option<usize>,
) -> (Vec<PersistedIndex>, Vec<PersistedIndex>) {
    let Some(n) = max_n else {
        // Back-compat: no cap → all eager, nothing cold.
        return (entries, Vec::new());
    };

    if n == 0 {
        return (Vec::new(), entries);
    }

    if entries.len() <= n {
        // All fit within the cap — nothing goes cold.
        return (entries, Vec::new());
    }

    // Sort descending by recency sort key, then ascending by id for tie-break.
    let mut sorted = entries;
    sorted.sort_by(|a, b| {
        let ka = warmboot_sort_key(a);
        let kb = warmboot_sort_key(b);
        kb.cmp(&ka).then_with(|| a.id.cmp(&b.id))
    });

    let cold = sorted.split_off(n);
    (sorted, cold)
}

/// In-memory registry of cold (not-yet-loaded) indexes.
///
/// Why (issue #993): indexes not in the top-N by recency are parked here at
/// startup. On first access via `get_or_load_index`, one background task loads
/// the index into the hot `IndexRegistry`. The per-index `Mutex<()>` prevents
/// concurrent double-loads.
/// What: a `DashMap<IndexId, PersistedIndex>` for the metadata, and a matching
/// `DashMap<IndexId, Arc<tokio::sync::Mutex<()>>>` for the per-index loading
/// gate (double-checked-lock pattern).
/// Test: `get_or_load_index_*` tests in this module.
#[derive(Clone, Default)]
pub struct ColdIndexStore {
    /// Persisted metadata for each cold index, keyed by `IndexId`.
    pub(crate) entries: Arc<DashMap<IndexId, PersistedIndex>>,
    /// Per-index mutex preventing concurrent double-loads.
    loading_gates: Arc<DashMap<IndexId, Arc<tokio::sync::Mutex<()>>>>,
}

impl ColdIndexStore {
    /// Why: zero-arg constructor for default state construction.
    /// What: creates empty DashMaps; no disk I/O.
    /// Test: `ColdIndexStore::default()`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a batch of cold entries at daemon startup.
    ///
    /// Why: `restore_indexes` calls this once with the "cold" slice returned by
    /// `select_warmboot_entries` so the store is populated before any query arrives.
    /// What: inserts each entry under its `IndexId`. Idempotent (re-insert replaces).
    /// Test: `cold_store_register_and_contains`.
    pub fn register_cold_entries(&self, entries: Vec<PersistedIndex>) {
        for entry in entries {
            let id = IndexId::new(entry.id.clone());
            self.entries.insert(id, entry);
        }
    }

    /// True when `id` is in the cold store (registered but not yet loaded).
    ///
    /// Why: `get_or_load_index` uses this to decide whether a 404 is a genuine
    /// unknown index or a not-yet-loaded cold index.
    /// What: O(1) DashMap lookup.
    /// Test: `cold_store_register_and_contains`.
    pub fn contains(&self, id: &IndexId) -> bool {
        self.entries.contains_key(id)
    }

    /// Total number of cold (not-yet-loaded) entries.
    ///
    /// Why: reported on `GET /health` as `indexes_lazy` so operators can see how
    /// many indexes are still pending their first load.
    /// What: `DashMap::len()`.
    /// Test: `cold_store_len`.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True when no cold entries remain (all have been lazily loaded).
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Remove a cold entry after it has been successfully loaded into the hot registry.
    ///
    /// Why: once the index is in `IndexRegistry`, future `get_or_load_index` calls
    /// hit the hot-path branch and the cold entry is no longer needed.
    /// What: removes the entry and its loading gate.
    /// Test: exercised by `get_or_load_index_loads_cold_index`.
    pub fn mark_loaded(&self, id: &IndexId) {
        self.entries.remove(id);
        self.loading_gates.remove(id);
    }

    /// Acquire or create the per-index loading gate.
    ///
    /// Why: double-checked lock — two concurrent queries for the same cold index
    /// must not both try to restore it simultaneously. The first acquires the
    /// Mutex; the second blocks until the first finishes, then re-checks the hot
    /// registry and returns immediately.
    /// What: inserts a fresh `Arc<Mutex<()>>` if not already present; returns the
    /// existing one otherwise. Returns `None` when the id is not in the cold store.
    /// Test: exercised by concurrent-load tests.
    pub fn loading_gate(&self, id: &IndexId) -> Option<Arc<tokio::sync::Mutex<()>>> {
        if !self.entries.contains_key(id) {
            return None;
        }
        Some(
            self.loading_gates
                .entry(id.clone())
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
                .clone(),
        )
    }
}

/// Look up an index from the hot registry, loading it lazily if it is cold.
///
/// Why (issue #993): all per-index HTTP handlers need to resolve a handle.
/// With lazy warm-boot, the handle may not be in the hot registry yet. This
/// helper implements the full load-on-demand flow: (1) hot fast-path via
/// `registry.get(id)`; (2) cold check; (3) acquire per-index loading gate;
/// (4) re-check hot registry; (5) load via `restore_fn(entry)` inside
/// `tokio::time::timeout`; (6) `mark_loaded(id)` on success; (7) return
/// `Err(LazyLoadError::Loading)` on timeout for `503 index_loading`.
///
/// What: generic over the restore function so tests can inject a fake restore.
///
/// Test: `get_or_load_index_hot_path`, `get_or_load_index_loads_cold_index`,
/// `get_or_load_index_returns_loading_on_timeout`.
pub async fn get_or_load_index<F, Fut>(
    id: &IndexId,
    registry: &IndexRegistry,
    cold_store: &ColdIndexStore,
    timeout: Duration,
    restore_fn: F,
) -> Result<Arc<crate::core::registry::IndexHandle>, LazyLoadError>
where
    F: FnOnce(PersistedIndex) -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    // 1. Hot fast-path.
    if let Some(handle) = registry.get(id) {
        return Ok(handle);
    }

    // 2. Cold check: if not in cold store either, it's a genuine 404.
    let entry = match cold_store.entries.get(id).map(|r| r.clone()) {
        Some(e) => e,
        None => return Err(LazyLoadError::NotFound),
    };

    // 3. Acquire loading gate (prevent double-load).
    let gate = match cold_store.loading_gate(id) {
        Some(g) => g,
        None => return Err(LazyLoadError::NotFound),
    };
    let _guard = gate.lock().await;

    // 4. Re-check hot registry after acquiring the gate.
    if let Some(handle) = registry.get(id) {
        return Ok(handle);
    }

    // 5. Load with timeout.
    tracing::info!(
        "lazy-load: index '{}' not yet warm-booted — loading on demand (issue #993)",
        id.0
    );
    let loaded = match tokio::time::timeout(timeout, restore_fn(entry)).await {
        Ok(success) => success,
        Err(_elapsed) => {
            tracing::warn!(
                "lazy-load: index '{}' timed out after {:.0}s — returning 503 (issue #993)",
                id.0,
                timeout.as_secs_f32()
            );
            return Err(LazyLoadError::Loading {
                retry_after_secs: timeout.as_secs(),
            });
        }
    };

    if !loaded {
        tracing::warn!(
            "lazy-load: index '{}' restore returned false (blocked volume or panic) \
             — returning 503 (issue #993)",
            id.0
        );
        return Err(LazyLoadError::Loading {
            retry_after_secs: timeout.as_secs(),
        });
    }

    // 6. Mark loaded and return handle.
    cold_store.mark_loaded(id);

    registry.get(id).ok_or(LazyLoadError::NotFound)
}

/// Error returned by [`get_or_load_index`].
///
/// Why: callers need to distinguish a genuine 404 (unknown id) from a
/// transient 503 (cold index still loading / timed out).
/// What: two variants — `NotFound` (emit 404) and `Loading` (emit 503 with
/// `retry_after_secs`).
/// Test: variant-level assertions in `get_or_load_index_*` tests.
#[derive(Debug)]
pub enum LazyLoadError {
    /// The index id is not in the hot registry and not in the cold store.
    NotFound,
    /// The index was found in the cold store but timed out or failed to load.
    Loading { retry_after_secs: u64 },
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

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
        use crate::core::registry::IndexRegistry;

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
        use crate::core::registry::IndexRegistry;

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
}
