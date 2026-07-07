//! Usage-based resident-index cap: env knobs + the non-destructive cold-park
//! primitive that lets a periodic sweep bound aggregate memory across every
//! registered index (issue #2161).
//!
//! Why: `TRUSTY_WARMBOOT_MAX_INDEXES` (issue #993) only bounds how many
//! indexes are loaded EAGERLY at boot — once an index is queried it stays
//! resident forever, so a daemon serving a long tail of occasionally-used
//! projects still accumulates unbounded RSS over its lifetime. This module
//! adds a runtime counterpart: a background sweep ranks currently-resident
//! indexes by the same recency key used at boot and cold-parks everything
//! beyond the top `TRUSTY_MAX_RESIDENT_INDEXES`, reusing the existing
//! cold-store + `get_or_load_index` machinery for the reload.
//!
//! What: `max_resident_indexes()` / `residency_sweep_secs()` (env readers),
//! `ids_to_park()` (pure selection — reuses [`select_warmboot_entries`]'s
//! comparator so boot-time and runtime ranking can never diverge), and
//! `cold_park_index()` (the non-destructive detach-and-park primitive). The
//! periodic sweep itself lives in `service::server::tickers` because it needs
//! `SearchAppState` (registry, cold store, reindex-progress guard, watcher
//! manager); this module stays free of that dependency so it is unit-testable
//! in isolation.
//!
//! Test: `max_resident_indexes_*`, `residency_sweep_secs_*`, `ids_to_park_*`,
//! `cold_park_index_*` below; end-to-end round-trip coverage lives in
//! `tests/residency_cold_park.rs`.

use crate::core::registry::{IndexId, IndexRegistry};
use crate::service::persistence::PersistedIndex;

use super::store::{select_warmboot_entries, ColdIndexStore};

/// Read the usage-based resident-index cap from `TRUSTY_MAX_RESIDENT_INDEXES`.
///
/// Why: operators with a long tail of rarely-used registered indexes want a
/// hard ceiling on how many stay loaded in memory at once, independent of how
/// many were eagerly warm-booted. Unset preserves the pre-#2161 behaviour —
/// no runtime eviction — so upgrading the daemon never surprises an operator
/// who never opted in.
/// What: parses the env var as a `usize`. Unset → `None` (disabled, back-compat
/// default). `0` → `Some(0)` (park every resident index on the next sweep,
/// mirroring `TRUSTY_WARMBOOT_MAX_INDEXES=0` semantics). A parse failure is
/// logged and treated as `None`.
/// Test: `max_resident_indexes_unset_returns_none`,
/// `max_resident_indexes_parses_env`, `max_resident_indexes_invalid_falls_back`.
pub fn max_resident_indexes() -> Option<usize> {
    let raw = std::env::var("TRUSTY_MAX_RESIDENT_INDEXES").ok()?;
    match raw.trim().parse::<usize>() {
        Ok(n) => Some(n),
        Err(e) => {
            tracing::warn!(
                "TRUSTY_MAX_RESIDENT_INDEXES={raw:?} is not a valid usize ({e}); \
                 residency sweep stays disabled"
            );
            None
        }
    }
}

/// Default interval (seconds) between residency-cap sweeps.
const DEFAULT_RESIDENCY_SWEEP_SECS: u64 = 120;

/// Read the residency-sweep interval from `TRUSTY_RESIDENCY_SWEEP_SECS`.
///
/// Why: the sweep is a background cost (one `indexes.toml` read plus a
/// registry walk per tick) independent of whether the cap is even enabled;
/// operators may want a tighter or looser cadence than the 2-minute default.
/// What: reads `TRUSTY_RESIDENCY_SWEEP_SECS` as `u64` seconds. `0` disables
/// the ticker outright (it never spawns). Unset / unparseable falls back to
/// [`DEFAULT_RESIDENCY_SWEEP_SECS`].
/// Test: `residency_sweep_secs_default_and_env_override`.
pub fn residency_sweep_secs() -> u64 {
    match std::env::var("TRUSTY_RESIDENCY_SWEEP_SECS") {
        Ok(v) if !v.is_empty() => match v.trim().parse::<u64>() {
            Ok(n) => n,
            Err(_) => {
                tracing::warn!(
                    "TRUSTY_RESIDENCY_SWEEP_SECS={v:?} is not a valid u64; \
                     using default ({DEFAULT_RESIDENCY_SWEEP_SECS}s)"
                );
                DEFAULT_RESIDENCY_SWEEP_SECS
            }
        },
        _ => DEFAULT_RESIDENCY_SWEEP_SECS,
    }
}

