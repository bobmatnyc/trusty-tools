//! Background ticker tasks: status, disk-size, idle-eviction, watcher-suspend,
//! orphan-reap, and the residency-cap sweep.
//!
//! Why: Separating long-running background spawns from handler code keeps
//! the handler files focused on request/response logic.
//! What: `pub(super) spawn_*_ticker` functions, each detached as a
//! `tokio::spawn` task holding a `Weak<SearchAppState>`.
//! Test: covered indirectly via handler tests that observe side-effects, plus
//! `residency_sweep_tests` for the issue #2161 sweep's per-tick logic and
//! `memory_pressure_tests` for the issue #2846 pressure sweep's hysteresis
//! and opt-in self-restart branch.
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use super::admin::collect_status_counts;
use super::state::{DaemonEvent, SearchAppState};
use crate::core::registry::IndexId;
use crate::service::reindex::ReindexStatus;

/// Spawn a background ticker that emits `StatusChanged` every 2 seconds.
///
/// Why: trusty-memory's pattern is push-driven via mutating handlers, but
/// trusty-search's headline stats (chunk count) change continuously during
/// reindex without a discrete event. A 2s ticker keeps the dashboard's
/// stat cards live (same cadence as the previous poll-based implementation)
/// while still routing through the broadcast channel so the SSE handler
/// stays purely subscription-driven.
/// What: Spawns a detached tokio task holding a `Weak<SearchAppState>` so
/// the ticker terminates automatically when the daemon shuts down (drops the
/// last `Arc`). Each tick recomputes counts and emits one event.
/// Test: subscribe to `/status/stream`, wait > 2s, observe a `status_changed`
/// frame.
pub(super) fn spawn_status_ticker(state: Arc<SearchAppState>) {
    let weak = Arc::downgrade(&state);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(2));
        // Skip the immediate first tick — subscribers get an explicit
        // `connected` frame, and a snapshot follows on the next tick.
        interval.tick().await;
        loop {
            interval.tick().await;
            let Some(state) = weak.upgrade() else {
                break;
            };
            let (indexes, total_chunks) = collect_status_counts(&state).await;
            state.emit(DaemonEvent::StatusChanged {
                indexes: indexes as u64,
                total_chunks: total_chunks as u64,
                uptime_secs: state.started_at.elapsed().as_secs(),
                version: env!("CARGO_PKG_VERSION").to_string(),
            });
        }
    });
}

/// Spawn a background ticker that recomputes the data-directory size every
/// 10 seconds and stores it in `state.disk_bytes`.
///
/// Why (issue #35): `GET /health` reports `disk_bytes`. Walking the data
/// directory (redb + usearch + snapshot files) on every health request would
/// turn a 2 s health poll into unbounded recursive I/O. Computing it off the
/// request path on a fixed cadence keeps `/health` cheap and bounds the
/// staleness to ~10 s — fine for an at-a-glance footprint figure.
/// What: spawns a detached tokio task holding a `Weak<SearchAppState>` so the
/// ticker stops automatically when the daemon drops its last `Arc`. Each tick
/// runs the (blocking) directory walk on `spawn_blocking` so it never stalls
/// the async runtime, then stores the byte total atomically.
/// Test: covered indirectly — `health_includes_resource_fields` asserts the
/// `disk_bytes` field is present and non-negative.
pub(super) fn spawn_disk_size_ticker(state: Arc<SearchAppState>) {
    let weak = Arc::downgrade(&state);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(10));
        loop {
            interval.tick().await;
            let Some(state) = weak.upgrade() else {
                break;
            };
            // The directory walk is blocking filesystem I/O — run it on the
            // blocking pool so it never parks an async worker thread.
            let bytes =
                tokio::task::spawn_blocking(|| match crate::service::persistence::data_dir() {
                    Ok(dir) => trusty_common::sys_metrics::dir_size_bytes(&dir),
                    Err(e) => {
                        tracing::debug!("disk_size_ticker: could not resolve data dir: {e}");
                        0
                    }
                })
                .await
                .unwrap_or(0);
            state
                .disk_bytes
                .store(bytes, std::sync::atomic::Ordering::Relaxed);
        }
    });
}

/// Spawn a background ticker that evicts each index's in-memory `chunks` map,
/// BM25 corpus, and per-file entity map, and demotes its HNSW vector store
/// back to mmap-view mode, after the index has been idle past the configured
/// window (issue #83 follow-up; BM25/entities added by issue #2162; HNSW
/// re-view added by issue #2164; cost-scaled threshold + oldest-idle-first
/// ordering added by issue #3683 slice 2).
///
/// Why (idle-memory audit): the durable redb corpus already serves the query
/// hot path, so an index that hasn't been queried or ingested for a while is
/// holding hundreds of MB of `RawChunk` text, a full tokenized BM25 copy of
/// that same text, and per-file entity lists in the process heap for nothing.
/// `CodeIndexer::evict_chunks_if_idle` and `evict_bm25_entities_if_idle` each
/// reclaim their slice of that heap and lazily rehydrate from redb on the
/// next access; this ticker is what drives both on a fixed cadence across
/// every registered index. It mirrors the `spawn_*_ticker` pattern: a
/// detached task holding a `Weak<SearchAppState>` so it stops when the daemon
/// drops its last `Arc`.
/// What: every 60 s, resolves the shared base idle window via
/// `crate::core::indexer::idle_evict_secs()` (env `TRUSTY_CHUNKS_IDLE_EVICT_SECS`;
/// `0` disables both evictions and the ticker idles) and delegates the sweep
/// itself to [`run_idle_eviction_tick`] — see that function for the
/// per-index cost-scaled threshold and oldest-idle-first ordering.
/// Test: `idle_eviction_drops_and_lazily_rehydrates_chunks`,
/// `bm25_entities_idle_eviction_drops_and_lazily_rehydrates`, and
/// `hnsw_idle_demotion_reviews_clean_promoted_store` cover the per-indexer
/// logic directly; `idle_eviction_tests` covers [`run_idle_eviction_tick`]'s
/// orchestration; this function is a thin scheduling wrapper.
pub(super) fn spawn_idle_chunk_eviction_ticker(state: Arc<SearchAppState>) {
    let weak = Arc::downgrade(&state);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(60));
        // Skip the immediate first tick so a freshly-started daemon isn't
        // evicting before it has served anything.
        interval.tick().await;
        loop {
            interval.tick().await;
            let Some(state) = weak.upgrade() else {
                break;
            };
            let secs = crate::core::indexer::idle_evict_secs();
            if secs == 0 {
                // Eviction disabled by env; keep ticking cheaply so an operator
                // re-enabling it (next process) is honoured without a restart
                // of this loop — but do no work this tick.
                continue;
            }
            let rss_before = crate::core::memguard::current_rss_mb();
            let total_evicted = run_idle_eviction_tick(&state, secs).await;
            // Issue #3657: per-index eviction above genuinely empties the
            // `chunks`/`bm25`/`entities` maps (no lingering `Arc` holder — see
            // `CodeIndexer::clear_in_memory_chunks` / `clear_bm25_entities`),
            // but the Linux release binary's default glibc allocator does not
            // return freed small-object heap to the OS on its own; production
            // observed the "evicted N chunks" log fire repeatedly on a
            // rehydrate/evict cycle while RSS never dropped. Trim once per
            // sweep (not per-index — `malloc_trim` walks the whole arena, so
            // calling it once after every index has been evicted is enough)
            // and log the actual RSS delta so this claim can be verified
            // instead of assumed.
            //
            // `malloc_trim` walks glibc's free lists under the malloc arena
            // lock(s); on a heap that has grown to many GiB (exactly the
            // production shape this fixes) that walk can take tens to
            // hundreds of milliseconds. Running it inline would block this
            // tokio worker thread for that whole span, stalling every other
            // task multiplexed onto it. `spawn_blocking` (already the
            // pattern this file uses for other blocking work — see
            // `spawn_disk_size_ticker` and the orphan-reaper walk below)
            // moves it to the blocking-pool instead.
            if total_evicted > 0 {
                let rss_after = tokio::task::spawn_blocking(|| {
                    crate::core::memguard::trim_heap();
                    crate::core::memguard::current_rss_mb()
                })
                .await
                .unwrap_or(None);
                tracing::info!(
                    total_evicted,
                    rss_before_mb = rss_before,
                    rss_after_mb = rss_after,
                    "idle-eviction sweep complete"
                );
            }
        }
    });
}

