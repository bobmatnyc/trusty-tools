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
//! What: `resolve_max_resident_indexes_for()` / `residency_sweep_secs()` (the
//! cap and cadence readers), `ids_to_park()` (pure selection — reuses
//! [`select_warmboot_entries`]'s comparator so boot-time and runtime ranking
//! can never diverge), and `cold_park_index()` (the non-destructive
//! detach-and-park primitive). The periodic sweep itself lives in
//! `service::server::tickers` because it needs `SearchAppState` (registry, cold
//! store, reindex-progress guard, watcher manager); this module stays free of
//! that dependency so it is unit-testable in isolation.
//!
//! What changed in #6821: an unset `TRUSTY_MAX_RESIDENT_INDEXES` used to
//! disable the cap outright, so every index that was ever queried stayed
//! resident for the daemon's lifetime (56 indexes / 15 GB on the reporting
//! host). It now resolves to a machine-tier default. `off` is the new spelling
//! that disables the cap; every numeric value, `0` included, means exactly what
//! it meant before.
//!
//! Test: `default_max_resident_indexes_*`, `resolve_max_resident_indexes_*`,
//! `warmboot_cap_*`, `residency_sweep_secs_*`, `ids_to_park_*`,
//! `cold_park_index_*` below; end-to-end round-trip coverage lives in
//! `tests/residency_cold_park.rs`.

use std::sync::Arc;

use crate::core::memory_policy::{detect_total_ram_mb, MemoryTier};
use crate::core::registry::{IndexId, IndexRegistry};
use crate::service::persistence::PersistedIndex;

use super::store::{select_warmboot_entries, ColdIndexStore};

/// The one env value that turns the resident-index cap off entirely (#6821).
const RESIDENT_CAP_OFF: &str = "off";

/// Where the resolved resident-index cap came from (#6821).
///
/// Why: with a tier-scaled default in play, the number alone no longer says
/// whether an operator chose it. `/health` and the startup log report both, so
/// "why is this host parking indexes" is answerable without reading the
/// daemon's environment.
/// What: three states — an env-supplied number, the env `off` spelling, or the
/// machine-tier default. An unparseable env value resolves to the tier default
/// and reports itself as such, because the daemon did not honour what was
/// written.
/// Test: `resolve_max_resident_indexes_reports_env_source`,
/// `resolve_max_resident_indexes_off_disables_the_cap`,
/// `resolve_max_resident_indexes_invalid_falls_back_to_the_tier_default`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResidentCapSource {
    /// `TRUSTY_MAX_RESIDENT_INDEXES` named a number.
    Env,
    /// `TRUSTY_MAX_RESIDENT_INDEXES=off` — no index is ever cold-parked.
    EnvOff,
    /// Unset, empty, or unparseable — the machine tier chose the number.
    TierDefault,
}

impl ResidentCapSource {
    /// Stable wire spelling for `/health` and the startup log.
    pub fn as_str(self) -> &'static str {
        match self {
            ResidentCapSource::Env => "env",
            ResidentCapSource::EnvOff => "env (off)",
            ResidentCapSource::TierDefault => "tier default",
        }
    }
}

/// The resolved resident-index cap plus the evidence behind it (#6821).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResidentIndexCap {
    /// How many indexes may stay resident. `None` means the cap is off and
    /// nothing is ever cold-parked at runtime.
    pub cap: Option<usize>,
    /// Which of the three inputs produced [`Self::cap`].
    pub source: ResidentCapSource,
    /// The machine tier the default was (or would have been) read from.
    pub tier: MemoryTier,
}

/// How many indexes a host of this tier keeps resident by default (#6821).
///
/// Why: the residency mechanism shipped in #2161 and then sat disabled, so a
/// 128 GB host held 56 indexes and 15 GB of heap with 35 of them never queried.
/// A cap has to default to a number for the mechanism to do anything, and that
/// number has to track the machine — 16 resident indexes is fine on 128 GB and
/// ruinous on 12 GB.
/// What: the tier bands are `trusty_common::machine_tier`'s (#6820). Each
/// resident index costs a redb handle, an HNSW view, and whatever chunk/BM25
/// cache has not been idle-evicted yet. `Degraded` stays at 2 because that host
/// cannot absorb a second working set; the bands above it step by 4 rather than
/// doubling, because a 24 GB host holding 8 indexes is the working posture the
/// owner ruled for and doubling from there overshoots what the larger bands buy.
/// A parked index is not lost — the next query reloads it through the same cold
/// path a never-warm-booted index uses.
/// Test: `default_max_resident_indexes_scales_with_tier`.
pub fn default_max_resident_indexes(tier: MemoryTier) -> usize {
    // #6821: owner ruling 2026-09-06, Medium 8-12
    match tier {
        MemoryTier::Degraded => 2,
        MemoryTier::Medium => 8,
        MemoryTier::Large => 12,
        MemoryTier::XLarge => 16,
    }
}