/// Select which currently-resident entries should be cold-parked this sweep.
///
/// Why: extracted so the sweep ticker (`service::server::tickers`) stays a
/// thin orchestration wrapper and the ranking decision is independently unit
/// testable without a real `SearchAppState`. Reuses
/// [`select_warmboot_entries`] — the exact comparator `start.rs` uses to
/// decide the boot-time eager/cold split — so "keep the hottest N" means the
/// same thing at boot and at runtime.
/// What: `resident_entries` is the `PersistedIndex` snapshot (from
/// `indexes.toml`) for every index currently in the hot `IndexRegistry`.
/// Returns the subset (beyond the top `cap` by recency) that should be
/// parked; empty when `resident_entries.len() <= cap` (nothing to do).
/// Test: `ids_to_park_keeps_top_n_by_recency`, `ids_to_park_empty_when_under_cap`.
pub fn ids_to_park(resident_entries: Vec<PersistedIndex>, cap: usize) -> Vec<PersistedIndex> {
    if resident_entries.len() <= cap {
        return Vec::new();
    }
    let (_keep_resident, park) = select_warmboot_entries(resident_entries, Some(cap));
    park
}

/// Non-destructively evict a resident index from the hot registry and park it
/// in the cold store so a subsequent query reloads it via `get_or_load_index`
/// (issue #2161).
///
/// Why: `IndexRegistry::unregister` / `remove_and_get` are otherwise only ever
/// paired with PERMANENT deletion (`delete_index_handler`, the orphan-reaper
/// ticker) — that path also scrubs `indexes.toml`, scrubs `roots.toml`, and
/// destroys the on-disk data directory. The residency cap needs a distinct
/// "detach from memory only, everything on disk stays put" operation so a
/// periodic sweep can bound aggregate RSS without the operator losing the
/// registration. This function does ONLY the detach — it never touches
/// `indexes.toml`, `roots.toml`, or any on-disk artifact.
///
/// Ordering matters for correctness: `entry` is registered into `cold_store`
/// **before** the handle is removed from `registry`, so there is no window in
/// which a concurrent query would see the index as neither hot nor cold
/// (which would otherwise produce a spurious 404 instead of a lazy reload).
///
/// Concurrency: if a query races this call and wins — it fetched the handle
/// from `registry.get` before we remove it — that query finishes normally
/// against the `Arc` it already holds (`IndexRegistry::remove_and_get`'s own
/// contract guarantees in-flight readers are safe). If a query arrives after
/// the removal, `get_or_load_index` finds the entry in the cold store
/// (registered in step 1) and reloads it through the existing cold-load path.
/// Worst case is one redundant reload — never a lost or corrupted index.
///
/// The caller (the residency-sweep ticker) is responsible for stopping the
/// index's file watcher after a successful park — see
/// `service::server::tickers::run_residency_sweep_tick` for why that is a
/// deliberate, documented choice rather than something this primitive does.
///
/// What: (1) `cold_store.register_cold_entries([entry])`; (2)
/// `registry.remove_and_get(id)`. Returns `true` when an index was actually
/// resident and got parked. Returns `false` — and rolls back the cold-store
/// registration — when the id had already been removed by a concurrent
/// delete / orphan-reap (benign race: nothing to park, and we must not leave
/// a stray, unloadable cold entry for an index that no longer exists).
/// Test: `cold_park_index_moves_hot_to_cold`,
/// `cold_park_index_absent_returns_false_and_leaves_no_stray_entry`; full
/// disk-round-trip coverage in `tests/residency_cold_park.rs`.
pub async fn cold_park_index(
    id: &IndexId,
    registry: &IndexRegistry,
    cold_store: &ColdIndexStore,
    entry: PersistedIndex,
) -> bool {
    // 1. Make the index discoverable as "cold" FIRST — closes the gap where a
    //    concurrent query would otherwise see it in neither store.
    cold_store.register_cold_entries(vec![entry]);
    // 2. Atomically detach the live handle. In-flight readers holding the old
    //    Arc finish safely (see `IndexRegistry::remove_and_get`'s own doc).
    let (removed, _handle) = registry.remove_and_get(id);
    if !removed {
        // Lost a race with a concurrent delete/orphan-reap: undo the cold
        // registration we just added so a genuinely-gone index doesn't linger
        // as an orphaned, permanently-unloadable cold entry.
        cold_store.mark_loaded(id);
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::Arc;
    use tokio::sync::RwLock;

    use crate::core::registry::IndexHandle;

    fn mk_entry(id: &str, q: Option<u64>) -> PersistedIndex {
        PersistedIndex {
            id: id.to_string(),
            root_path: PathBuf::from(format!("/tmp/{id}")),
            last_queried_unix: q,
            ..Default::default()
        }
    }

    fn build_mock_handle(id: &str) -> IndexHandle {
        let index_id = IndexId::new(id.to_string());
        let root_path = PathBuf::from(format!("/tmp/test-residency-{id}"));
        let indexer = Arc::new(RwLock::new(crate::core::indexer::CodeIndexer::new(
            id, &root_path,
        )));
        IndexHandle::bare(index_id, indexer, root_path)
    }

    // ── max_resident_indexes ─────────────────────────────────────────────

    #[test]
    #[serial_test::serial]
    fn max_resident_indexes_unset_returns_none() {
        unsafe { std::env::remove_var("TRUSTY_MAX_RESIDENT_INDEXES") };
        assert!(max_resident_indexes().is_none());
    }

    #[test]
    #[serial_test::serial]
    fn max_resident_indexes_parses_env() {
        unsafe { std::env::set_var("TRUSTY_MAX_RESIDENT_INDEXES", "5") };
        assert_eq!(max_resident_indexes(), Some(5));
        unsafe { std::env::remove_var("TRUSTY_MAX_RESIDENT_INDEXES") };
    }

    #[test]
    #[serial_test::serial]
    fn max_resident_indexes_invalid_falls_back() {
        unsafe { std::env::set_var("TRUSTY_MAX_RESIDENT_INDEXES", "not-a-number") };
        assert!(max_resident_indexes().is_none());
        unsafe { std::env::remove_var("TRUSTY_MAX_RESIDENT_INDEXES") };
    }

    // ── residency_sweep_secs ─────────────────────────────────────────────

    #[test]
    #[serial_test::serial]
    fn residency_sweep_secs_default_and_env_override() {
        unsafe { std::env::remove_var("TRUSTY_RESIDENCY_SWEEP_SECS") };
        assert_eq!(residency_sweep_secs(), DEFAULT_RESIDENCY_SWEEP_SECS);

        unsafe { std::env::set_var("TRUSTY_RESIDENCY_SWEEP_SECS", "30") };
        assert_eq!(residency_sweep_secs(), 30);

        unsafe { std::env::set_var("TRUSTY_RESIDENCY_SWEEP_SECS", "0") };
        assert_eq!(residency_sweep_secs(), 0, "0 must disable, not fall back");

        unsafe { std::env::remove_var("TRUSTY_RESIDENCY_SWEEP_SECS") };
    }

    // ── ids_to_park ───────────────────────────────────────────────────────

    #[test]
    fn ids_to_park_empty_when_under_cap() {
        let entries = vec![mk_entry("a", Some(1)), mk_entry("b", Some(2))];
        assert!(ids_to_park(entries, 5).is_empty());
    }

    #[test]
    fn ids_to_park_keeps_top_n_by_recency() {
        // a: 300 (hottest), b: 200, c: 100 (coldest) — cap=2 must park "c".
        let entries = vec![
            mk_entry("a", Some(300)),
            mk_entry("b", Some(200)),
            mk_entry("c", Some(100)),
        ];
        let parked = ids_to_park(entries, 2);
        assert_eq!(parked.len(), 1);
        assert_eq!(parked[0].id, "c");
    }

    #[test]
    fn ids_to_park_cap_zero_parks_everything() {
        let entries = vec![mk_entry("a", Some(1)), mk_entry("b", Some(2))];
        let parked = ids_to_park(entries, 0);
        assert_eq!(parked.len(), 2);
    }

    // ── cold_park_index ──────────────────────────────────────────────────

    #[tokio::test]
    async fn cold_park_index_moves_hot_to_cold() {
        let registry = IndexRegistry::default();
        let cold = ColdIndexStore::new();
        let id = IndexId::new("hot-1".to_string());
        registry.register(build_mock_handle("hot-1"));
        assert!(registry.get(&id).is_some());

        let parked = cold_park_index(&id, &registry, &cold, mk_entry("hot-1", Some(1))).await;

        assert!(parked, "resident index must be parked");
        assert!(
            registry.get(&id).is_none(),
            "index must be detached from the hot registry"
        );
        assert!(
            cold.contains(&id),
            "index must be discoverable in the cold store after park"
        );
    }

    #[tokio::test]
    async fn cold_park_index_absent_returns_false_and_leaves_no_stray_entry() {
        let registry = IndexRegistry::default();
        let cold = ColdIndexStore::new();
        let id = IndexId::new("never-registered".to_string());

        let parked =
            cold_park_index(&id, &registry, &cold, mk_entry("never-registered", None)).await;

        assert!(!parked, "an id that was never resident cannot be parked");
        assert!(
            !cold.contains(&id),
            "a failed park must not leave a stray, unloadable cold entry"
        );
    }

    #[tokio::test]
    async fn cold_park_index_registers_cold_before_detaching() {
        // Regression guard for the ordering invariant documented on
        // `cold_park_index`: even though we can't observe the exact
        // interleaving in a single-threaded await, we can assert the
        // POST-STATE invariant that both operations completed and the id
        // is never simultaneously absent from both stores by re-deriving
        // membership right after the call.
        let registry = IndexRegistry::default();
        let cold = ColdIndexStore::new();
        let id = IndexId::new("order-1".to_string());
        registry.register(build_mock_handle("order-1"));

        cold_park_index(&id, &registry, &cold, mk_entry("order-1", Some(42))).await;

        let hot = registry.get(&id).is_some();
        let is_cold = cold.contains(&id);
        assert!(
            hot || is_cold,
            "index must be discoverable in exactly one of hot/cold after park, never neither"
        );
        assert!(!hot && is_cold, "park must leave the index cold, not hot");
    }
}