/// Sort a snapshot of `(index id, idle duration)` pairs so the LONGEST-idle
/// (oldest / least-recently-used) index sorts FIRST (issue #3683 slice 2 —
/// Defect 2's "the sweep should evict the cheapest-loss index first" fix).
///
/// Why: extracted as a pure function so the ordering rule is unit-testable
/// with synthetic idle durations — mirrors `should_reclaim_now`'s extraction
/// for the identical reason (driving a REAL index's idle clock to specific
/// values on demand in a test is impractical). The #3683 production RCA
/// found the sweep processed every registered index in whatever order
/// `registry.list()` happened to return — effectively arbitrary — so an
/// index that had gone idle a second ago and one that had sat idle for hours
/// were treated identically. The longer an index has sat idle, the less
/// likely it is to be queried again imminently, making it the safest /
/// cheapest-to-lose candidate; this makes that priority explicit and
/// mechanically enforced rather than accidental.
/// What: sorts `candidates` in place, descending by idle [`Duration`] (ties
/// broken by original relative order — `sort_by` is a stable sort).
/// Test: `oldest_idle_first_orders_most_idle_index_first_ties_stable` in
/// `idle_eviction_tests`.
pub(super) fn oldest_idle_first(candidates: &mut [(IndexId, Duration)]) {
    candidates.sort_by_key(|(_, idle)| std::cmp::Reverse(*idle));
}

/// One idle-chunk-eviction tick: snapshot every registered index's idle
/// duration, process them OLDEST-IDLE-FIRST, and evict each past ITS OWN
/// cost-scaled threshold (issue #3683 slice 2). Extracted from
/// [`spawn_idle_chunk_eviction_ticker`] so the orchestration is separable
/// from the `tokio::spawn` scaffolding and directly testable, mirroring
/// [`run_memory_pressure_tick`] / [`run_residency_sweep_tick`].
///
/// Why: the #3683 production RCA's Defect 2 found two compounding policy
/// bugs: (1) every index shared one flat idle window regardless of how
/// expensive it was to rehydrate, and (2) the sweep visited indexes in
/// whatever arbitrary order the registry returned. Fix (1) is
/// `CodeIndexer::cost_scaled_idle_threshold` — an index with a large,
/// expensive-to-rehydrate corpus earns proportionally more idle time before
/// eviction than a small, cheap one, using that index's OWN measured/
/// estimated rehydrate cost (`CodeIndexer::rehydrate_cost_estimate_ms`)
/// rather than a single number shared by every index. Fix (2) is
/// [`oldest_idle_first`]: processing the least-recently-used (safest to
/// lose) index first, rather than an arbitrary order, so the ordering
/// itself is a deliberate, testable policy rather than an accident of
/// `HashMap`/registry iteration.
/// What: takes an already-resolved `base_secs` (the caller has already
/// handled the `idle_evict_secs() == 0` "disabled" short-circuit, matching
/// `run_memory_pressure_tick`'s "no soft ceiling configured" early return
/// style) — but is still defensively a no-op when `base_secs == 0`, so a
/// test (or a future caller) can pass it directly. Snapshots `(id,
/// idle_duration)` for every registered index in ONE read-lock pass, sorts
/// via [`oldest_idle_first`], then in a SECOND pass computes each index's
/// own `cost_scaled_idle_threshold(base_secs)` and calls
/// `evict_chunks_if_idle` / `evict_bm25_entities_if_idle` /
/// `demote_vector_store_if_idle` with that per-index threshold. Returns the
/// total evicted-entry count across every index this tick.
///
/// Ordering is COSMETIC here, not load-bearing (issue #3683 slice 2, critic
/// review — contrast with [`run_pressure_sweep`], where it is): every index
/// is evicted (or not) purely by comparing ITS OWN idle duration against ITS
/// OWN `cost_scaled_idle_threshold`, independent of every other index and
/// with no stop-early budget — visiting the whole snapshot in a different
/// order would evict the exact same set of indexes, just in a different
/// sequence. `oldest_idle_first` is applied anyway for log-ordering
/// consistency (the oldest, safest-to-lose index is reported first) and
/// because it's the natural order to visit for a human reading the tracing
/// output — but no test should assert that changing this order changes
/// WHICH indexes get evicted, because it provably doesn't.
/// Test: `oldest_idle_first_orders_most_idle_index_first_ties_stable`,
/// `run_idle_eviction_tick_evicts_cheap_index_but_spares_costly_one`,
/// `run_idle_eviction_tick_is_noop_when_secs_is_zero` in `idle_eviction_tests`.
async fn run_idle_eviction_tick(state: &Arc<SearchAppState>, base_secs: u64) -> usize {
    if base_secs == 0 {
        return 0;
    }

    // Pass 1: snapshot idle duration for every registered index BEFORE
    // evicting anything, so the sweep can process oldest-idle-first
    // regardless of `registry.list()`'s arbitrary order.
    let mut candidates: Vec<(IndexId, Duration)> = Vec::new();
    for id in state.registry.list() {
        let Some(handle) = state.registry.get(&id) else {
            continue;
        };
        let idle = handle.indexer.read().await.idle_duration();
        candidates.push((id, idle));
    }
    oldest_idle_first(&mut candidates);

    // Pass 2: evict oldest-idle-first, each against ITS OWN cost-scaled
    // threshold rather than one flat window shared by every index.
    let mut total_evicted = 0usize;
    for (id, _idle) in candidates {
        let Some(handle) = state.registry.get(&id) else {
            continue;
        };
        let indexer = handle.indexer.read().await;
        let threshold = indexer.cost_scaled_idle_threshold(base_secs);
        total_evicted += indexer.evict_chunks_if_idle(threshold).await;
        total_evicted += indexer.evict_bm25_entities_if_idle(threshold).await;
        indexer.demote_vector_store_if_idle(threshold).await;
    }
    total_evicted
}

