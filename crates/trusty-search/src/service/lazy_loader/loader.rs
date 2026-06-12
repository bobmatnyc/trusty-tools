//! `get_or_load_index` — hot-path resolver with lazy cold-index loading (#993).
//!
//! Why: all per-index HTTP handlers need to resolve an `IndexHandle`. With
//! selective warm-boot, the handle may not be in the hot `IndexRegistry` yet.
//! This module implements the full load-on-demand flow without exposing the
//! double-checked-lock details to callers.
//! What: one async function (`get_or_load_index`) and one error type
//! (`LazyLoadError`). Generic over the restore function so tests inject fakes.
//!
//! PR #1103 TOCTOU fix: between the `entries.get(id)` cold-check (step 2) and
//! `loading_gate(id)` (step 3), a concurrent `mark_loaded` can remove the entry
//! from the cold store so `loading_gate` returns `None`. The previous code
//! returned `LazyLoadError::NotFound` in that case — a spurious 404 for an
//! index that just became hot. The fix: when `loading_gate` returns `None`,
//! re-check the hot registry; if the index is now there, return it (the
//! concurrent load raced us and won).
//!
//! Test: `get_or_load_index_*` in the parent module's `tests` block;
//!       `get_or_load_index_gate_none_but_index_just_became_hot` for the race path.

use std::sync::Arc;
use std::time::Duration;

use crate::core::registry::{IndexId, IndexRegistry};
use crate::service::persistence::PersistedIndex;

use super::store::ColdIndexStore;

/// Look up an index from the hot registry, loading it lazily if it is cold.
///
/// Why (issue #993): all per-index HTTP handlers need to resolve a handle.
/// With lazy warm-boot, the handle may not be in the hot registry yet. This
/// helper implements the full load-on-demand flow: (1) hot fast-path via
/// `registry.get(id)`; (2) cold check — `NotFound` if absent from both stores;
/// (3) acquire per-index loading gate; if gate returns `None` (concurrent
/// `mark_loaded` raced us), re-check hot registry and return it or `NotFound`;
/// (4a) re-check hot registry after gate acquired; (4b) re-check `is_failed`
/// after gate acquired — if a concurrent thread just called `mark_failed(id)`,
/// short-circuit with `RestoreFailed` instead of calling `restore_fn` a second
/// time for the same first-failure event (TOCTOU fix, issue #1125); (5) load
/// via `restore_fn(entry)` inside `tokio::time::timeout`; (6) `mark_loaded(id)`;
/// (7) return `Err(LazyLoadError::Loading)` on timeout for `503 index_loading`.
///
/// Issue #1106: when `restore_fn` returns `false` (blocked volume, missing
/// root_path), call `cold_store.mark_failed(id)` to evict the entry from
/// `entries` (so `indexes_lazy` decreases and `contains()` returns `false`)
/// and return `LazyLoadError::RestoreFailed` instead of re-returning `Loading`.
/// Subsequent calls for the same id go through the `cold_store.contains()`
/// guard in the search handler, which returns `false` for failed entries,
/// causing the handler to return 404 (which is acceptable — the index exists
/// in the registry sense but cannot be served). Callers that need to
/// distinguish "truly unknown" from "restore failed" should additionally check
/// `cold_store.is_failed(id)`.
///
/// What: generic over the restore function so tests can inject a fake restore.
///
/// Test: `get_or_load_index_hot_path`, `get_or_load_index_loads_cold_index`,
/// `get_or_load_index_returns_loading_on_timeout`,
/// `get_or_load_index_gate_none_but_index_just_became_hot`,
/// `get_or_load_index_restore_false_marks_failed`,
/// `get_or_load_index_gate_recheck_is_failed_short_circuits`.
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
    //
    // PR #1103 TOCTOU: between step 2 and here, a concurrent `mark_loaded(id)`
    // may have removed the entry from the cold store, so `loading_gate` returns
    // `None`. That means the index just became hot — re-check the registry
    // before returning NotFound.
    let gate = match cold_store.loading_gate(id) {
        Some(g) => g,
        None => {
            // The cold entry vanished: a concurrent load raced us and won.
            // If the index is now in the hot registry, return it. Only if it
            // is absent from both places is this a genuine NotFound.
            return registry.get(id).ok_or(LazyLoadError::NotFound);
        }
    };
    let _guard = gate.lock().await;

    // 4a. Re-check hot registry after acquiring the gate.
    if let Some(handle) = registry.get(id) {
        return Ok(handle);
    }

    // 4b. Re-check failed set after acquiring the gate.
    //
    // TOCTOU fix (issue #1125): between step 2 (cold-store lookup) and here,
    // a concurrent thread may have been inside `restore_fn`, had it return
    // `false`, and called `mark_failed(id)`. That thread now holds the gate
    // before us (step 3 serializes them), and by the time we acquire it the
    // entry has already been moved to `failed_entries`. Without this re-check
    // we would call `restore_fn` a second time for the same first-failure
    // event — potentially hitting a blocked volume or deleted root_path twice.
    // Mirroring the hot-registry re-check in step 4a, we short-circuit here.
    if cold_store.is_failed(id) {
        return Err(LazyLoadError::RestoreFailed);
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
        // Issue #1106: evict the entry from the cold store immediately so that
        // (a) `indexes_lazy` decreases and stays honest, (b) subsequent queries
        // skip the expensive restore path, and (c) the health metric correctly
        // reflects this as a failed index rather than a pending one.
        //
        // Policy: failure is permanent for the daemon's lifetime — the operator
        // must restart the daemon or re-register the index to retry. This avoids
        // unbounded restore storms caused by a blocked volume or missing root_path.
        cold_store.mark_failed(id);
        tracing::warn!(
            "lazy-load: index '{}' restore returned false (blocked volume, \
             missing root_path, or panic) — marking permanently failed and \
             returning 503 (issue #1106)",
            id.0
        );
        return Err(LazyLoadError::RestoreFailed);
    }

    // 6. Mark loaded and return handle.
    cold_store.mark_loaded(id);

    registry.get(id).ok_or(LazyLoadError::NotFound)
}

/// Error returned by [`get_or_load_index`].
///
/// Why: callers need to distinguish a genuine 404 (unknown id) from a
/// transient 503 (cold index still loading / timed out) and a permanent 503
/// (cold index restore failed — issue #1106).
/// What: three variants — `NotFound` (emit 404), `Loading` (emit 503 with
/// `retry_after_secs` — transient), and `RestoreFailed` (emit 503 — permanent,
/// `restore_fn` returned `false`; operator must restart or re-register).
/// Test: variant-level assertions in `get_or_load_index_*` tests.
#[derive(Debug)]
pub enum LazyLoadError {
    /// The index id is not in the hot registry, not in the cold store, and not
    /// in the failed-entries set. Genuine unknown index — emit 404.
    NotFound,
    /// The index was found in the cold store but timed out before loading.
    /// Transient: the caller may retry after `retry_after_secs`.
    Loading { retry_after_secs: u64 },
    /// The index was found in the cold store but `restore_fn` returned `false`
    /// (blocked volume, deleted root_path, or panic — issue #1106).
    /// Permanent for the daemon's lifetime: the operator must restart the
    /// daemon or re-register the index. The cold store entry has already been
    /// moved to `failed_entries` so this variant is returned on subsequent
    /// calls without re-attempting the restore.
    RestoreFailed,
}
