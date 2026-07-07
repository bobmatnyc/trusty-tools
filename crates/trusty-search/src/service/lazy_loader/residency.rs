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
/// **In-flight-cold-load guard (found by QA against an earlier revision of
/// this function — the fix below closes it):** `get_or_load_index`
/// (`loader.rs`) has an unavoidable internal gap between `restore_fn`
/// resolving — at which point the freshly-built handle is ALREADY registered
/// into the hot `registry` — and its own `cold_store.mark_loaded(id)` call,
/// which is what finally clears `id` from `cold_store.entries`. `id` is
/// therefore present in BOTH stores for the whole duration of that gap. If
/// this function parked `id` during that window, `register_cold_entries`
/// would re-insert a cold entry, `remove_and_get` would detach the
/// just-loaded handle, and then the racing loader's `mark_loaded(id)` would
/// remove the very entry we just added — orphaning `id` in NEITHER store and
/// turning the racing query's success into a spurious `NotFound`. The guard:
/// bail out immediately (`return false`, nothing touched) whenever `id` is
/// still present in `cold_store` at call time — that membership is the exact,
/// already-existing tell for "a cold-load has this id's entry claimed and
/// hasn't finished settling it" (a purely resident, settled id is never a
/// member of `cold_store.entries`). Parking is an optimisation; skipping one
/// id for one sweep tick is always safe — the next tick tries again once the
/// load has settled.
///
/// Reverse ordering (a load starting while a park is mid-flight): between
/// this function's step 1 (`register_cold_entries`) and step 2
/// (`remove_and_get`), `id` is a member of BOTH stores, never neither. A
/// concurrent `get_or_load_index` call either (a) reads the hot registry
/// before step 2 runs and returns the still-valid old handle via its normal
/// fast path — step 2 having not yet run, nothing races; or (b) reads the hot
/// registry after step 2 has removed it, finds `None`, then finds the cold
/// entry step 1 already installed, and proceeds through the ordinary
/// cold-load path. Because a call can only reach `get_or_load_index`'s cold
/// branch once the hot registry lookup has ALREADY returned `None`, a fresh
/// load can never observe `id` as "neither" either. Combined with the
/// in-flight guard above, `id` is provably in at least one of {hot registry,
/// cold store} at every observable instant of any park/load interleaving.
///
/// The caller (the residency-sweep ticker) is responsible for stopping the
/// index's file watcher after a successful park — see
/// `service::server::tickers::run_residency_sweep_tick` for why that is a
/// deliberate, documented choice rather than something this primitive does.
///
/// What: (0) bail out with `false` if `id` is already in `cold_store`
/// (in-flight load guard, see above); (1) `cold_store.register_cold_entries([entry])`;
/// (2) `registry.remove_and_get(id)`. Returns `true` when an index was
/// actually resident and got parked. Returns `false` — and rolls back the
/// cold-store registration — when the id had already been removed by a
/// concurrent delete / orphan-reap (benign race: nothing to park, and we must
/// not leave a stray, unloadable cold entry for an index that no longer
/// exists).
/// Test: `cold_park_index_moves_hot_to_cold`,
/// `cold_park_index_absent_returns_false_and_leaves_no_stray_entry`,
/// `cold_park_index_never_orphans_a_racing_cold_load`; full disk-round-trip
/// coverage in `tests/residency_cold_park.rs`.
pub async fn cold_park_index(
    id: &IndexId,
    registry: &IndexRegistry,
    cold_store: &ColdIndexStore,
    entry: PersistedIndex,
) -> bool {
    // 0. In-flight-cold-load guard: `id` still being a member of `cold_store`
    //    means a concurrent `get_or_load_index` call has this id's cold entry
    //    claimed and has not yet reached `mark_loaded` — see the function doc
    //    for the full race analysis. Skip; the next sweep tick retries once
    //    the load has settled.
    if cold_store.contains(id) {
        return false;
    }
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

    /// Deterministic (synchronization-based, not timing-based) reproduction
    /// of the orphan race a QA pass caught against an earlier revision of
    /// `cold_park_index`: a residency-sweep park landing in the gap between
    /// `get_or_load_index`'s `restore_fn` registering the freshly-loaded
    /// handle into the hot registry and its own `mark_loaded` call clearing
    /// the id from the cold store.
    ///
    /// Why deterministic rather than timing-based: `restore_fn` is `await`ed
    /// synchronously by `get_or_load_index` — injecting the racing
    /// `cold_park_index` call INSIDE the closure, after it registers the
    /// handle but before it returns, reproduces the exact interleaving on
    /// every run with no `sleep`/`yield_now` and no flakiness.
    ///
    /// What: seeds a cold entry, then drives `get_or_load_index` with a
    /// `restore_fn` that (a) registers the handle — mirroring
    /// `restore_index_on_demand` — then (b) calls `cold_park_index` for the
    /// SAME id right there, mid-load. Asserts: the racing park must refuse
    /// (returns `false`, the in-flight guard); the original load must still
    /// resolve `Ok` (never a spurious `NotFound`); and afterward the id is in
    /// EXACTLY one of {hot registry, cold store} — never neither, never both.
    /// Test: this test (issue #2161 QA follow-up).
    #[tokio::test]
    async fn cold_park_index_never_orphans_a_racing_cold_load() {
        let registry = IndexRegistry::default();
        let cold = ColdIndexStore::new();
        let id = IndexId::new("race-load-park".to_string());
        cold.register_cold_entries(vec![mk_entry("race-load-park", Some(1))]);

        let registry_for_restore = registry.clone();
        let cold_for_restore = cold.clone();
        let id_for_restore = id.clone();

        let result = crate::service::lazy_loader::get_or_load_index(
            &id,
            &registry,
            &cold,
            std::time::Duration::from_secs(5),
            move |restored_entry| {
                let registry_for_restore = registry_for_restore.clone();
                let cold_for_restore = cold_for_restore.clone();
                let id_for_restore = id_for_restore.clone();
                async move {
                    // Mirror `restore_index_on_demand`: register the handle
                    // into the hot registry BEFORE `get_or_load_index` has
                    // had a chance to call `mark_loaded`. `restored_entry`
                    // still lives in `cold_store.entries` at this exact
                    // instant — this is the race window under test.
                    registry_for_restore.register(build_mock_handle("race-load-park"));

                    // The residency sweep races in here, mid-load, and tries
                    // to park the very id that is being loaded right now.
                    let parked = cold_park_index(
                        &id_for_restore,
                        &registry_for_restore,
                        &cold_for_restore,
                        restored_entry,
                    )
                    .await;
                    assert!(
                        !parked,
                        "cold_park_index must refuse an id with an in-flight cold-load"
                    );

                    true
                }
            },
        )
        .await;

        assert!(
            result.is_ok(),
            "the racing load must still succeed — never a spurious NotFound"
        );

        let hot = registry.get(&id).is_some();
        let is_cold = cold.contains(&id);
        assert!(
            hot ^ is_cold,
            "after the race the id must be in EXACTLY one of {{hot, cold}} — \
             hot={hot} is_cold={is_cold}"
        );
        assert!(
            hot,
            "the load won the race and must leave the index resident, not cold"
        );
    }
}