/// Spawn a background ticker that suspends the FSEvents watcher of any index
/// that has gone idle, to stop burning CPU / `fseventsd` load on projects
/// nobody is using.
///
/// Why: once `spawn_for_index` fires (warm-boot or `POST /indexes`), an index's
/// OS watch runs until the index is deleted — so a host tracking hundreds of
/// registered projects keeps hundreds of live watches even though only a few
/// are in active use. Releasing an idle index's watch reclaims that cost; the
/// query path re-establishes it (and reconciles missed edits) on the next
/// query, so suspension is invisible to an active user. Complements the
/// idle-chunk-eviction ticker, which reclaims heap but keeps the watcher hot.
/// What: every 60 s, resolves the idle window via `watch_idle_suspend_secs()`
/// (env `TRUSTY_WATCH_IDLE_SUSPEND_SECS`, default 900 s; `0` disables and the
/// ticker idles), then for each *currently-watched* index whose
/// `CodeIndexer::idle_duration()` meets the threshold, calls
/// `watcher_manager.stop_for_index`. The per-index idle read takes only the
/// indexer read lock; unwatched indexes are skipped before any lock.
/// Test: the idle window helper is covered by
/// `watch_idle_suspend_secs_default_and_env_override`; `is_watching` transitions
/// by `is_watching_reflects_spawn_and_stop`; this is a thin scheduling wrapper.
pub(super) fn spawn_watcher_idle_suspend_ticker(state: Arc<SearchAppState>) {
    let weak = Arc::downgrade(&state);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(60));
        // Skip the immediate first tick so a freshly-started daemon isn't
        // suspending watchers before it has served anything.
        interval.tick().await;
        loop {
            interval.tick().await;
            let Some(state) = weak.upgrade() else {
                break;
            };
            let secs = crate::core::indexer::watch_idle_suspend_secs();
            if secs == 0 {
                // Suspension disabled by env; keep ticking cheaply so an operator
                // re-enabling it (next process) is honoured — but do no work.
                continue;
            }
            let threshold = Duration::from_secs(secs);
            let mut suspended = 0usize;
            for id in state.registry.list() {
                // Only watched indexes can be suspended; check before any lock.
                if !state.watcher_manager.is_watching(&id).await {
                    continue;
                }
                let Some(handle) = state.registry.get(&id) else {
                    continue;
                };
                let idle = handle.indexer.read().await.idle_duration();
                if idle >= threshold && state.watcher_manager.stop_for_index(&id).await {
                    suspended += 1;
                    tracing::info!(
                        index_id = %id.0,
                        idle_secs = idle.as_secs(),
                        "watcher idle-suspended — will resume on next query"
                    );
                }
            }
            if suspended > 0 {
                tracing::info!(
                    "watcher idle-suspend: released {suspended} idle watch(es) this cycle"
                );
            }
        }
    });
}

/// Spawn a background ticker that unregisters indexes whose `root_path` was
/// deleted while the daemon runs (orphan self-heal).
///
/// Why: MPM deletes ephemeral `.worktrees/<uuid>` roots continuously, so a
/// long-lived daemon steadily accumulates dead registrations — each one an
/// idle FSEvents watch that pins `fseventsd` (production: 485 orphans / 8 GB
/// `fseventsd` over 26 days). The boot-time `heal_boot_orphans` pass only runs
/// at startup; this ticker reclaims roots that vanish mid-run. It mirrors the
/// other `spawn_*_ticker` shape: a detached task holding a `Weak` so it stops
/// when the daemon drops its last `Arc`.
/// What: every `TRUSTY_ORPHAN_REAP_SECS` seconds (default hourly; `0` disables
/// and the ticker never spawns), snapshots each index's `(id, root_path)`,
/// runs the existence checks on `spawn_blocking` (a stat-per-index is blocking
/// I/O and must not park an async worker), then calls
/// `unregister_index(.., delete_data=false)` for each reapable orphan —
/// registration removed, on-disk data preserved. [`is_reapable_orphan`] only
/// fires when the root is missing AND its parent survives, so an unmounted
/// external volume is never reaped.
/// Test: `orphan_reaper` unit tests cover the predicate + interval; this is a
/// thin scheduling wrapper.
///
/// [`is_reapable_orphan`]: crate::service::orphan_reaper::is_reapable_orphan
pub(super) fn spawn_orphan_reaper_ticker(state: Arc<SearchAppState>) {
    use crate::service::orphan_reaper::{
        is_reapable_orphan, reap_interval_secs, REAP_INTERVAL_ENV,
    };
    let Some(secs) = reap_interval_secs() else {
        tracing::info!("orphan-reaper: disabled via {REAP_INTERVAL_ENV}=0");
        return;
    };
    let weak = Arc::downgrade(&state);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(secs));
        // Skip the immediate first tick so a freshly-started daemon (which just
        // ran the boot heal) isn't sweeping again before it has served anything.
        interval.tick().await;
        loop {
            interval.tick().await;
            let Some(state) = weak.upgrade() else {
                break;
            };
            // Snapshot (id, root_path) and run existence checks off the async
            // runtime — a stat-per-index is blocking filesystem I/O.
            let candidates: Vec<(String, std::path::PathBuf)> = state
                .registry
                .list_handles()
                .into_iter()
                .map(|h| (h.id.0.clone(), h.root_path.clone()))
                .collect();
            let reapable: Vec<String> = tokio::task::spawn_blocking(move || {
                candidates
                    .into_iter()
                    .filter(|(_, root)| is_reapable_orphan(root))
                    .map(|(id, _)| id)
                    .collect()
            })
            .await
            .unwrap_or_default();

            let mut reaped = 0usize;
            for id in reapable {
                // delete_data=false: never destroy on-disk data automatically —
                // a false-positive detection stays recoverable by re-registering.
                if super::search::unregister_index(&state, &id, false).await {
                    reaped += 1;
                    tracing::info!(
                        "orphan-reaper: unregistered index '{id}' — root_path deleted \
                         (data preserved)"
                    );
                }
            }
            if reaped > 0 {
                tracing::info!(
                    "orphan-reaper: reaped {reaped} orphaned registration(s) this cycle"
                );
            }
        }
    });
}

/// Spawn the usage-based resident-index cap sweep (issue #2161).
///
/// Why: `TRUSTY_WARMBOOT_MAX_INDEXES` only bounds how many indexes are
/// loaded EAGERLY at boot; once queried, an index stays resident for the
/// daemon's whole lifetime. A host tracking a long tail of occasionally-used
/// projects steadily accumulates unbounded RSS as more of them get their
/// first query. This ticker bounds that: every `TRUSTY_RESIDENCY_SWEEP_SECS`
/// (default 120 s), it ranks currently-resident indexes by the same recency
/// key used at boot (`lazy_loader::ids_to_park`, sharing
/// `select_warmboot_entries`'s comparator) and cold-parks everything beyond
/// `TRUSTY_MAX_RESIDENT_INDEXES` via `lazy_loader::cold_park_index` — a
/// non-destructive detach that leaves `indexes.toml`, `roots.toml`, and every
/// on-disk artifact untouched. A subsequent query reloads the index lazily
/// through the existing cold-store path, exactly like a never-yet-queried
/// boot-time cold index.
/// What: spawns a detached task (mirrors every other `spawn_*_ticker`) that
/// ticks at a FIXED interval resolved once at spawn time
/// (`residency_sweep_secs()`; `0` disables the ticker entirely, never
/// spawning). Inside the loop, `max_resident_indexes()` is re-read on every
/// tick so `TRUSTY_MAX_RESIDENT_INDEXES` can be toggled via `daemon.env`
/// without a restart — when it is unset the tick is a cheap no-op (back-compat
/// default: nothing is ever parked).
/// Test: the pure selection logic is covered by `lazy_loader::residency::tests`;
/// this function is a thin scheduling wrapper — `run_residency_sweep_tick` (the
/// per-tick logic) is covered directly by `residency_sweep_tests`; the full
/// on-disk round trip is covered by `tests/residency_cold_park.rs`.
pub(super) fn spawn_residency_sweep_ticker(state: Arc<SearchAppState>) {
    let secs = crate::service::lazy_loader::residency_sweep_secs();
    if secs == 0 {
        tracing::info!("residency-sweep: disabled via TRUSTY_RESIDENCY_SWEEP_SECS=0");
        return;
    }
    let weak = Arc::downgrade(&state);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(secs));
        // Skip the immediate first tick so a freshly-started daemon isn't
        // parking indexes before it has served anything.
        interval.tick().await;
        loop {
            interval.tick().await;
            let Some(state) = weak.upgrade() else {
                break;
            };
            run_residency_sweep_tick(&state).await;
        }
    });
}

