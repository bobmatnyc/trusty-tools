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
/// re-view added by issue #2164).
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
/// What: every 60 s, resolves the shared idle window via
/// `crate::core::indexer::idle_evict_secs()` (env `TRUSTY_CHUNKS_IDLE_EVICT_SECS`;
/// `0` disables both evictions and the ticker idles), then walks the registry
/// and calls `evict_chunks_if_idle` followed by `evict_bm25_entities_if_idle`
/// on each indexer. Both calls are themselves no-ops for active indexes,
/// indexes without a durable corpus, and already-empty structures, so the
/// walk is cheap. Each eviction acquires the relevant read lock only to check
/// `corpus`/idle state and a brief write lock only when it actually clears
/// its structure.
/// Test: `idle_eviction_drops_and_lazily_rehydrates_chunks`,
/// `bm25_entities_idle_eviction_drops_and_lazily_rehydrates`, and
/// `hnsw_idle_demotion_reviews_clean_promoted_store` cover the per-indexer
/// logic directly; the ticker is a thin scheduling wrapper.
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
            let threshold = Duration::from_secs(secs);
            for id in state.registry.list() {
                let Some(handle) = state.registry.get(&id) else {
                    continue;
                };
                let indexer = handle.indexer.read().await;
                indexer.evict_chunks_if_idle(threshold).await;
                indexer.evict_bm25_entities_if_idle(threshold).await;
                indexer.demote_vector_store_if_idle(threshold).await;
            }
        }
    });
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

/// Spawn the steady-state memory-limit enforcement ticker (issue #2846).
///
/// Why: the daemon accepts a `memory_limit_mb` soft ceiling (auto-tuned to
/// ~25% of host RAM, or `TRUSTY_MEMORY_LIMIT_MB`) but, before this ticker,
/// only ever *enforced* it inside the reindex pipeline. A long-lived serving
/// instance that rarely reindexes grew its resident heap without bound — a
/// production daemon reached ~26.6 GB, 2.2× its own 12 GB stated limit, over
/// ~20 days and was ultimately OOM-killed. This ticker closes that gap: it
/// samples process RSS on a fixed cadence and, once RSS crosses the configured
/// high-water mark, sheds every index's evictable in-memory caches (raw chunk
/// text, the tokenized BM25 corpus, per-file entities, and the promoted HNSW
/// heap copy) via [`CodeIndexer::reclaim_memory_now`]. All of those are
/// durable-corpus-backed and rehydrate lazily, so reclaim is non-destructive.
/// As an opt-in last resort for un-evictable growth (allocator fragmentation /
/// native arenas / a true leak), if RSS is *still* over the hard limit after a
/// reclaim sweep and `TRUSTY_MEMORY_RESTART_ON_LIMIT` is enabled, it triggers a
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

async fn run_memory_pressure_tick(state: &Arc<SearchAppState>) {
    use crate::core::memguard;
    use std::sync::atomic::Ordering;

    // No soft ceiling configured (operator set the limit to 0 / "off") → the
    // enforcement contract is "unlimited"; nothing to do.
    let Some(limit) = memguard::memory_limit_mb() else {
        return;
    };
    let Some(rss) = memguard::current_rss_mb() else {
        return; // Could not sample RSS this tick (rare); try again next tick.
    };
    // Keep /health's RSS telemetry fresh even when no query has triggered a
    // sys-metrics sample recently, so operators see approaching-limit RSS.
    state.last_rss_mb.store(rss, Ordering::Relaxed);

    let pct = memguard::high_water_pct();
    if !memguard::over_high_water(rss, limit, pct) {
        // Below the high-water mark: reset the hysteresis baseline so the
        // NEXT pressure episode always reclaims on its first crossing rather
        // than inheriting a stale baseline from a prior episode.
        state.last_reclaim_rss_mb.store(0, Ordering::Relaxed);
        return;
    }

    // Hysteresis gate — see `should_reclaim_now`'s doc comment for the design
    // rationale (issue #2846 review — MEDIUM: reclaim/rehydrate thrash).
    let last_reclaim_rss = state.last_reclaim_rss_mb.load(Ordering::Relaxed);
    if !should_reclaim_now(rss, last_reclaim_rss) {
        tracing::debug!(
            rss_mb = rss,
            last_reclaim_rss_mb = last_reclaim_rss,
            limit_mb = limit,
            high_water_pct = pct,
            "memory-pressure: RSS at/over high-water but not risen since last reclaim — \
             skipping sweep (hysteresis)"
        );
        return;
    }

    tracing::warn!(
        rss_mb = rss,
        limit_mb = limit,
        high_water_pct = pct,
        "memory-pressure: RSS at/over soft high-water mark — reclaiming evictable caches"
    );

    let mut reclaimed = 0usize;
    for id in state.registry.list() {
        let Some(handle) = state.registry.get(&id) else {
            continue;
        };
        reclaimed += handle.indexer.read().await.reclaim_memory_now().await;
    }

    // Re-sample so the log (and /health) reflect the post-reclaim footprint and
    // the self-restart gate decides on current reality, not the pre-reclaim RSS.
    let after = memguard::current_rss_mb().unwrap_or(rss);
    state.last_rss_mb.store(after, Ordering::Relaxed);
    // Record the hysteresis baseline for the NEXT tick, regardless of whether
    // `after` is still over the high-water mark — a sweep that didn't fully
    // clear the pressure still shouldn't re-fire until RSS rises further.
    state.last_reclaim_rss_mb.store(after, Ordering::Relaxed);
    tracing::warn!(
        reclaimed_entries = reclaimed,
        rss_before_mb = rss,
        rss_after_mb = after,
        limit_mb = limit,
        "memory-pressure: reclaim sweep complete"
    );

    // Last resort (opt-in, default OFF): still over the HARD limit after
    // reclaiming everything evictable means the growth is un-evictable
    // (fragmentation / native arenas / a true leak). Under a supervisor a
    // graceful restart is the only reliable self-cap; warm-boot reloads every
    // index from disk, so no data is lost.
    if memguard::over_high_water(after, limit, 100) && memguard::restart_on_limit_enabled() {
        tracing::error!(
            rss_mb = after,
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
