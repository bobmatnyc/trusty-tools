//! `ColdIndexStore` — in-memory registry of deferred ("cold") indexes (#993).
//!
//! Why: indexes not in the top-N by recency are parked here at startup instead
//! of being loaded into `IndexRegistry`. The double-checked-lock pattern (via
//! per-index `Mutex<()>`) prevents concurrent double-loads of the same index.
//! What: two `DashMap`s — one for persisted metadata, one for loading gates.
//! Test: `cold_store_*` tests in the parent module's `tests` block.

use std::sync::Arc;

use dashmap::DashMap;

use crate::core::registry::IndexId;
use crate::service::persistence::{warmboot_sort_key, PersistedIndex};

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
/// Test: `select_warmboot_entries_*` in the parent module's `tests` block.
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
///
/// Why (issue #1106): when `restore_fn` returns `false` (blocked volume,
/// missing root_path), the entry must be moved out of `entries` into
/// `failed_entries` so that (a) `indexes_lazy` only counts genuinely-restorable
/// pending indexes, (b) repeated queries for the same permanently-failed index
/// skip the expensive restore path and return a fast error, and (c) callers can
/// distinguish "not yet loaded" from "restore permanently failed".
///
/// What: three `DashMap`s — one for pending metadata (`entries`), one for
/// per-index loading gates, and one for permanently-failed index ids
/// (`failed_entries`). `len()` counts only `entries` (pending). `failed_len()`
/// counts `failed_entries`.
/// Test: `cold_store_*` tests in the parent module's `tests` block;
///       `cold_store_mark_failed_*` tests for the issue #1106 paths.
#[derive(Clone, Default)]
pub struct ColdIndexStore {
    /// Persisted metadata for each cold index, keyed by `IndexId`.
    pub(crate) entries: Arc<DashMap<IndexId, PersistedIndex>>,
    /// Per-index mutex preventing concurrent double-loads.
    loading_gates: Arc<DashMap<IndexId, Arc<tokio::sync::Mutex<()>>>>,
    /// Permanently-failed entries (issue #1106): indexes whose `restore_fn`
    /// returned `false`. These are evicted from `entries` so `len()` and
    /// `indexes_lazy` stay honest. Presence here signals "do not retry".
    ///
    /// Why: the value is `()` — we only need set semantics (O(1) membership
    /// test). A `DashMap<IndexId, ()>` gives that without an extra `HashSet`.
    /// What: populated by `mark_failed`; checked by `is_failed`.
    /// Test: `cold_store_mark_failed_*` unit tests.
    failed_entries: Arc<DashMap<IndexId, ()>>,
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
    /// unknown index or a not-yet-loaded cold index. Returns `false` for
    /// permanently-failed entries (issue #1106) so callers do not re-enter the
    /// expensive restore path.
    /// What: O(1) DashMap lookup on `entries` only (not `failed_entries`).
    /// Test: `cold_store_register_and_contains`.
    pub fn contains(&self, id: &IndexId) -> bool {
        self.entries.contains_key(id)
    }

    /// True when `id` has previously failed to restore (issue #1106).
    ///
    /// Why: distinguishes "not registered at all" from "registered but
    /// permanently unrestorable". Callers use this to return a fast 503
    /// (`index_restore_failed`) without re-entering the expensive restore path.
    /// What: O(1) DashMap lookup on `failed_entries`.
    /// Test: `cold_store_mark_failed_is_failed` unit test.
    pub fn is_failed(&self, id: &IndexId) -> bool {
        self.failed_entries.contains_key(id)
    }

    /// Total number of cold (not-yet-loaded) entries.
    ///
    /// Why: reported on `GET /health` as `indexes_lazy` so operators can see how
    /// many indexes are still pending their first load. Does NOT include
    /// permanently-failed entries (issue #1106) — those are counted separately
    /// by `failed_len()` so the metric stays honest.
    /// What: `DashMap::len()` on `entries` only.
    /// Test: `cold_store_len`.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True when no entries remain PENDING their first load.
    ///
    /// Why: cheap O(1) check used by callers that want to know whether the cold
    /// store has been drained of pending entries.
    /// What: checks only `entries` (pending). Does NOT account for permanently-
    /// failed entries — those are absent from `entries` and already counted
    /// separately by `failed_len()`. In other words, `is_empty()` returning
    /// `true` does NOT imply `failed_len() == 0`; it only means every registered
    /// cold entry has either been successfully loaded (via `mark_loaded`) or
    /// permanently failed (via `mark_failed`).
    /// Test: `cold_store_register_and_contains` and `cold_store_mark_failed_*`.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Number of indexes that permanently failed to restore (issue #1106).
    ///
    /// Why: reported on `GET /health` as `indexes_failed` so operators can
    /// distinguish "pending lazy load" from "restore permanently failed" —
    /// e.g. blocked volume or deleted root_path. Before this fix both appeared
    /// as "lazy pending", making the metric misleading.
    /// What: `DashMap::len()` on `failed_entries`.
    /// Test: `cold_store_mark_failed_failed_len` unit test.
    pub fn failed_len(&self) -> usize {
        self.failed_entries.len()
    }

    /// Count how many cold entries are in the provided id set.
    ///
    /// Why: `global_search_handler` (PR #1103) needs to count cold indexes
    /// that a restricted fan-out caller requested but that were skipped because
    /// they are not yet hot. Providing a method here keeps the caller from
    /// iterating `entries` directly and coupling to the internal DashMap type.
    /// What: O(|ids|) DashMap lookups.
    /// Test: exercised by `test_global_search_surfaces_cold_indexes_skipped`.
    pub fn count_matching<I>(&self, ids: I) -> usize
    where
        I: IntoIterator,
        I::Item: AsRef<str>,
    {
        ids.into_iter()
            .filter(|s| self.entries.contains_key(&IndexId::new(s.as_ref())))
            .count()
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

    /// Record that a cold index permanently failed to restore (issue #1106).
    ///
    /// Why: when `restore_fn` returns `false` (blocked volume, deleted
    /// root_path, or panic→false), the entry must be evicted from `entries`
    /// so that (a) `len()` / `indexes_lazy` decrements and stays honest, (b)
    /// the search handler's `cold_store.contains()` returns `false` preventing
    /// it from re-entering the expensive restore path, and (c) callers can
    /// detect the failure via `is_failed()` and return a fast, accurate 503.
    ///
    /// Policy: failure is permanent for the daemon's lifetime. If the underlying
    /// cause is transient (e.g. a volume that was temporarily unmounted), the
    /// operator should restart the daemon or use `POST /indexes` to re-register
    /// the index. This is conservative but safe: it prevents unbounded restore
    /// retry storms on every query.
    ///
    /// What: moves the id from `entries` to `failed_entries`; also removes the
    /// loading gate so it can be reclaimed.
    /// Test: `cold_store_mark_failed_*` and
    ///       `get_or_load_index_restore_false_marks_failed` unit tests.
    pub fn mark_failed(&self, id: &IndexId) {
        self.entries.remove(id);
        self.loading_gates.remove(id);
        self.failed_entries.insert(id.clone(), ());
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