/// Spawn the steady-state memory-limit enforcement ticker (issue #2846;
/// budgeted/oldest-idle-first/recency-exempt sweep added by issue #3683
/// slice 2).
///
/// Why: the daemon accepts a `memory_limit_mb` soft ceiling (auto-tuned to
/// ~25% of host RAM, or `TRUSTY_MEMORY_LIMIT_MB`) but, before this ticker,
/// only ever *enforced* it inside the reindex pipeline. A long-lived serving
/// instance that rarely reindexes grew its resident heap without bound — a
/// production daemon reached ~26.6 GB, 2.2× its own 12 GB stated limit, over
/// ~20 days and was ultimately OOM-killed. This ticker closes that gap: it
/// samples process RSS on a fixed cadence and, once RSS crosses the configured
/// high-water mark, sheds evictable in-memory caches (raw chunk text, the
/// tokenized BM25 corpus, per-file entities, and the promoted HNSW heap
/// copy) via [`CodeIndexer::reclaim_memory_now`] — see [`run_pressure_sweep`]
/// for how it now BUDGETS that sweep instead of clearing every registered
/// index unconditionally. All of those structures are durable-corpus-backed
/// and rehydrate lazily, so reclaim is non-destructive. As an opt-in last
/// resort for un-evictable growth (allocator fragmentation / native arenas /
/// a true leak), if RSS is *still* over the hard limit after a reclaim
/// sweep and `TRUSTY_MEMORY_RESTART_ON_LIMIT` is enabled, it triggers a
/// graceful drain-and-exit for the supervisor (launchd/systemd) to respawn —
/// mirroring the ops workaround (a sibling instance restarted daily never
/// accumulates the growth). The restart tier defaults OFF so an unsupervised
/// daemon never self-terminates.
/// What: mirrors every other `spawn_*_ticker` — a detached task holding a
/// `Weak<SearchAppState>` so it stops when the daemon drops its last `Arc`.
/// The cadence is resolved once via `memguard::enforce_interval_secs()`; `0`
/// disables the ticker entirely (it never spawns). Each tick delegates to
/// [`run_memory_pressure_tick`].
/// Test: the threshold decision is covered by `memguard::tests::test_over_high_water`;
/// the reclaim mechanics by `indexer::tests::memory_pressure_reclaim_now_clears_caches`;
/// the budgeted sweep by `memory_pressure_tests`' `pressure_sweep_*` tests;
/// this function is a thin scheduling wrapper.
pub(super) fn spawn_memory_pressure_ticker(state: Arc<SearchAppState>) {
    let secs = crate::core::memguard::enforce_interval_secs();
    if secs == 0 {
        tracing::info!("memory-pressure enforcement: disabled via TRUSTY_MEMORY_ENFORCE_SECS=0");
        return;
    }
    let weak = Arc::downgrade(&state);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(secs));
        // Skip the immediate first tick so a freshly-started daemon isn't
        // reclaiming before it has served (or even warm-booted) anything.
        interval.tick().await;
        loop {
            interval.tick().await;
            let Some(state) = weak.upgrade() else {
                break;
            };
            run_memory_pressure_tick(&state).await;
        }
    });
}

/// One memory-pressure enforcement tick: sample RSS, reclaim evictable caches
/// when over the high-water mark (subject to hysteresis), and (opt-in)
/// self-restart if still over the hard limit afterwards. Extracted from
/// [`spawn_memory_pressure_ticker`] so the orchestration is separable from
/// the `tokio::spawn` scaffolding.
///
/// Why (ordering): the reclaim sweep only touches durable-corpus-backed
/// structures, so it is safe to run under any concurrent read/write load — an
/// index being queried concurrently simply rehydrates on its next access. The
/// self-restart gate re-samples RSS *after* reclaim so a sweep that successfully
/// brought the daemon back under the ceiling never triggers an unnecessary
/// restart; only genuinely un-evictable growth reaches that branch.
///
/// Why (issue #3683 slice 2 — critic review HIGH): the reclaim sweep itself
/// used to be `for id in registry.list() { reclaim_memory_now() }` —
/// unconditional, in whatever order the registry returned, with no notion of
/// "enough" or "this index is busy". That cleared a HOT, actively-queried
/// index the instant RSS crossed the high-water mark, on the very same
/// tick that first noticed the pressure. The sweep now delegates to
/// [`run_pressure_sweep`], which computes `target_freed_mb` (how far RSS
/// sits above the high-water mark) and stops once that's been (estimated to
/// be) reclaimed, oldest-idle-first, exempting recently-queried indexes
/// unless a desperation pass is required — see that function's doc comment.
///
/// Why hysteresis (issue #2846 review — MEDIUM): without it, a host whose
/// steady-state RSS simply sits at/above the high-water mark (independent of
/// evictable caches — e.g. baseline usage genuinely close to the ceiling)
/// would force a full-fleet cache clear on EVERY tick forever, each one
/// making every subsequent query lane pay a redb-rehydrate cost for no
/// memory benefit (nothing new had accumulated to reclaim). `state.last_reclaim_rss_mb`
/// is the hysteresis baseline (see its doc comment on `SearchAppState`): a
/// sweep only runs when current RSS has risen past the RSS observed right
/// after the previous sweep — i.e. caches have measurably repopulated since
/// then. Falling back under the high-water mark resets the baseline so the
/// next pressure episode always reclaims on its first crossing. This is a
/// "rising-edge" gate, not a fixed cooldown timer, so it naturally adapts to
/// how fast the host's workload repopulates caches instead of an arbitrary
/// wait.
///
/// Why anon-RSS enforcement (issue #3683 slice 3 — Defect 3): every RSS value
/// in this function's gate/hysteresis/budget chain (`over_high_water`,
/// `should_reclaim_now`'s baseline, `target_freed_mb`, and the post-sweep
/// re-arm decision) reads [`crate::core::memguard::enforcement_rss_mb`], NOT
/// [`crate::core::memguard::current_rss_mb`] — anonymous RSS by default on
/// Linux (`TRUSTY_MEMORY_ENFORCE_MEASURE`, default `anon` there / `total` on
/// macOS). Total RSS on the #3683 production workload was dominated by
/// file-backed redb mmap pages the kernel reclaims on its own, which read the
/// daemon as permanently over its ceiling even when a sweep freed almost
/// nothing durable. Total RSS is still sampled and stays visible in
/// `/health` (`state.last_rss_mb`) and the tick's log line for operator
/// context — only the ENFORCEMENT decision moved. Every comparison in this
/// chain uses the SAME measure end to end; mixing anon and total RSS in one
/// comparison (e.g. a total-RSS-derived `last_reclaim_rss_mb` compared
/// against a fresh anon-RSS sample) would make the hysteresis baseline
/// meaningless.
/// Pure hysteresis decision: given the current RSS and the RSS observed right
/// after the last reclaim sweep this pressure episode (`0` = no sweep yet),
/// should [`run_memory_pressure_tick`] reclaim again this tick?
///
/// Why: extracted so the rising-edge gate is unit-testable with synthetic
/// values — mirrors [`crate::core::memguard::over_high_water`]'s extraction
/// for the identical reason (a real process's RSS cannot be driven to
/// specific values on demand in a test, and forcing it across a multi-GB
/// ceiling would mean allocating gigabytes). See
/// [`run_memory_pressure_tick`]'s doc comment for the full "why hysteresis"
/// rationale.
/// What: `true` when `last_reclaim_rss_mb == 0` (first crossing of a fresh
/// pressure episode — always reclaim) OR `rss_mb > last_reclaim_rss_mb`
/// (caches have measurably repopulated since the last sweep). `false`
/// (skip this tick) when RSS is flat or has fallen relative to the last
/// sweep's outcome.
/// Test: `tickers::memory_pressure_tests::{hysteresis_first_crossing_always_reclaims,
/// hysteresis_skips_when_rss_has_not_risen, hysteresis_reclaims_again_once_rss_has_risen}`.
fn should_reclaim_now(rss_mb: u64, last_reclaim_rss_mb: u64) -> bool {
    last_reclaim_rss_mb == 0 || rss_mb > last_reclaim_rss_mb
}

