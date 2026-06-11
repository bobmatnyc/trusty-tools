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
/// (4) re-check hot registry after gate acquired; (5) load via
/// `restore_fn(entry)` inside `tokio::time::timeout`; (6) `mark_loaded(id)`;
/// (7) return `Err(LazyLoadError::Loading)` on timeout for `503 index_loading`.
///
/// What: generic over the restore function so tests can inject a fake restore.
///
/// Test: `get_or_load_index_hot_path`, `get_or_load_index_loads_cold_index`,
/// `get_or_load_index_returns_loading_on_timeout`,
/// `get_or_load_index_gate_none_but_index_just_became_hot`.
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