/// Resolve the resident-index cap against a known machine tier (#6821).
///
/// Why: the tier is a hardware fact that costs a `sysctl` spawn to read, so
/// every caller that resolves the cap more than once — the residency sweep on
/// every tick, `/health` on every poll — passes the tier it already holds
/// (`SearchAppState::machine_tier`) instead of re-detecting it. The env read
/// stays per-call so `TRUSTY_MAX_RESIDENT_INDEXES` is still toggleable through
/// `daemon.env` without a restart, exactly as #2161 documented.
/// What: precedence is env number > env `off` > tier default. An unset or empty
/// value takes the tier default. `off` (any case) disables the cap. A numeric
/// value is honoured verbatim, `0` included — `0` still means "park every
/// resident index on the next sweep", the #2161 meaning, which is why `off` had
/// to be a separate spelling. An unparseable value is warned about and takes the
/// tier default rather than silently disabling the cap.
/// Test: `resolve_max_resident_indexes_unset_takes_the_tier_default`,
/// `resolve_max_resident_indexes_reports_env_source`,
/// `resolve_max_resident_indexes_zero_still_parks_everything`,
/// `resolve_max_resident_indexes_off_disables_the_cap`,
/// `resolve_max_resident_indexes_invalid_falls_back_to_the_tier_default`.
pub fn resolve_max_resident_indexes_for(tier: MemoryTier) -> ResidentIndexCap {
    let fallback = default_max_resident_indexes(tier);
    let tier_default = ResidentIndexCap {
        cap: Some(fallback),
        source: ResidentCapSource::TierDefault,
        tier,
    };
    let Ok(raw) = std::env::var("TRUSTY_MAX_RESIDENT_INDEXES") else {
        return tier_default;
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return tier_default;
    }
    if trimmed.eq_ignore_ascii_case(RESIDENT_CAP_OFF) {
        return ResidentIndexCap {
            cap: None,
            source: ResidentCapSource::EnvOff,
            tier,
        };
    }
    match trimmed.parse::<usize>() {
        Ok(n) => ResidentIndexCap {
            cap: Some(n),
            source: ResidentCapSource::Env,
            tier,
        },
        Err(e) => {
            // #6821: falling back to the tier default rather than to "disabled"
            // — a typo must not silently restore the unbounded pre-#6821 growth.
            tracing::warn!(
                "TRUSTY_MAX_RESIDENT_INDEXES={raw:?} is neither a usize nor \
                 \"{RESIDENT_CAP_OFF}\" ({e}); using the {tier} tier default \
                 ({fallback})"
            );
            tier_default
        }
    }
}

/// Resolve the resident-index cap, detecting the machine tier first (#6821).
///
/// Why: for the handful of callers that have no `SearchAppState` to read the
/// tier from — the startup log, and `max_resident_indexes()` below.
/// What: [`resolve_max_resident_indexes_for`] against
/// `MemoryTier::from_total_ram_mb(detect_total_ram_mb())`, degrading to
/// `Degraded` when RAM cannot be read (the same posture `MachineBudget::detect`
/// takes). Spawns `sysctl` on macOS — never call it on a hot path.
/// Test: `resolve_max_resident_indexes_unset_takes_the_tier_default`.
pub fn resolve_max_resident_indexes() -> ResidentIndexCap {
    let tier = detect_total_ram_mb().map_or(MemoryTier::Degraded, MemoryTier::from_total_ram_mb);
    resolve_max_resident_indexes_for(tier)
}

/// The resolved resident-index cap as a bare `Option` (#2161, #6821).
///
/// `None` means the cap is off. Detects the tier — see
/// [`resolve_max_resident_indexes`] for the cost note.
/// Test: `resolve_max_resident_indexes_unset_takes_the_tier_default`.
pub fn max_resident_indexes() -> Option<usize> {
    resolve_max_resident_indexes().cap
}

/// How many indexes warm-boot may load eagerly (#993, capped by #6821).
///
/// Why: warm-boot and the residency sweep bounded different things and only one
/// of them had a default, so a daemon eagerly loaded every registered index at
/// boot and then relied on the sweep to park them back down 120 s later — the
/// peak the epic's measurement pass flagged as the real risk. Inheriting the
/// resident cap means the boot never reaches that peak in the first place.
/// What: `TRUSTY_WARMBOOT_MAX_INDEXES` when set (`0` included), otherwise the
/// resolved resident cap for `tier`. `None` — reachable only via
/// `TRUSTY_MAX_RESIDENT_INDEXES=off` with no warm-boot cap set — keeps the
/// pre-#993 warm-boot-everything behaviour. The ordering is
/// [`select_warmboot_entries`]'s, so the kept slice is the most-recently-used
/// N and a never-queried, never-indexed entry (sort key `0`) is always in the
/// deferred remainder.
/// Test: `warmboot_cap_prefers_the_explicit_env_var`,
/// `warmboot_cap_falls_back_to_the_resident_cap`,
/// `warmboot_cap_keeps_the_mru_slice_and_defers_never_used_entries`.
pub fn warmboot_cap_for(tier: MemoryTier) -> Option<usize> {
    super::env::warmboot_max_indexes().or_else(|| resolve_max_resident_indexes_for(tier).cap)
}