/// What `state.last_reclaim_rss_mb` (the [`should_reclaim_now`] hysteresis
/// baseline) should become after a sweep completing with `completion`,
/// having sampled `after_rss_mb` post-trim (issue #3683 slice 2, round-2
/// critic review HIGH).
///
/// Why: extracted as a pure function — same rationale as `should_reclaim_now`
/// itself — so the interaction this fixes is unit-testable without driving
/// real process RSS. Before the stop-early budget
/// ([`run_pressure_sweep`]/[`SweepCompletion`]) existed, a completed sweep
/// ALWAYS meant "every evictable index was actually cleared", so trusting
/// `after_rss_mb` as the next baseline was safe: if RSS was still high
/// afterward, only genuine repopulation (a real rise) could justify sweeping
/// again. [`ESTIMATED_BYTES_FREED_PER_RECLAIMED_ENTRY`] being an uncalibrated
/// guess breaks that guarantee for [`SweepCompletion::EarlyStop`] — the
/// sweep may have stopped while real RSS is still over the high-water mark
/// AND candidates remain unswept. Storing `after_rss_mb` in that case would
/// let `should_reclaim_now`'s strict `rss_mb > last_reclaim_rss_mb` gate wedge
/// the daemon indefinitely at a steady-state RSS plateau near the ceiling —
/// reclaimable memory sitting untouched forever — exactly the 2.2×-limit gap
/// issue #2846 (and this ticket) exists to close.
/// What: [`SweepCompletion::Exhausted`] → `after_rss_mb` (trust it, matching
/// pre-stop-early-budget semantics — there is genuinely nothing left to
/// reclaim, so only a real RSS rise should re-trigger).
/// [`SweepCompletion::EarlyStop`] → `0`, the same "no sweep yet" sentinel
/// [`run_memory_pressure_tick`] uses when RSS falls below the high-water
/// mark — forces the NEXT tick to reclaim unconditionally
/// (`should_reclaim_now` returns `true` whenever the baseline is `0`),
/// regardless of whether RSS rose, so the untouched candidates get a chance
/// to be swept (or the estimate that skipped them gets corrected on retry).
/// Test: `hysteresis_survives_early_stop_sweep_even_when_rss_is_flat`.
fn hysteresis_baseline_after_sweep(completion: SweepCompletion, after_rss_mb: u64) -> u64 {
    match completion {
        SweepCompletion::Exhausted => after_rss_mb,
        SweepCompletion::EarlyStop => 0,
    }
}

// ---------------------------------------------------------------------------
// Budgeted, oldest-idle-first, recency-exempt pressure sweep (issue #3683
// slice 2 — critic review HIGH: the pressure sweep was still an
// undifferentiated full-fleet clear).
// ---------------------------------------------------------------------------

/// Default idle-duration floor (seconds) below which an index is EXEMPT from
/// the memory-pressure sweep's first pass (issue #3683 slice 2). Override
/// via `TRUSTY_MEMORY_PRESSURE_EXEMPT_IDLE_SECS`; `0` disables the exemption
/// entirely (every index is a pass-1 candidate, matching the pre-this-fix
/// behaviour of clearing whatever the sweep reaches before its budget runs
/// out).
///
/// Why: the #3683 production incident's pressure sweep cleared a HOT,
/// actively-queried 315K-chunk index the instant RSS crossed the high-water
/// mark — the pressure-path twin of the idle-evict path's thrash-eviction
/// bug this slice already fixed. 30s is deliberately much shorter than the
/// idle-evict base window (300s): a pressure sweep is a genuine "running low
/// on memory" emergency, so the bar for "this index is busy enough to spare"
/// is lower than idle-eviction's "has anyone touched this in the last five
/// minutes" bar.
const DEFAULT_MEMORY_PRESSURE_EXEMPT_IDLE_SECS: u64 = 30;

/// Resolve the memory-pressure exemption idle floor (seconds) from
/// `TRUSTY_MEMORY_PRESSURE_EXEMPT_IDLE_SECS`, falling back to
/// [`DEFAULT_MEMORY_PRESSURE_EXEMPT_IDLE_SECS`] when unset or unparseable.
fn memory_pressure_exempt_idle_secs() -> u64 {
    std::env::var("TRUSTY_MEMORY_PRESSURE_EXEMPT_IDLE_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_MEMORY_PRESSURE_EXEMPT_IDLE_SECS)
}

/// Coarse, deliberately conservative estimate (bytes) of resident heap freed
/// per reclaimed chunk/BM25-doc entry, used ONLY to decide when a pressure
/// sweep pass has freed "enough" (issue #3683 slice 2 — critic review's
/// "per-index freed estimates between evictions" option).
///
/// Why: freed heap is not returned to the OS — and so is not reflected in a
/// fresh RSS sample — until `malloc_trim` runs, and `malloc_trim` itself
/// costs tens to hundreds of milliseconds on a multi-GB heap (see
/// `run_memory_pressure_tick`'s own trim comment below). Re-sampling REAL
/// RSS between every reclaimed index during a sweep of potentially hundreds
/// of indexes would mean paying that cost hundreds of times per tick. This
/// estimate lets the sweep decide "stop early" using only cheap arithmetic,
/// with exactly ONE trim + re-sample at the very end of the tick (unchanged
/// from before this fix).
///
/// **Uncalibrated (round-2 critic review note):** `2048` is a documented
/// guess, not measured against a real heap profile. It can OVER-estimate
/// real freed bytes just as easily as under-estimate — average chunk/BM25
/// entry size varies enormously by corpus (language, chunk granularity,
/// comment density). An over-estimate makes [`run_pressure_sweep`] stop
/// "early" while real RSS is still over the high-water mark. That used to be
/// safe by construction (a completed sweep always meant "cleared every
/// evictable index"), but a stop-early budget breaks that assumption — see
/// [`SweepCompletion`] and `run_memory_pressure_tick`'s handling of it for
/// the fix (issue #3683 slice 2, round-2 critic review HIGH). The tick's
/// "reclaim sweep complete" log line reports both this estimate and the
/// REAL post-trim RSS delta side by side specifically so a future pass can
/// calibrate this constant against production data instead of guessing again.
const ESTIMATED_BYTES_FREED_PER_RECLAIMED_ENTRY: u64 = 2_048;

/// Estimate MB freed for `reclaimed_entries` reclaimed entries — see
/// [`ESTIMATED_BYTES_FREED_PER_RECLAIMED_ENTRY`].
fn estimate_freed_mb(reclaimed_entries: usize) -> u64 {
    (reclaimed_entries as u64).saturating_mul(ESTIMATED_BYTES_FREED_PER_RECLAIMED_ENTRY)
        / (1024 * 1024)
}

/// Whether a [`run_pressure_sweep`] call stopped because every reachable
/// candidate was actually visited, or because it stopped EARLY on the
/// (possibly wrong) [`estimate_freed_mb`] budget while candidates remained
/// untouched (issue #3683 slice 2, round-2 critic review HIGH).
///
/// Why: [`ESTIMATED_BYTES_FREED_PER_RECLAIMED_ENTRY`] is an uncalibrated
/// guess and can OVER-estimate real freed bytes, in which case a sweep that
/// "stopped early" left real RSS still over the high-water mark with
/// reachable candidates never visited. Before the stop-early budget existed,
/// a completed sweep always meant "every evictable index was actually
/// cleared" — `run_memory_pressure_tick` trusted the post-trim RSS as the
/// next tick's hysteresis baseline (`state.last_reclaim_rss_mb`) precisely
/// because of that guarantee. A stop-early sweep does NOT carry that
/// guarantee, so the caller must react differently — see
/// `run_memory_pressure_tick`'s match on this enum.
/// Test: `pressure_sweep_stops_early_once_target_reached_sparing_least_idle`,
/// `pressure_sweep_exempts_hot_indexes_under_mild_pressure`,
/// `pressure_sweep_desperation_pass_clears_hot_indexes_under_extreme_pressure`,
/// `hysteresis_survives_early_stop_sweep_even_when_rss_is_flat`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SweepCompletion {
    /// The running freed-estimate met `target_freed_mb` while at least one
    /// candidate (cold or hot) remained unvisited. Those candidates were
    /// spared ON PURPOSE by the budget, but the budget itself might be
    /// wrong — the caller must not treat this like "nothing more to
    /// reclaim".
    EarlyStop,
    /// Every candidate that could possibly be visited (all of `cold`, plus
    /// all of `hot` if the desperation pass ran) actually WAS visited —
    /// whether or not the target was ever reached. There is nothing left to
    /// reclaim from any registered index regardless of the estimate's
    /// accuracy, so this is safe to treat exactly like the pre-stop-early-
    /// budget "swept everything" semantics.
    Exhausted,
}

/// Budgeted, oldest-idle-first, recency-exempt memory-pressure sweep (issue
/// #3683 slice 2 — critic review HIGH). Replaces the former unconditional
/// "clear every registered index" behaviour that used to live inline in
/// [`run_memory_pressure_tick`].
///
/// Why: an undifferentiated full-fleet clear cleared a HOT, actively-queried
/// 315K-chunk index the instant RSS crossed the high-water mark — the
/// pressure-path twin of the idle-evict thrash-eviction bug this slice
/// already fixed on the idle-evict ticker. Three changes fix it here:
///
/// 1. **Oldest-idle-first order** (reusing [`oldest_idle_first`] — THIS is
///    where that ordering becomes load-bearing: with a stop-early budget
///    (point 2), which indexes get cleared before the budget is met now
///    depends on this order, unlike [`run_idle_eviction_tick`]'s per-index
///    independent threshold, where the sweep order never changes which
///    indexes ultimately get evicted, only what order they're visited in).
/// 2. **Stop once the target is (estimated to be) met**: `target_freed_mb`
///    is how far current RSS sits above the high-water mark
///    (`rss - high_water_target_mb`, computed by the caller). The sweep
///    accumulates [`estimate_freed_mb`] as it clears indexes and stops as
///    soon as the running estimate meets or exceeds the target, instead of
///    unconditionally clearing every registered index (which could be
///    hundreds) — see [`SweepCompletion`] for how the caller is told whether
///    that happened.
/// 3. **Two-phase: exempt-first, then desperation.** Pass 1 sweeps only
///    indexes idle at least [`memory_pressure_exempt_idle_secs`] (a hot
///    index is never the first thing cleared), oldest-idle-first among
///    them. If pass 1 exhausts every non-exempt index WITHOUT reaching the
///    target — genuine memory pressure, not just cold caches — pass 2
///    (desperation) sweeps the previously-exempt (hot) indexes too,
///    oldest-idle-first among THEM: avoiding an OOM kill outweighs a hot
///    index's warm cache.
///
/// What: returns `(total reclaimed entries, indexes actually cleared,
/// completion)` for the tick's log line and hysteresis decision (see
/// [`SweepCompletion`]). `target_freed_mb == 0` sweeps nothing and returns
/// `(0, 0, Exhausted)` — trivially "nothing needed reclaiming" — the current
/// caller only invokes this after confirming over-high-water (so
/// `target_freed_mb` is always `> 0` in production, modulo the exact-
/// boundary case where `rss == high_water_target_mb`), but the guard is kept
/// so a future/test caller can pass `0` safely. `completion` is computed by
/// comparing how many candidates were actually visited against the total
/// snapshotted (`cold.len() + hot.len()`) — `Exhausted` iff every one was
/// visited, `EarlyStop` otherwise (a `break` fired with candidates left).
/// Test: `pressure_sweep_stops_early_once_target_reached_sparing_least_idle`,
/// `pressure_sweep_exempts_hot_indexes_under_mild_pressure`,
/// `pressure_sweep_desperation_pass_clears_hot_indexes_under_extreme_pressure`
/// in `memory_pressure_tests`.
async fn run_pressure_sweep(
    state: &Arc<SearchAppState>,
    target_freed_mb: u64,
) -> (usize, usize, SweepCompletion) {
    if target_freed_mb == 0 {
        return (0, 0, SweepCompletion::Exhausted);
    }
    let exempt_secs = memory_pressure_exempt_idle_secs();
    let exempt_floor = Duration::from_secs(exempt_secs);

    // Snapshot idle duration for every registered index in ONE read-lock
    // pass (mirrors `run_idle_eviction_tick`), splitting into "cold enough
    // for pass 1" vs "hot — exempt unless desperation", each sorted
    // oldest-idle-first.
    let mut cold: Vec<(IndexId, Duration)> = Vec::new();
    let mut hot: Vec<(IndexId, Duration)> = Vec::new();
    for id in state.registry.list() {
        let Some(handle) = state.registry.get(&id) else {
            continue;
        };
        let idle = handle.indexer.read().await.idle_duration();
        if exempt_secs > 0 && idle < exempt_floor {
            hot.push((id, idle));
        } else {
            cold.push((id, idle));
        }
    }
    oldest_idle_first(&mut cold);
    oldest_idle_first(&mut hot);
    // Snapshotted BEFORE either pass drains its `Vec` — see the `visited`
    // comparison at the end, which is how `SweepCompletion` is decided.
    let total_candidates = cold.len() + hot.len();

    let mut reclaimed = 0usize;
    let mut cleared = 0usize;
    let mut freed_mb = 0u64;
    let mut visited = 0usize;

    // Pass 1: non-exempt (cold) indexes only, oldest-idle-first.
    for (id, _idle) in cold {
        if freed_mb >= target_freed_mb {
            break;
        }
        visited += 1;
        let Some(handle) = state.registry.get(&id) else {
            continue;
        };
        let n = handle.indexer.read().await.reclaim_memory_now().await;
        if n > 0 {
            reclaimed += n;
            cleared += 1;
            freed_mb += estimate_freed_mb(n);
        }
    }

    // Pass 2 (desperation): only reached if pass 1 exhausted every
    // non-exempt index without hitting the target AND there are exempt (hot)
    // indexes left. Avoiding an OOM kill outweighs a hot index's warm cache.
    if freed_mb < target_freed_mb && !hot.is_empty() {
        tracing::warn!(
            target_freed_mb,
            freed_so_far_mb = freed_mb,
            hot_candidates = hot.len(),
            "memory-pressure: exemption-respecting pass did not reach target — desperation pass \
             will clear recently-queried (hot) indexes too"
        );
        for (id, _idle) in hot {
            if freed_mb >= target_freed_mb {
                break;
            }
            visited += 1;
            let Some(handle) = state.registry.get(&id) else {
                continue;
            };
            let n = handle.indexer.read().await.reclaim_memory_now().await;
            if n > 0 {
                reclaimed += n;
                cleared += 1;
                freed_mb += estimate_freed_mb(n);
            }
        }
    }

    let completion = if visited >= total_candidates {
        SweepCompletion::Exhausted
    } else {
        SweepCompletion::EarlyStop
    };
    (reclaimed, cleared, completion)
}