/// Log the resolved resident-index cap once, at daemon startup (#6821).
///
/// Test: `resolve_max_resident_indexes_reports_env_source` covers the values
/// this renders; the log line itself has no assertable behaviour.
pub fn log_resident_index_cap(tier: MemoryTier) {
    let resolved = resolve_max_resident_indexes_for(tier);
    match resolved.cap {
        Some(n) => tracing::info!(
            "resident-index cap: {n} (source={}, tier={}) — the residency sweep \
             cold-parks everything beyond this; set TRUSTY_MAX_RESIDENT_INDEXES=off \
             to disable",
            resolved.source.as_str(),
            resolved.tier
        ),
        None => tracing::info!(
            "resident-index cap: off (source={}, tier={}) — no index is ever \
             cold-parked at runtime",
            resolved.source.as_str(),
            resolved.tier
        ),
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
/// (in-flight load guard, see above); (0.5) snapshot the handle currently
/// registered under `id` as `expected` — see "Concurrent-write guard" below;
/// (0.75) [`persist_before_park`] saves the vector store (#6870); (1)
/// `cold_store.register_cold_entries([entry])`; (2)
/// `registry.remove_and_get(id)`, compared by `Arc::ptr_eq` against
/// `expected`. Returns `true` when an index was actually resident and got
/// parked. Returns `false` in three distinct cases. The first mutates nothing:
/// (0) the save failed, so the park is abandoned before it starts and the next
/// sweep retries. The other two roll back the cold-store registration: (a) the
/// id had already been removed by a concurrent delete / orphan-reap (benign
/// race: nothing to park); (b) a concurrent create/relocate/reindex-override
/// swapped in a DIFFERENT handle under this id — see below. In both of those we
/// must not leave a stray, unloadable cold entry for an id whose live
/// registration is either gone or no longer what we thought it was.
///
/// **Concurrent-write guard (issue #3995 round 4, HIGH; identity-guarded
/// reap added round 5, CRITICAL):** `create_index_handler`,
/// `relocate_index_handler`, and `reindex_handler`'s `root_path` override all
/// call `cold_store.mark_loaded_if` immediately after each registers a FRESH
/// `IndexHandle` under `id` (issue #3993 round 3, reaping a stale cold-store
/// record left over from before the id was re-created). That makes each of
/// them a SECOND, uncoordinated writer of `cold_store.entries` — racing this
/// function's own step 1. Since steps 1–2 here are two synchronous `DashMap`
/// operations with no `.await` between them, the only way a writer can land
/// its register+reap pair inside that window is genuine multi-thread
/// parallelism (this function's caller — the residency-sweep ticker — is not
/// itself racing anyone; a concurrent HTTP handler on a different tokio
/// worker thread is).
///
/// Round 4 closed the REGISTRY side of this race with the `expected` snapshot
/// and `Arc::ptr_eq` check below, but left the COLD-STORE side of the same
/// handlers' cleanup as an unconditional `mark_loaded(id)` — round 5 review
/// proved (by execution, not just paper reasoning — see
/// `cold_park_index_restores_concurrently_swapped_handle_instead_of_orphaning`)
/// that this was still enough to orphan `id` in 2 of 10 possible
/// interleavings: when a handler's `register` lands BEFORE this function's
/// `expected` snapshot even runs, the identity check below trivially matches
/// (see the residual paragraph below) and this function's step 2 "succeeds"
/// without rolling back its own step-1 insertion — so whichever of {this
/// function's step 1, the handler's reap} runs LAST wins the cold-store slot.
/// If the handler's reap runs last, it blindly deletes the entry THIS
/// function just legitimately inserted, having no way to tell it apart from
/// the stale leftover it meant to clean up — `id` ends up in neither the hot
/// registry (removed by this function's step 2) nor the cold store (reaped
/// by the handler), an unrecoverable orphan.
///
/// The fix: every writer that reaps a cold entry it does not itself own
/// outright first captures `cold_store.entry_token(id)` — an opaque identity
/// token, `Arc::ptr_eq`-comparable exactly like a registry handle — BEFORE
/// its own register/insert, then reaps via `mark_loaded_if(id, that_token)`
/// afterward instead of the unconditional `mark_loaded`. This function
/// itself follows the identical discipline for its OWN insertion: `own_token`
/// (captured from `register_cold_entries`'s return value at step 1) is what
/// `mark_loaded_if` rolls back with below, so it only ever removes the entry
/// IT inserted — never a handler's freshly-registered stale-record reap that
/// happened to land on the same id in between. Worked example of the bug this
/// closes: (1) this function's step 1 parks a stale snapshot of `id` into
/// `cold_store`, capturing `own_token`; (2) a concurrent relocate registers a
/// brand-new live handle for `id` at a new root, snapshots
/// `cold_store.entry_token(id)` (observing THIS function's freshly-inserted
/// entry, not its own old one), then calls `mark_loaded_if` with that
/// snapshot — sees the entry is still the one it just observed, reaps it, and
/// in doing so removes what actually was this function's entry (a legitimate,
/// visible outcome of losing the race, not an orphan — see the residual
/// paragraph); (3) this function's step 2 unconditionally removes whatever is
/// now live under `id` — the relocate's fresh handle — but that path is
/// reached BY THE MISMATCH BRANCH, which calls `mark_loaded_if(id, own_token)`
/// to undo its own step-1 insertion: `own_token` no longer matches (the
/// relocate's reap already removed it, or a completely different entry now
/// sits there), so `mark_loaded_if` is a safe no-op rather than deleting
/// something it doesn't recognize. Either way `id` never lands in neither
/// store. This makes every interleaving of {this function's steps} and {a
/// concurrent handler's register + identity-guarded reap} resolve to `id`
/// present in at least one of {hot registry, cold store} — see the full
/// interleaving enumeration on `cold_park_index_restores_concurrently_swapped_handle_instead_of_orphaning`
/// below (and `collision_3993_tests.rs` for the handler-side reproduction).
///
/// One narrower, residual (non-orphaning) effect this guard does not — and
/// structurally cannot — close: if the concurrent write's `register` lands
/// BEFORE this function's `expected` snapshot even runs (e.g., mid-sweep,
/// before this specific id's turn in the sweep's per-id loop — a far wider
/// window than the two-op one above), `expected` itself observes the
/// writer's fresh handle, the identity check trivially matches, and the park
/// proceeds "successfully" with a stale `entry` (describing the id's OLD
/// root_path) — the id ends up parked cold under stale metadata rather than
/// live at its new location: a park that silently reverts a very-recent
/// relocate/reindex-override. This is a metadata-staleness issue, not a
/// reachability one — `id` stays discoverable (in exactly one of the two
/// stores) throughout, per the identity-guarded reap above — and narrowing it
/// further would require the sweep to re-read `indexes.toml` (or re-derive
/// `entry`) per-id at park time rather than once per tick, a broader change
/// than this fix's remit.
/// Test: `cold_park_index_moves_hot_to_cold`,
/// `cold_park_index_absent_returns_false_and_leaves_no_stray_entry`,
/// `cold_park_index_never_orphans_a_racing_cold_load`,
/// `cold_park_index_restores_concurrently_swapped_handle_instead_of_orphaning`,
/// `cold_park_index_handler_reap_guarded_before_park_never_orphans` (round 5);
/// full disk-round-trip coverage in `tests/residency_cold_park.rs`.
pub async fn cold_park_index(
    id: &IndexId,
    registry: &IndexRegistry,
    cold_store: &ColdIndexStore,
    entry: PersistedIndex,
) -> bool {
    cold_park_index_inner(id, registry, cold_store, entry, || {}).await
}

/// Test seam for [`cold_park_index`] (issue #3995 round 4 HIGH): `hook` runs
/// synchronously right after step 1 (`register_cold_entries`) and before
/// step 2 (`remove_and_get`) — the exact window a concurrent
/// create/relocate/reindex-override write can land in. Production code only
/// ever calls this via [`cold_park_index`], which passes a no-op `hook`
/// (`impl FnOnce()`, zero runtime cost when inlined). Tests use `hook` to
/// force the precise interleaving deterministically instead of relying on
/// real OS-thread scheduling to hit a multi-microsecond window — the same
/// need documented on [`cold_park_index`]'s "Concurrent-write guard" section.
/// Snapshot the index's vector store to disk before the park detaches it
/// (#6870).
///
/// Why: parking drops the last `Arc` to the `IndexHandle`, and with it a
/// heap-resident HNSW store holding writes that were never saved. The next
/// `get_or_load_index` rebuilds from the on-disk snapshot, so those writes are
/// gone — silently, with the index looking healthy. #2161 shipped the park with
/// the cap off by default, which made this rare; #6821 turns the cap on for
/// every host and makes it the ordinary path.
/// What: calls [`CodeIndexer::save_vector_store`] — the same path the shutdown
/// flush takes — against the entry's snapshot path, under the indexer read
/// lock. Returns `true` when the caller may proceed to detach: the save
/// succeeded, no store is wired (BM25-only), or the entry has no resolvable
/// snapshot path. Returns `false` on a save error so the caller parks nothing
/// and the next sweep retries; losing 120 s of residency headroom is cheaper
/// than losing the writes. A store already serving from its mmap view has
/// nothing unpersisted and `UsearchStore::save` short-circuits it.
///
/// This is a durable write to the index's OWN snapshot path, which the
/// non-destructive contract in [`cold_park_index`] already permits — it is what
/// the shutdown flush and the incremental persister write. `indexes.toml`,
/// `roots.toml`, and the redb corpus are still untouched.
/// Test: `cold_park_persists_a_dirty_vector_store_before_detaching` in
/// `tests/residency_cold_park.rs`.
async fn persist_before_park(
    id: &IndexId,
    handle: &Arc<crate::core::registry::IndexHandle>,
    entry: &PersistedIndex,
) -> bool {
    let hnsw_path = match crate::service::persistence::hnsw_path_for_entry(entry) {
        Ok(path) => path,
        Err(e) => {
            // No resolvable snapshot path — there is nowhere to save to, and
            // that is not a reason to keep the index resident forever.
            tracing::debug!(
                "residency-park: '{}' has no resolvable HNSW snapshot path ({e}) \
                 — parking (#6870)",
                id.0
            );
            return true;
        }
    };
    let started = std::time::Instant::now();
    let saved = handle
        .indexer
        .read()
        .await
        .save_vector_store(&hnsw_path)
        .await;
    match saved {
        Ok(true) => {
            let bytes = std::fs::metadata(&hnsw_path).map(|m| m.len()).unwrap_or(0);
            tracing::info!(
                "residency-park: saved '{}' HNSW snapshot before detaching \
                 ({bytes} bytes, {:?}) (#6870)",
                id.0,
                started.elapsed()
            );
            true
        }
        Ok(false) => {
            tracing::debug!(
                "residency-park: '{}' has no vector store to save — parking (#6870)",
                id.0
            );
            true
        }
        Err(e) => {
            tracing::warn!(
                "residency-park: refusing to park '{}' — its vector store could \
                 not be saved ({e}); parking now would drop unpersisted writes. \
                 The next sweep retries (#6870)",
                id.0
            );
            false
        }
    }
}

async fn cold_park_index_inner(
    id: &IndexId,
    registry: &IndexRegistry,
    cold_store: &ColdIndexStore,
    entry: PersistedIndex,
    hook: impl FnOnce(),
) -> bool {
    // 0. In-flight-cold-load guard: `id` still being a member of `cold_store`
    //    means a concurrent `get_or_load_index` call has this id's cold entry
    //    claimed and has not yet reached `mark_loaded` — see the function doc
    //    for the full race analysis. Skip; the next sweep tick retries once
    //    the load has settled.
    if cold_store.contains(id) {
        return false;
    }
    // 0.5. Snapshot the handle we intend to park (issue #3995 round 4 HIGH —
    //    see "Concurrent-write guard" on `cold_park_index`). `None` means the
    //    id has already been removed (concurrent delete/orphan-reap) —
    //    nothing to park.
    let Some(expected) = registry.get(id) else {
        return false;
    };
    // 0.75. #6870: persist before detaching. Nothing has been mutated yet, so a
    //    failed save is a clean no-op and the next sweep retries.
    if !persist_before_park(id, &expected, &entry).await {
        return false;
    }
    // 1. Make the index discoverable as "cold" FIRST — closes the gap where a
    //    concurrent query would otherwise see it in neither store. Capture
    //    the identity token this specific insertion is stamped with (issue
    //    #3995 round 5 CRITICAL) so any rollback below reaps precisely THIS
    //    entry — never a different one a concurrent write installs under the
    //    same id afterward (see `ColdIndexStore::mark_loaded_if`).
    let own_token = cold_store
        .register_cold_entries(vec![entry])
        .into_iter()
        .next();
    hook();
    // 2. Atomically detach the live handle. In-flight readers holding the old
    //    Arc finish safely (see `IndexRegistry::remove_and_get`'s own doc).
    let (_removed, handle) = registry.remove_and_get(id);
    match handle {
        Some(h) if Arc::ptr_eq(&h, &expected) => {
            // We parked the exact handle we observed at entry. Normal path.
            true
        }
        Some(h) => {
            // A concurrent create/relocate/reindex-override swapped in a
            // DIFFERENT handle under this id between our snapshot and the
            // removal above. Hand it straight back — identity-preserving, no
            // new Arc — and undo our own cold-store insertion (identity-
            // guarded: only removes it if it's still there unchanged) so
            // `id` is never left in neither store.
            registry.restore(h);
            cold_store.mark_loaded_if(id, own_token);
            false
        }
        None => {
            // Lost a race with a concurrent delete/orphan-reap: undo the cold
            // registration we just added (identity-guarded) so a genuinely-
            // gone index doesn't linger as an orphaned, permanently-unloadable
            // cold entry.
            cold_store.mark_loaded_if(id, own_token);
            false
        }
    }
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

    /// Every tier, so a table test cannot silently skip the variant a new
    /// `MemoryTier` band would add — the enum is deliberately not
    /// `#[non_exhaustive]` for exactly this reason (#6820).
    const ALL_TIERS: [MemoryTier; 4] = [
        MemoryTier::Degraded,
        MemoryTier::Medium,
        MemoryTier::Large,
        MemoryTier::XLarge,
    ];

    fn clear_cap_env() {
        unsafe { std::env::remove_var("TRUSTY_MAX_RESIDENT_INDEXES") };
        unsafe { std::env::remove_var("TRUSTY_WARMBOOT_MAX_INDEXES") };
    }

    // ── the tier-scaled default (#6821) ──────────────────────────────────

    /// The proposed table, pinned. A change here is a change to how much RAM a
    /// host of that size will hold, so it must be a deliberate edit.
    #[test]
    fn default_max_resident_indexes_scales_with_tier() {
        // #6821: owner ruling 2026-09-06, Medium 8-12
        assert_eq!(default_max_resident_indexes(MemoryTier::Degraded), 2);
        assert_eq!(default_max_resident_indexes(MemoryTier::Medium), 8);
        assert_eq!(default_max_resident_indexes(MemoryTier::Large), 12);
        assert_eq!(default_max_resident_indexes(MemoryTier::XLarge), 16);

        // Monotonic: a bigger machine never holds fewer indexes.
        for pair in ALL_TIERS.windows(2) {
            assert!(
                default_max_resident_indexes(pair[0]) < default_max_resident_indexes(pair[1]),
                "{:?} must allow fewer resident indexes than {:?}",
                pair[0],
                pair[1]
            );
        }
    }

    // ── resolve_max_resident_indexes_for (#6821) ─────────────────────────

    /// The #6821 behaviour change: unset used to disable the cap outright.
    #[test]
    #[serial_test::serial]
    fn resolve_max_resident_indexes_unset_takes_the_tier_default() {
        clear_cap_env();
        for tier in ALL_TIERS {
            let resolved = resolve_max_resident_indexes_for(tier);
            assert_eq!(
                resolved.cap,
                Some(default_max_resident_indexes(tier)),
                "an unset TRUSTY_MAX_RESIDENT_INDEXES must resolve to the {tier} default"
            );
            assert_eq!(resolved.source, ResidentCapSource::TierDefault);
            assert_eq!(resolved.tier, tier);
        }
        // The tier-detecting wrapper agrees with whatever tier this host is.
        assert!(
            max_resident_indexes().is_some(),
            "the cap must be on by default — that is the whole of #6821"
        );
    }

    /// An explicit number wins over the tier default on every tier.
    #[test]
    #[serial_test::serial]
    fn resolve_max_resident_indexes_reports_env_source() {
        clear_cap_env();
        unsafe { std::env::set_var("TRUSTY_MAX_RESIDENT_INDEXES", "5") };
        for tier in ALL_TIERS {
            let resolved = resolve_max_resident_indexes_for(tier);
            assert_eq!(
                resolved.cap,
                Some(5),
                "an explicit cap must win over the {tier} default"
            );
            assert_eq!(resolved.source, ResidentCapSource::Env);
            assert_eq!(resolved.source.as_str(), "env");
        }
        clear_cap_env();
    }

    /// `0` keeps its #2161 meaning — park everything — because the issue's
    /// contract is that a numeric value still "overrides exactly as today".
    #[test]
    #[serial_test::serial]
    fn resolve_max_resident_indexes_zero_still_parks_everything() {
        clear_cap_env();
        unsafe { std::env::set_var("TRUSTY_MAX_RESIDENT_INDEXES", "0") };
        let resolved = resolve_max_resident_indexes_for(MemoryTier::XLarge);
        assert_eq!(
            resolved.cap,
            Some(0),
            "0 must stay `Some(0)` — the sweep parks every resident index"
        );
        assert_eq!(resolved.source, ResidentCapSource::Env);
        clear_cap_env();
    }

    /// `off` is the one spelling that turns the cap off.
    #[test]
    #[serial_test::serial]
    fn resolve_max_resident_indexes_off_disables_the_cap() {
        clear_cap_env();
        for spelling in ["off", "OFF", " Off "] {
            unsafe { std::env::set_var("TRUSTY_MAX_RESIDENT_INDEXES", spelling) };
            let resolved = resolve_max_resident_indexes_for(MemoryTier::Medium);
            assert_eq!(
                resolved.cap, None,
                "{spelling:?} must disable the cap entirely"
            );
            assert_eq!(resolved.source, ResidentCapSource::EnvOff);
            assert_eq!(resolved.source.as_str(), "env (off)");
        }
        clear_cap_env();
    }

    /// A typo must not silently restore the unbounded pre-#6821 growth.
    #[test]
    #[serial_test::serial]
    fn resolve_max_resident_indexes_invalid_falls_back_to_the_tier_default() {
        clear_cap_env();
        for raw in ["not-a-number", "-1", "12.5"] {
            unsafe { std::env::set_var("TRUSTY_MAX_RESIDENT_INDEXES", raw) };
            let resolved = resolve_max_resident_indexes_for(MemoryTier::Large);
            assert_eq!(
                resolved.cap,
                Some(default_max_resident_indexes(MemoryTier::Large)),
                "{raw:?} must fall back to the tier default, not to disabled"
            );
            assert_eq!(resolved.source, ResidentCapSource::TierDefault);
        }
        // An empty value is "unset", not "off".
        unsafe { std::env::set_var("TRUSTY_MAX_RESIDENT_INDEXES", "") };
        assert_eq!(
            resolve_max_resident_indexes_for(MemoryTier::Large).source,
            ResidentCapSource::TierDefault
        );
        clear_cap_env();
    }

    // ── warm-boot inherits the cap (#6821) ───────────────────────────────

    #[test]
    #[serial_test::serial]
    fn warmboot_cap_prefers_the_explicit_env_var() {
        clear_cap_env();
        unsafe { std::env::set_var("TRUSTY_WARMBOOT_MAX_INDEXES", "3") };
        unsafe { std::env::set_var("TRUSTY_MAX_RESIDENT_INDEXES", "9") };
        assert_eq!(warmboot_cap_for(MemoryTier::XLarge), Some(3));

        // `0` is a choice, not an absence.
        unsafe { std::env::set_var("TRUSTY_WARMBOOT_MAX_INDEXES", "0") };
        assert_eq!(warmboot_cap_for(MemoryTier::XLarge), Some(0));
        clear_cap_env();
    }

    #[test]
    #[serial_test::serial]
    fn warmboot_cap_falls_back_to_the_resident_cap() {
        clear_cap_env();
        for tier in ALL_TIERS {
            assert_eq!(
                warmboot_cap_for(tier),
                Some(default_max_resident_indexes(tier)),
                "an unset warm-boot cap must inherit the {tier} resident default"
            );
        }
        // `off` on the resident cap restores warm-boot-everything.
        unsafe { std::env::set_var("TRUSTY_MAX_RESIDENT_INDEXES", "off") };
        assert_eq!(
            warmboot_cap_for(MemoryTier::Medium),
            None,
            "TRUSTY_MAX_RESIDENT_INDEXES=off must restore the pre-#6821 warm-boot"
        );
        clear_cap_env();
    }

    /// Acceptance (d): with M > N registered indexes and no env vars set, the
    /// warm-boot split keeps exactly N, most-recently-used first, and both
    /// never-used entries land in the deferred remainder.
    #[test]
    #[serial_test::serial]
    fn warmboot_cap_keeps_the_mru_slice_and_defers_never_used_entries() {
        clear_cap_env();
        // Degraded caps at 2; five entries, two of which were never touched.
        let entries = vec![
            mk_entry("never-a", None),
            mk_entry("cold", Some(100)),
            mk_entry("hottest", Some(300)),
            mk_entry("never-b", None),
            mk_entry("warm", Some(200)),
        ];
        let cap = warmboot_cap_for(MemoryTier::Degraded);
        assert_eq!(cap, Some(2));

        let (eager, deferred) = select_warmboot_entries(entries, cap);
        let eager_ids: Vec<&str> = eager.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(
            eager_ids,
            vec!["hottest", "warm"],
            "exactly the top-2 by recency, most-recently-used first"
        );
        let deferred_ids: Vec<&str> = deferred.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(deferred_ids.len(), 3);
        for never in ["never-a", "never-b"] {
            assert!(
                deferred_ids.contains(&never),
                "a never-queried, never-indexed index must never be warm-booted \
                 ahead of a used one — deferred={deferred_ids:?}"
            );
        }
        clear_cap_env();
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

    /// Deterministic reproduction of the round-4 HIGH finding on PR #3995:
    /// `create_index_handler` / `relocate_index_handler` / `reindex_handler`'s
    /// override, right after each registers a fresh handle (issue #3993
    /// round 3), reap the stale cold-store record for the same id — an
    /// uncoordinated SECOND writer of `cold_store.entries`, racing this
    /// function's own step 1 (`register_cold_entries`) / step 2
    /// (`remove_and_get`) pair. Uses the `cold_park_index_inner` test seam
    /// (`hook`) to land the concurrent write's exact sequence — snapshot,
    /// `registry.register(new_handle)`, guarded reap — in the precise window
    /// between this function's two `DashMap` ops, since that window has no
    /// `.await` boundary and is therefore not reproducible via ordinary
    /// tokio task interleaving (see the function doc's "Concurrent-write
    /// guard").
    ///
    /// At round-4 head (before issue #3995 round 5) this failed: the
    /// concurrent write's reap was an unconditional `cold_store.mark_loaded`,
    /// which unconditionally removed whatever was live under `id` — the
    /// entry THIS function's step 1 had just inserted — leaving `id` in
    /// NEITHER the hot registry (removed by this function's step 2, since the
    /// mismatch it correctly detects still triggers a rollback attempt) NOR
    /// the cold store (already reaped by the concurrent write before that
    /// rollback could run). With both sides of the race now using the
    /// identity-guarded `entry_token` + `mark_loaded_if` pair, each writer
    /// only ever undoes ITS OWN insertion, so the concurrent write's fresh
    /// handle is handed straight back and `id` stays live and discoverable
    /// throughout.
    #[tokio::test]
    async fn cold_park_index_restores_concurrently_swapped_handle_instead_of_orphaning() {
        let registry = IndexRegistry::default();
        let cold = ColdIndexStore::new();
        let id = IndexId::new("race-swap".to_string());
        registry.register(build_mock_handle("race-swap"));

        let hook_registry = registry.clone();
        let hook_cold = cold.clone();
        let hook_id = id.clone();

        let parked = cold_park_index_inner(
            &id,
            &registry,
            &cold,
            mk_entry("race-swap", Some(1)),
            move || {
                // Mirrors `relocate_index_handler` / `create_index_handler` /
                // `reindex_handler`'s override arm, post-issue-#3995-round-5:
                // snapshot the cold entry token BEFORE registering (this
                // observes THIS function's step-1 insertion, since the hook
                // fires right after it), register a brand-new handle under
                // the SAME id, then reap the cold entry only if it is still
                // the SAME one observed by the snapshot — landing exactly
                // between this function's `register_cold_entries` (already
                // ran) and its `remove_and_get` (about to run).
                let cold_entry_before_register = hook_cold.entry_token(&hook_id);
                hook_registry.register(build_mock_handle("race-swap"));
                hook_cold.mark_loaded_if(&hook_id, cold_entry_before_register);
            },
        )
        .await;

        assert!(
            !parked,
            "the park must detect the concurrent swap and report failure, not success"
        );

        let hot = registry.get(&id).is_some();
        let is_cold = cold.contains(&id);
        assert!(
            hot ^ is_cold,
            "after the race `id` must be in EXACTLY one of {{hot, cold}} — never \
             neither (an orphan, issue #3995 round 4 HIGH) and never both — \
             hot={hot} is_cold={is_cold}"
        );
        assert!(
            hot,
            "the concurrent write's fresh registration must survive the race, \
             not be silently discarded by the losing park"
        );
    }

    /// Deterministic reproduction of the round-5 CRITICAL finding on PR
    /// #3995: with a handler's `register` landing entirely BEFORE this
    /// function's `expected` snapshot (issue #3995 round 4's own documented,
    /// believed-safe residual — "never an orphan, but a park that silently
    /// reverts a very recent relocate"), an UNGUARDED reap of the cold-store
    /// entry — the exact `ColdIndexStore::mark_loaded` semantics round 4
    /// shipped at this call site — still orphans `id`. This is ordering
    /// (0, *) in the round-5 re-derivation (see the module doc's
    /// "Concurrent-write guard"): the handler's `register` (mark `g1`) lands
    /// in gap 0 (before `expected` @ step 0.5), and its reap (mark `g2`)
    /// lands in gap 2 (between step 1 `register_cold_entries` and step 2
    /// `remove_and_get`) — one of the 2 (of 4) sub-orderings in that bucket
    /// where the reap fires AT OR AFTER `register_cold_entries`.
    ///
    /// This test uses the OLD unconditional `ColdIndexStore::mark_loaded` —
    /// still present (and still correct) for `get_or_load_index`'s own,
    /// differently-guarded call site — to reproduce EXACTLY what round 4's
    /// three handler call sites did before round 5. It is intentionally kept
    /// in the permanent suite as a regression tripwire: this proves the bug
    /// is real and reproducible, not merely a paper concern, and guards
    /// against any FUTURE call site reintroducing a bare `mark_loaded` reap
    /// after a `register` without going through `entry_token` +
    /// `mark_loaded_if`. See `cold_park_index_handler_reap_guarded_before_park_never_orphans`
    /// immediately below for the fixed counterpart using the pattern the real
    /// handlers now follow.
    #[tokio::test]
    async fn cold_park_index_handler_naive_reap_before_park_orphans_index() {
        let registry = IndexRegistry::default();
        let cold = ColdIndexStore::new();
        let id = IndexId::new("handler-before-park".to_string());

        // The handler's `register` has ALREADY landed by the time this park
        // even starts — e.g. mid-sweep, before this specific id's turn in
        // the sweep's per-id loop (a far wider, entirely ordinary window; no
        // special timing required).
        registry.register(build_mock_handle("handler-before-park"));

        let hook_cold = cold.clone();
        let hook_id = id.clone();

        // The handler's own (unguarded, round-4-shipped) reap fires deep
        // inside the park's critical section: preempted right after its own
        // `register` above, it only resumes to run its `mark_loaded` once
        // this thread yields the core — landing here, between this
        // function's `register_cold_entries` (already ran) and
        // `remove_and_get` (about to run).
        let parked = cold_park_index_inner(
            &id,
            &registry,
            &cold,
            mk_entry("handler-before-park", Some(1)),
            move || {
                hook_cold.mark_loaded(&hook_id);
            },
        )
        .await;

        // `expected` (captured at step 0.5, AFTER the handler's register())
        // already observed the handler's fresh handle, so the identity check
        // at step 2 trivially matches and the park reports "success".
        assert!(
            parked,
            "expected snapshot already matched the handler's pre-existing \
             fresh handle, so the park proceeds \"successfully\""
        );

        let hot = registry.get(&id).is_some();
        let is_cold = cold.contains(&id);
        assert!(
            !hot && !is_cold,
            "reproduction of the round-5 CRITICAL orphan: an UNGUARDED reap \
             deletes this function's own just-inserted cold entry, while \
             this function's own step 2 has already removed the (matching) \
             live handle — id ends up in NEITHER store — hot={hot} is_cold={is_cold}"
        );
    }

    /// Fixed counterpart of
    /// `cold_park_index_handler_naive_reap_before_park_orphans_index`: same
    /// exact interleaving, but the handler's reap uses the issue #3995
    /// round-5 pattern every real call site (`create_index_handler`,
    /// `relocate_index_handler`, `reindex_handler`'s override) now follows —
    /// snapshot `cold_store.entry_token(id)` BEFORE the handler's own
    /// `register` call, then reap via `mark_loaded_if` afterward instead of
    /// an unconditional `mark_loaded`.
    ///
    /// Because the handler's snapshot is taken before ANY of this park's
    /// activity (nothing was cold yet), it observes `None`. By the time the
    /// deferred reap actually runs (inside the hook, after this function's
    /// `register_cold_entries` has inserted its own entry), the current
    /// token is `Some` — a mismatch against the handler's `None` snapshot —
    /// so `mark_loaded_if` correctly refuses to remove an entry it does not
    /// recognize as its own. This function's own step 2 still detaches the
    /// (matching) live handle, so the net effect is the documented,
    /// accepted-out-of-scope residual: the id ends up parked cold under
    /// stale metadata (a park that silently reverts a very-recent
    /// relocate/reindex-override) rather than orphaned in neither store.
    #[tokio::test]
    async fn cold_park_index_handler_reap_guarded_before_park_never_orphans() {
        let registry = IndexRegistry::default();
        let cold = ColdIndexStore::new();
        let id = IndexId::new("handler-before-park-guarded".to_string());

        // Handler's pre-register cold-store snapshot: captured before its
        // own `register` call below, i.e. before ANY of this park's
        // activity — nothing is cold yet, so this observes `None`.
        let handler_snapshot = cold.entry_token(&id);
        assert!(
            handler_snapshot.is_none(),
            "sanity: nothing has been parked yet"
        );

        registry.register(build_mock_handle("handler-before-park-guarded"));

        let hook_cold = cold.clone();
        let hook_id = id.clone();
        let handler_snapshot_for_hook = handler_snapshot.clone();

        let parked = cold_park_index_inner(
            &id,
            &registry,
            &cold,
            mk_entry("handler-before-park-guarded", Some(1)),
            move || {
                // Same deferred-reap timing as the naive-reap reproduction
                // above, but using the round-5 guarded pattern: the reap
                // only proceeds if the cold entry present now is STILL the
                // one (`None`, in this ordering) the handler observed before
                // its own register.
                hook_cold.mark_loaded_if(&hook_id, handler_snapshot_for_hook);
            },
        )
        .await;

        assert!(
            parked,
            "expected snapshot already matched the handler's pre-existing \
             fresh handle, so the park proceeds \"successfully\" — same as \
             the naive-reap reproduction above"
        );

        let hot = registry.get(&id).is_some();
        let is_cold = cold.contains(&id);
        assert!(
            hot ^ is_cold,
            "after the race `id` must be in EXACTLY one of {{hot, cold}} — \
             never neither (the round-5 CRITICAL orphan) — hot={hot} is_cold={is_cold}"
        );
        assert!(
            is_cold,
            "the guarded reap must refuse to remove an entry it did not \
             observe as its own, so this function's own cold-store insertion \
             survives — the id ends up parked cold (reverting the handler's \
             very-recent write), not orphaned"
        );
    }
}