async fn run_memory_pressure_tick(state: &Arc<SearchAppState>) {
    use crate::core::memguard;
    use std::sync::atomic::Ordering;

    // No soft ceiling configured (operator set the limit to 0 / "off") → the
    // enforcement contract is "unlimited"; nothing to do.
    let Some(limit) = memguard::memory_limit_mb() else {
        return;
    };
    let Some(total_rss) = memguard::current_rss_mb() else {
        return; // Could not sample RSS this tick (rare); try again next tick.
    };
    // Keep /health's RSS telemetry fresh (and always TOTAL RSS — the figure
    // operators expect there, matching the reindex pipeline's `over_memory_limit`)
    // even when no query has triggered a sys-metrics sample recently, so
    // operators see approaching-limit RSS. Untouched by issue #3683 slice 3's
    // enforcement-measure switch below: total RSS stays visible for context
    // regardless of which measure the GATE itself uses.
    state.last_rss_mb.store(total_rss, Ordering::Relaxed);

    // Issue #3683 slice 3 (Defect 3): the ENFORCEMENT decision below — the
    // high-water gate, the hysteresis baseline, and the sweep's
    // `target_freed_mb` — reads whichever measure `enforcement_measure()`
    // selects (anon RSS by default on Linux, total RSS on macOS), NOT
    // necessarily `total_rss` above. This is the one substitution this slice
    // makes; every comparison in this chain must use `enforce_rss`
    // consistently — mixing it with `total_rss` anywhere in the chain would
    // make the slice-2 hysteresis baseline meaningless (see
    // `memguard::enforcement_rss_mb_for_pid`'s doc comment).
    let Some(enforce_rss) = memguard::enforcement_rss_mb() else {
        return; // Could not sample the chosen measure this tick; try again next tick.
    };

    let pct = memguard::high_water_pct();
    if !memguard::over_high_water(enforce_rss, limit, pct) {
        // Below the high-water mark: reset the hysteresis baseline so the
        // NEXT pressure episode always reclaims on its first crossing rather
        // than inheriting a stale baseline from a prior episode.
        state.last_reclaim_rss_mb.store(0, Ordering::Relaxed);
        return;
    }

    // Hysteresis gate — see `should_reclaim_now`'s doc comment for the design
    // rationale (issue #2846 review — MEDIUM: reclaim/rehydrate thrash).
    let last_reclaim_rss = state.last_reclaim_rss_mb.load(Ordering::Relaxed);
    if !should_reclaim_now(enforce_rss, last_reclaim_rss) {
        tracing::debug!(
            enforce_rss_mb = enforce_rss,
            total_rss_mb = total_rss,
            last_reclaim_rss_mb = last_reclaim_rss,
            limit_mb = limit,
            high_water_pct = pct,
            "memory-pressure: RSS at/over high-water but not risen since last reclaim — \
             skipping sweep (hysteresis)"
        );
        return;
    }

    // Issue #3683 slice 2 (critic review HIGH): budget the sweep to how far
    // over the high-water mark we actually are, instead of clearing every
    // registered index unconditionally — see `run_pressure_sweep`.
    let high_water_mb = memguard::high_water_target_mb(limit, pct);
    let target_freed_mb = enforce_rss.saturating_sub(high_water_mb);
    tracing::warn!(
        enforce_rss_mb = enforce_rss,
        total_rss_mb = total_rss,
        enforcement_measure = ?memguard::enforcement_measure(),
        limit_mb = limit,
        high_water_pct = pct,
        target_freed_mb,
        "memory-pressure: RSS at/over soft high-water mark — reclaiming evictable caches"
    );

    let (reclaimed, indexes_cleared, completion) = run_pressure_sweep(state, target_freed_mb).await;

    // Issue #3657: `reclaim_memory_now` empties the in-memory maps (a genuine
    // Rust-level free — no lingering `Arc` holder), but the Linux release
    // binary's default glibc allocator does not hand freed small-object heap
    // back to the OS on its own; RSS previously stayed flat here even though
    // `reclaimed` was nonzero. Trimming right after the sweep, before the
    // re-sample below, is what makes `rss_after_mb` an honest number instead
    // of one that always equals `rss_before_mb`.
    //
    // This tick fires exactly when the box is under memory pressure — i.e.
    // exactly when the heap is largest and most fragmented, so `malloc_trim`
    // walking glibc's free lists under the arena lock(s) can take tens to
    // hundreds of milliseconds here. Run it on the blocking pool (matching
    // this file's existing pattern — see `spawn_disk_size_ticker` and the
    // orphan-reaper's `spawn_blocking` walk) instead of stalling this tokio
    // worker thread, and sample RSS inside the same blocking closure so the
    // before/after delta stays honest (no other allocation activity can slip
    // in between the trim and the sample on a different thread).
    // Sample BOTH measures inside the same blocking closure, immediately
    // after the trim, so neither reading drifts relative to the other or to
    // the trim itself (matching the pre-slice-3 single-sample discipline —
    // see the comment above this block).
    let (after_total, after_enforce) = tokio::task::spawn_blocking(|| {
        memguard::trim_heap();
        (memguard::current_rss_mb(), memguard::enforcement_rss_mb())
    })
    .await
    .unwrap_or((None, None));
    let after_total = after_total.unwrap_or(total_rss);
    let after_enforce = after_enforce.unwrap_or(enforce_rss);

    // Store the post-reclaim TOTAL RSS so /health keeps reporting the
    // operator-facing figure, unaffected by which measure gates enforcement.
    state.last_rss_mb.store(after_total, Ordering::Relaxed);
    // Record the hysteresis baseline for the NEXT tick (issue #3683 slice 2,
    // round-2 critic review HIGH) — using `after_enforce`, the SAME measure
    // the gate above compared against, per issue #3683 slice 3's "never mix
    // anon and total in one comparison chain" invariant. An `Exhausted` sweep
    // trusts `after_enforce` as-is (nothing more to reclaim short of real
    // repopulation); an `EarlyStop` sweep resets to the `0` sentinel instead,
    // so the next tick reclaims unconditionally rather than being wedged by
    // `should_reclaim_now`'s strict rise check on an estimate that might
    // have been wrong — see `hysteresis_baseline_after_sweep`.
    state.last_reclaim_rss_mb.store(
        hysteresis_baseline_after_sweep(completion, after_enforce),
        Ordering::Relaxed,
    );
    // Issue #3683 slice 2 (critic review OPTIONAL note): log the ESTIMATED
    // freed MB (from the uncalibrated per-entry constant) alongside the REAL
    // before/after delta on the enforcement measure (the number the budget
    // was computed against), plus total RSS for operator context — issue
    // #3683 slice 3 keeps total RSS visible in logs even though it no longer
    // drives the gate.
    tracing::warn!(
        reclaimed_entries = reclaimed,
        indexes_cleared,
        sweep_completion = ?completion,
        estimated_freed_mb = estimate_freed_mb(reclaimed),
        enforcement_measure = ?memguard::enforcement_measure(),
        actual_freed_mb = enforce_rss.saturating_sub(after_enforce),
        enforce_rss_before_mb = enforce_rss,
        enforce_rss_after_mb = after_enforce,
        total_rss_before_mb = total_rss,
        total_rss_after_mb = after_total,
        limit_mb = limit,
        "memory-pressure: reclaim sweep complete"
    );

    // Last resort (opt-in, default OFF): still over the HARD limit (on the
    // enforcement measure) after reclaiming everything evictable means the
    // growth is un-evictable (fragmentation / native arenas / a true leak).
    // Under a supervisor a graceful restart is the only reliable self-cap;
    // warm-boot reloads every index from disk, so no data is lost.
    if memguard::over_high_water(after_enforce, limit, 100) && memguard::restart_on_limit_enabled()
    {
        tracing::error!(
            enforce_rss_mb = after_enforce,
            total_rss_mb = after_total,
            limit_mb = limit,
            "memory-pressure: RSS still over hard limit after reclaim — triggering graceful \
             self-restart (TRUSTY_MEMORY_RESTART_ON_LIMIT enabled) for supervisor respawn"
        );
        // watch::Sender::send only errs if all receivers dropped (daemon already
        // shutting down); ignoring it is correct — the shutdown is already underway.
        let _ = state.shutdown_tx.send(true);
    }
}

/// One residency-sweep tick: rank resident indexes, cold-park everything
/// beyond the cap. Extracted from `spawn_residency_sweep_ticker` so the
/// per-tick logic can be reasoned about (and, via the pure helpers it calls,
/// tested) independently of the `tokio::spawn` scaffolding.
///
/// Why (TOCTOU / composition with reindex): an index with a `Running`
/// reindex must never be parked. The reindex task holds its OWN `Arc` to the
/// live `IndexHandle` (captured when the reindex started) and keeps mutating
/// it — including `handle.stages`, which lives on that specific instance.
/// Parking would detach the handle from the registry while the reindex is
/// still writing into it; a query landing after that point would lazily
/// rebuild a BRAND NEW `IndexHandle` from disk (via `get_or_load_index`) that
/// the in-flight reindex task knows nothing about, so its completion would
/// update a `stages` `Arc` no future request will ever observe. Skipping any
/// index with a `Running` entry in `reindex_progress` avoids that permanently
/// wedged status.
///
/// Why (watcher lifetime): `cold_park_index` deliberately never touches the
/// file watcher (see its own doc). But leaving the OLD watcher attached to
/// the now-detached indexer would starve the NEXT reload: `WatcherManager`
/// keys `is_watching` purely by `IndexId`, so after a lazy reload builds a
/// FRESH `IndexHandle` + `CodeIndexer`, the search handler's
/// `if !is_watching(id) { spawn_for_index(...) }` wake-up would see `true`
/// (the stale watcher is still registered under that id) and never spawn a
/// watcher pointed at the fresh indexer — silently freezing that index's
/// live-update path until the next full daemon restart. Stopping the watcher
/// here mirrors `spawn_watcher_idle_suspend_ticker` exactly: the query-time
/// wake-up already re-spawns the watcher AND runs `reconcile_one_index` to
/// catch up on anything that changed while unwatched, so nothing is lost —
/// this is the identical, already-tested mechanism idle-suspend relies on.
async fn run_residency_sweep_tick(state: &Arc<SearchAppState>) {
    let Some(cap) = crate::service::lazy_loader::max_resident_indexes() else {
        return; // Feature disabled — back-compat default (issue #2161).
    };

    let resident_ids: HashSet<String> = state.registry.list().into_iter().map(|id| id.0).collect();
    if resident_ids.len() <= cap {
        return;
    }

    let toml_entries = match crate::service::persistence::load_index_registry() {
        Ok(entries) => entries,
        Err(e) => {
            tracing::warn!("residency-sweep: could not read indexes.toml: {e}");
            return;
        }
    };
    let resident_entries: Vec<_> = toml_entries
        .into_iter()
        .filter(|e| resident_ids.contains(&e.id))
        .collect();

    let to_park = crate::service::lazy_loader::ids_to_park(resident_entries, cap);
    if to_park.is_empty() {
        return;
    }

    let mut parked = 0usize;
    for entry in to_park {
        let id = IndexId::new(entry.id.clone());

        // Never park an index with an in-flight reindex — see the function
        // doc for why this composes badly with the residency detach.
        if state
            .reindex_progress
            .get(&id)
            .is_some_and(|p| p.status.load() == ReindexStatus::Running)
        {
            continue;
        }

        if crate::service::lazy_loader::cold_park_index(
            &id,
            &state.registry,
            &state.cold_store,
            entry,
        )
        .await
        {
            // See the function doc: stop the watcher so the next reload's
            // wake-up path re-establishes it (and reconciles) against the
            // FRESH indexer instance instead of leaving a stale one pinned.
            state.watcher_manager.stop_for_index(&id).await;
            parked += 1;
        }
    }

    if parked > 0 {
        tracing::info!(
            "residency-sweep: cold-parked {parked} index(es) beyond top-{cap} resident cap"
        );
    }
}

#[cfg(test)]
#[path = "residency_sweep_tests.rs"]
mod residency_sweep_tests;

#[cfg(test)]
#[path = "memory_pressure_tests.rs"]
mod memory_pressure_tests;

#[cfg(test)]
#[path = "idle_eviction_tests.rs"]
mod idle_eviction_tests;
