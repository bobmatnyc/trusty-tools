//! Idle eviction + lazy rehydration for the BM25 corpus and per-file
//! entities (issue #2162).
//!
//! Why: heap-profiling a production daemon (77 indexes, 3.9 GB on-disk vs.
//! ~13 GB resident) found two structures that are permanently resident once
//! loaded, independent of query activity: `BM25Index::doc_terms` (a full
//! second, tokenized copy of every chunk's text — see
//! `crates/trusty-common/src/bm25.rs`) and the per-file entity map. Both are
//! rebuilt in-heap on every warm-boot (`persist::load_chunks_from_redb`) and
//! neither has an eviction path today — only the chunk-text map does
//! (`CodeIndexer::evict_chunks_if_idle` in `mod.rs`). Since both are 100%
//! recoverable from the durable redb corpus, they can follow the exact same
//! idle-evict / lazy-rehydrate shape, just for a different pair of fields.
//! What: [`CodeIndexer::evict_bm25_entities_if_idle`] clears `self.bm25` and
//! `self.entities` once the index has been idle past a threshold (shares the
//! same `TRUSTY_CHUNKS_IDLE_EVICT_SECS` window and the same 60s ticker as
//! chunk eviction — see `service::server::tickers::spawn_idle_chunk_eviction_ticker`).
//! [`CodeIndexer::ensure_bm25_entities_loaded`] rehydrates both from redb the
//! next time a query lane or ingest-commit path needs them, mirroring
//! `ensure_chunks_loaded`'s guard-flag-then-rebuild shape so concurrent
//! readers never observe a half-populated structure.
//! Test: `bm25_entities_idle_eviction_drops_and_lazily_rehydrates` and
//! `bm25_entities_idle_eviction_skips_indexers_without_corpus` in
//! `indexer::tests`.
//!
//! Also home to [`CodeIndexer::demote_vector_store_if_idle`] (issue #2164),
//! which rides the exact same idle sweep to re-view (mmap-demote) an idle,
//! write-promoted HNSW vector store back to `Index::view` — see that
//! method's doc comment and `UsearchStore::try_demote_to_view` for the full
//! design.
//!
//! ## Detached, deduplicated rehydrate (issue #3683 slice 1)
//!
//! Why: production RCA (#3683) found that the rehydrate this module performs
//! used to run INLINE inside the caller's own awaited future — including
//! interactive query handlers wrapped in `service::query_timeout`'s
//! `tokio::time::timeout`. When that outer timeout cancels the handler on
//! expiry, the whole awaited future — scan AND the map-publish/flag-clear
//! that followed it — is dropped mid-flight. The redb scan itself doesn't
//! stop (it was `spawn_blocking`, which can't be cancelled), but its RESULT
//! is thrown away: the maps stay empty and `*_evicted` stays `true`, so the
//! very next query pays the full O(corpus) scan again. On a 315K-chunk NFS-
//! backed index (27-40s/scan) this is a self-sustaining livelock: every
//! query times out, discarding its own rehydrate, forever (until an
//! unrelated race lets a scan land in the [`REHYDRATE_RACE_RETRIES`] window
//! in `search/lanes.rs`, purely by luck).
//!
//! [`CodeIndexer::ensure_corpus_rehydrated`] fixes this by running the scan
//! AND the commit (map/BM25/entity publish + flag clear) inside a `tokio::
//! spawn` task that is never owned by any caller's future — mirroring
//! `core::corpus::open_guard`'s detached-attempt pattern for the identical
//! "a caller's own timeout must never cancel work other callers depend on"
//! problem (issue #3659). A caller only ever cancels ITS OWN bounded wait for
//! the task's completion notification; the task itself runs to completion
//! and commits regardless of how many waiters gave up. This also
//! consolidates what used to be two independent `load_all_chunks()` scans
//! (one each for `ensure_chunks_loaded` / `ensure_bm25_entities_loaded`) into
//! one shared scan that populates chunks + BM25 + entities together, since
//! both flags are always set together by the same idle-evict tick (see
//! `service::server::tickers::spawn_idle_chunk_eviction_ticker`).
//!
//! While a rehydrate is in flight, `ensure_corpus_rehydrated` also folds in
//! the issue #3684 fix: the shared scan sorts the loaded chunks by their
//! stable `id` before the cap-truncated BM25 upsert loop, so which subset of
//! an over-cap corpus is lexically searchable converges on the SAME set
//! every rehydrate (see [`spawn_detached_rehydrate`] and
//! [`crate::core::bm25::Bm25Index::upsert_document_reporting`]), instead of
//! silently shifting with redb's B-tree iteration order.
//!
//! ### Two hazards found in code-critic review (issue #3683), both fixed here
//!
//! 1. **Panic-safe gate clearing.** The first version of this module cleared
//!    the in-flight gate via an ordinary trailing statement after the
//!    commit phases. A panic anywhere in those phases (a `Bm25Index` bug, an
//!    allocation failure, `metrics` macro panic, etc.) would unwind the
//!    detached task WITHOUT ever reaching that statement — tokio catches the
//!    panic at the task boundary (so the daemon doesn't crash), but
//!    `rehydrate_inflight` would stay `Some(dead_notify)` FOREVER, denying
//!    every future caller for the rest of the process's lifetime (the exact
//!    #3659/#3666 round-2 "opposite-polarity" bug recurring in a new guard).
//!    [`spawn_detached_rehydrate`] now constructs a real RAII guard
//!    ([`RehydrateGateClearOnDrop`]) BEFORE the blocking scan even starts, so
//!    the clear fires on every exit path — success, a handled error, OR an
//!    unwinding panic — exactly mirroring `open_guard::WedgeClearOnDrop`.
//! 2. **Evict-vs-rehydrate commit race.** A rehydrate's final step —
//!    clearing `chunks_evicted` / `bm25_entities_evicted` back to `false` —
//!    used to run unconditionally. `reclaim_memory_now` (issue #2846) is NOT
//!    idle-gated and can clear the very same maps + re-set those flags to
//!    `true` WHILE a rehydrate is mid-flight; the rehydrate's unconditional
//!    flag-clear would then silently undo that fresh eviction, breaking the
//!    `evicted == false` ⟹ "maps are populated" invariant with no
//!    self-healing path. [`CodeIndexer::rehydrate_generation`] is a
//!    monotonic counter bumped by every real clear; the detached task
//!    snapshots it before spawning and only commits the flag-clear if the
//!    counter is unchanged — a concurrent evict invalidates the commit
//!    instead of being silently overwritten.
//!
//!    **Round-3 review (remaining HIGH): the fix above still had a narrower
//!    load-then-store race.** Reading `rehydrate_generation` via a bare
//!    atomic `load`, then separately `store`-ing the `*_evicted` flags, left
//!    a window strictly BETWEEN those two operations for a concurrent
//!    evict's own bump-and-set to land — the commit's `store(false)` would
//!    then silently clobber that fresh `true` right back to `false`, the
//!    same bug through a narrower door. [`CodeIndexer::rehydrate_generation`]
//!    is now a `std::sync::Mutex<u64>`; both sides — the evict-side
//!    `{set flag = true; bump generation}` ([`CodeIndexer::clear_bm25_entities`],
//!    [`CodeIndexer::clear_in_memory_chunks`]) and the commit-side
//!    `{read generation; conditionally clear flags}` below — now hold that
//!    SAME lock across their entire read/bump/store sequence, so the two
//!    critical sections are mutually exclusive and the interleaving has no
//!    window left to land in.
//!
//! ### A third finding, addressed in `search/lanes.rs` + here (issue #3683)
//!
//! 3. **Silent degrade on a slow production cold-start (HIGH — "the
//!    killer").** `REHYDRATE_RACE_RETRIES * rehydrate_wait_budget()`
//!    (`search/lanes.rs`) is a deliberately bounded wait so an interactive
//!    query never blocks on an O(corpus) scan — but the production RCA that
//!    opened this issue measured 27-40s cold scans on a 315K-chunk corpus
//!    (deployment i-0076), and the wait budget shipped in slice 1
//!    ([`DEFAULT_REHYDRATE_WAIT_MS`] × 3 = ~12s) was comfortably LESS than
//!    that on every axis, so on that corpus every first cold query was
//!    *guaranteed* to exhaust its retries and silently degrade to an empty
//!    lexical lane with a 200 OK — trading a loud 408 for a quiet wrong
//!    answer. Two changes close this: (a) [`DEFAULT_REHYDRATE_WAIT_MS`] is
//!    raised so the worst case (3 × 9s = 27s) sits at the low end of the
//!    measured 27-40s window instead of well under it — narrowing, though
//!    (given a hard 30s request timeout) not eliminating, the fraction of
//!    cold queries that degrade; operators on the slow end of that range
//!    should raise `TRUSTY_REHYDRATE_WAIT_MS` and `TRUSTY_QUERY_TIMEOUT_SECS`
//!    together. (b) the degrade is now OBSERVABLE instead of silent: see
//!    [`CodeIndexer::lane_degraded`] and the `trusty_bm25_lane_degraded`
//!    gauge set by `search::lanes::bm25_search` /
//!    `grep_fallback_search`, surfaced in the search HTTP response's
//!    `meta.bm25_lane_degraded` field — mirroring how `WarmBootSummary`
//!    surfaces warm-boot degradation on `/health`. The gauge/flag reset to
//!    `false` in this module's commit-success branch below (finding 5:
//!    the "recovery" instant, so a stale degraded reading is never reported
//!    once the corpus is actually warm again).
//!
//! Test: `detached_rehydrate_survives_caller_cancellation`,
//! `rehydrate_dedupes_concurrent_callers_onto_one_scan`,
//! `rehydrate_is_deterministic_across_repeated_cycles_over_cap`,
//! `rehydrate_gate_clears_after_a_panic_in_the_commit_phase`,
//! `rehydrate_commit_skips_flag_clear_when_evict_races_it`,
//! `rehydrate_commit_survives_evict_racing_the_generation_critical_section`,
//! `lane_degraded_flag_sets_on_exhausted_retries_and_clears_on_rehydrate` in
//! `indexer::rehydrate_tests`.
//!
//! ## Cost-scaled idle-eviction window + oldest-idle-first sweep (issue #3683 slice 2)
//!
//! Why: slice 1 fixed what happens on a slow rehydrate (survives cancellation,
//! degrades observably) but deliberately left the eviction POLICY untouched —
//! the flat 60s idle window (issue #2166) and the registry's arbitrary
//! iteration order in `service::server::tickers::spawn_idle_chunk_eviction_ticker`.
//! That flat window is the actual root cause of the #3683 production
//! incident's query volume: it treated a 315K-chunk NFS-backed index
//! (27-40s rehydrate) identically to a handful-of-chunks index
//! (sub-millisecond rehydrate), so the expensive index was evicted and forced
//! to cold-start roughly once a minute even under light interactive traffic.
//!
//! What: [`CodeIndexer::rehydrate_cost_estimate_ms`] and
//! [`CodeIndexer::cost_scaled_idle_threshold`] (below) give each index its
//! OWN idle-eviction window, scaled by how expensive that specific index is
//! to rehydrate — see `helpers::scaled_idle_evict_threshold` for the formula.
//! [`spawn_detached_rehydrate`] now also records the wall-clock duration of
//! its own redb scan into [`super::CodeIndexer::last_rehydrate_cost_ms`], so
//! the scaling input becomes a real MEASUREMENT (not just a chunk-count
//! estimate) after an index's first rehydrate in a process's lifetime. The
//! sweep itself (`spawn_idle_chunk_eviction_ticker` /
//! `service::server::tickers::run_idle_eviction_tick`) now also processes
//! indexes OLDEST-IDLE-FIRST rather than the registry's arbitrary order —
//! see that function's doc comment for the ordering rationale.
//! Test: `rehydrate_cost_estimate_ms_prefers_measured_over_estimated`,
//! `cost_scaled_idle_threshold_scales_with_rehydrate_cost` in
//! `indexer::cost_scaled_threshold_tests`; `oldest_idle_first_orders_most_idle_index_first_ties_stable`,
//! `run_idle_eviction_tick_evicts_cheap_index_but_spares_costly_one` in
//! `service::server::idle_eviction_tests`.

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use tokio::sync::Notify;

use crate::core::bm25::Bm25Index;
use crate::core::chunker::RawChunk;
use crate::core::corpus::CorpusStore;
use crate::core::entity::RawEntity;

use super::CodeIndexer;

/// RAII guard clearing the per-index rehydrate gate + waking every waiter
/// when dropped — unconditionally, on ANY exit path of the detached
/// rehydrate task (issue #3683 code-critic review finding 1).
///
/// Why: mirrors `core::corpus::open_guard::WedgeClearOnDrop` for the
/// identical hazard class (issue #3659 round 3) — constructing the guard
/// right after installing the gate, BEFORE any fallible work runs, makes the
/// clear a structural guarantee (fires on early return, a future error path,
/// OR a panic unwinding through the commit phases) instead of resting on
/// "the code after the guard never panics", which was exactly the bug this
/// replaces: an earlier version cleared the gate via an ordinary trailing
/// statement, so a panic in the BM25/map commit phases left
/// `rehydrate_inflight` wedged at `Some(dead_notify)` forever.
/// What: holds the `Arc`s needed to clear the gate and notify; `Drop::drop`
/// is synchronous (this is why `rehydrate_inflight` is a `std::sync::Mutex`,
/// not a `tokio::sync::Mutex` — `Drop` cannot `.await`), so it can run
/// during a panicking unwind, where async code cannot.
/// Test: `rehydrate_gate_clears_after_a_panic_in_the_commit_phase` in
/// `indexer::rehydrate_tests`.
struct RehydrateGateClearOnDrop {
    gate: Arc<StdMutex<Option<Arc<Notify>>>>,
    notify: Arc<Notify>,
}

impl Drop for RehydrateGateClearOnDrop {
    fn drop(&mut self) {
        *self.gate.lock().unwrap_or_else(|e| e.into_inner()) = None;
        self.notify.notify_waiters();
    }
}

/// Test-only hook: inject a panic into the detached rehydrate task's commit
/// phase (issue #3683 code-critic review finding 1).
///
/// Why: proving the gate-clear survives a panic requires actually panicking
/// partway through Phase 1-3 — there is no production code path that does
/// this intentionally, so a dedicated test hook is the only way to exercise
/// the unwind deterministically.
/// What: when `true`, `spawn_detached_rehydrate` panics immediately after
/// the redb scan completes (i.e. inside the commit phase, after the
/// `RehydrateGateClearOnDrop` guard is already live), instead of running
/// Phase 1-3 normally. Reset to `false` by tests when done.
#[cfg(test)]
pub(crate) static TEST_PANIC_IN_COMMIT_PHASE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Test-only hook: pause AFTER Phase 1-3 have published fresh data but
/// BEFORE the generation-CAS commit check (issue #3683 code-critic review
/// finding 2).
///
/// Why: deterministically reproducing the evict-vs-rehydrate race requires
/// a reliable window in which a test can run a concurrent
/// `evict_bm25_entities_if_idle` / `evict_chunks_if_idle` call against
/// freshly-repopulated (therefore non-empty, therefore actually evictable)
/// structures, strictly between this task's own writes and its commit
/// check — a window normally microseconds wide.
/// What: an async `tokio::time::sleep` (not `std::thread::sleep` —
/// `spawn_detached_rehydrate`'s async block, not its `spawn_blocking`
/// closure) for this many milliseconds, gated to zero (a no-op) outside
/// tests.
#[cfg(test)]
pub(crate) static TEST_DELAY_BEFORE_COMMIT_MS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

/// Test-only hook: hold `rehydrate_generation`'s mutex across a SYNCHRONOUS
/// sleep, injected strictly BETWEEN the commit's generation load and its
/// conditional flag-stores (code-critic review round 3 — the remaining
/// HIGH after finding 2's fix).
///
/// Why: [`TEST_DELAY_BEFORE_COMMIT_MS`] above only covers a race landing
/// BEFORE the generation read (the commit hasn't taken the lock yet). The
/// round-3 finding is narrower and landed INSIDE the old
/// read-then-store gap that used to exist between the generation load and
/// the flag stores — proving THAT window is closed requires a test that
/// holds the lock across it and shows a concurrently-spawned evict call
/// genuinely blocks on the same mutex (not merely "happens to run later")
/// until the critical section finishes, so the evict's own bump-and-set can
/// never land in between.
/// What: `std::thread::sleep` (deliberately NOT `.await` — never block on
/// I/O/await while holding a `std::sync::Mutex` guard; this parks the
/// current OS worker thread for the duration, which is fine for a
/// `#[cfg(test)]`-only hook exercised under a multi-thread tokio runtime,
/// where a second worker thread keeps the racing evict task running) for
/// this many milliseconds while the commit branch's `generation_guard` is
/// held. Zero (the default) is a no-op. Requires the test's runtime to have
/// at least 2 worker threads (`#[tokio::test(flavor = "multi_thread", worker_threads = 2)]`)
/// so the concurrently-spawned evict call can actually attempt the lock
/// (and block on it) on a different OS thread while this one sleeps —
/// otherwise a single-threaded runtime would just serialize the two tasks
/// without ever exercising real contention.
/// Test: `rehydrate_commit_survives_evict_racing_the_generation_critical_section`
/// in `indexer::rehydrate_tests`.
#[cfg(test)]
pub(crate) static TEST_DELAY_IN_GENERATION_CRITICAL_SECTION_MS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

/// Restored (chunks, per-file entities) pair read from the durable corpus.
/// Named for readability at the `spawn_blocking` closure boundary, mirroring
/// `persist::RestoredCorpus`.
type RestoredCorpus = (Vec<RawChunk>, Vec<(String, Vec<RawEntity>)>);

/// Default bound on how long a SINGLE `ensure_corpus_rehydrated` call waits
/// for an in-flight (or newly-triggered) rehydrate before returning to let
/// the caller degrade (issue #3683 slice 1). Override via
/// `TRUSTY_REHYDRATE_WAIT_MS`.
///
/// Why: an interactive query lane must never burn its entire deadline on an
/// O(corpus) scan. `search/lanes.rs`'s `REHYDRATE_RACE_RETRIES` loop calls
/// `ensure_*_loaded()` up to 3 times, so the worst-case total wait before a
/// lane degrades to empty is bounded at roughly `3 * this value`.
///
/// Code-critic review finding 3 (issue #3683, HIGH): the value first shipped
/// here (4s, ~12s total) was comfortably LESS than the 27-40s cold-scan
/// latency this issue's own RCA measured on the production 315K-chunk corpus
/// (deployment i-0076) — meaning every first cold query on that corpus was
/// *guaranteed* to degrade, not merely at risk of it. 9s (~27s total) instead
/// sits at the low end of that measured window, leaving ~3s of the default
/// 30s `TRUSTY_QUERY_TIMEOUT_SECS` for the rest of the handler (embedding,
/// KG expansion, JSON serialization) — narrowing, though not eliminating,
/// how often a cold query on that specific corpus degrades. This is a
/// judgment call, not a guarantee: a corpus whose cold scan reliably exceeds
/// ~27s should raise `TRUSTY_REHYDRATE_WAIT_MS` (and, in lock-step,
/// `TRUSTY_QUERY_TIMEOUT_SECS`, or the wait budget just gets bounded by the
/// outer request timeout instead) rather than rely on this default. Either
/// way the degrade is no longer silent — see [`CodeIndexer::lane_degraded`]
/// and the `trusty_bm25_lane_degraded` gauge.
const DEFAULT_REHYDRATE_WAIT_MS: u64 = 9_000;

/// Test-only artificial delay injected into the detached rehydrate task's
/// blocking scan (issue #3683 slice 1).
///
/// Why: proving the detached task survives a CALLER's cancellation requires a
/// scan slow enough to still be running when the caller's own (much shorter)
/// timeout fires — a real redb scan is far too fast in a unit-test corpus of
/// a handful of chunks. This lets tests simulate the production 27-40s NFS
/// scan deterministically without a slow fixture.
/// What: milliseconds to `std::thread::sleep` on the blocking-pool thread
/// before running the real scan. Zero (the default) is a no-op.
#[cfg(test)]
pub(crate) static TEST_REHYDRATE_DELAY_MS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

/// Read `TRUSTY_REHYDRATE_WAIT_MS`, falling back to
/// [`DEFAULT_REHYDRATE_WAIT_MS`] when unset, zero, or unparsable.
fn rehydrate_wait_budget() -> Duration {
    std::env::var("TRUSTY_REHYDRATE_WAIT_MS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|&ms| ms > 0)
        .map(Duration::from_millis)
        .unwrap_or(Duration::from_millis(DEFAULT_REHYDRATE_WAIT_MS))
}

impl CodeIndexer {
    /// Drop the in-memory BM25 corpus and per-file entity map when the index
    /// has been idle longer than `idle_threshold` and a durable corpus can
    /// repopulate them.
    ///
    /// Why: see the module docs — BM25's `doc_terms` is a full tokenized
    /// second copy of every chunk's text, permanently resident regardless of
    /// query activity once an index has been queried or ingested once.
    /// What: a no-op when `idle_threshold` is zero, no durable corpus is
    /// wired, BM25 is already empty (nothing to reclaim — also covers the
    /// "already evicted" case since eviction empties it), or the index was
    /// recently active. Otherwise replaces `self.bm25` with a fresh empty
    /// index and clears `self.entities`, marks `bm25_entities_evicted`, and
    /// logs an `info` with the reclaimed BM25 document count. Chunks and the
    /// symbol graph are untouched by this method — they have their own
    /// (chunks) or no (symbol graph) eviction path.
    /// Returns the number of BM25 documents evicted (0 when skipped).
    /// Test: `bm25_entities_idle_eviction_drops_and_lazily_rehydrates`.
    pub async fn evict_bm25_entities_if_idle(&self, idle_threshold: Duration) -> usize {
        if idle_threshold.is_zero() {
            return 0;
        }
        if self.idle_duration() < idle_threshold {
            return 0;
        }
        let evicted = self.clear_bm25_entities().await;
        if evicted > 0 {
            tracing::info!(
                "index '{}': evicted {} in-memory BM25 documents + entities after {}s idle \
                 (durable corpus retained; lazily rehydrates on next access)",
                self.index_id,
                evicted,
                idle_threshold.as_secs(),
            );
        }
        evicted
    }

    /// Unconditionally drop the in-memory BM25 corpus + per-file entity map (no
    /// idle guard, no logging). Shared by the idle-evict path
    /// ([`Self::evict_bm25_entities_if_idle`]) and the memory-pressure reclaim
    /// path ([`Self::reclaim_memory_now`], issue #2846).
    ///
    /// Why: `BM25Index::doc_terms` is a full tokenized second copy of every
    /// chunk's text and, with the entity map, one of the largest anonymous-heap
    /// consumers a warm-booted index carries. Both callers need the identical
    /// clear-and-mark; factoring it here keeps the two paths from drifting.
    /// What: a no-op returning 0 when no durable corpus is wired or BM25 is
    /// already empty. Otherwise replaces `self.bm25` with a fresh empty index,
    /// clears `self.entities`, marks `bm25_entities_evicted`, and returns the
    /// reclaimed BM25 document count. Callers own any logging.
    /// Test: exercised by `bm25_entities_idle_eviction_drops_and_lazily_rehydrates`
    /// (idle path) and `memory_pressure_reclaim_now_clears_caches` (pressure path).
    async fn clear_bm25_entities(&self) -> usize {
        if self.corpus.is_none() {
            return 0;
        }
        let mut bm25 = self.bm25.write().await;
        if bm25.is_empty() {
            return 0;
        }
        let evicted = bm25.len();
        *bm25 = Bm25Index::new();
        drop(bm25);

        let mut entities = self.entities.write().await;
        entities.clear();
        entities.shrink_to_fit();
        drop(entities);

        // Issue #3683 round-3 review (remaining HIGH): the flag-set and the
        // generation bump MUST happen under the same lock acquisition as a
        // single critical section — see `CodeIndexer::rehydrate_generation`'s
        // doc comment. Without this, a concurrent rehydrate commit could read
        // the pre-bump generation, get preempted, have THIS eviction land
        // (bump + set `true`), then resume and clear the flag back to `false`
        // over the now-empty maps this eviction just cleared. A short,
        // never-awaited-while-held std Mutex critical section (no I/O, no
        // `.await` inside it).
        {
            let mut generation = self
                .rehydrate_generation
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            self.bm25_entities_evicted.store(true, Ordering::Relaxed);
            *generation += 1;
        }
        evicted
    }

    /// Force-reclaim this index's evictable in-memory heap **regardless of idle
    /// state** — the steady-state memory-limit enforcement path (issue #2846).
    ///
    /// Why: the idle-evict ticker only reclaims indexes that have been quiet
    /// past the idle window, so a daemon under genuine memory pressure whose
    /// indexes are all "recently active" keeps growing until the OS OOM-killer
    /// intervenes — exactly the production failure #2846 reports (RSS reached
    /// 2.2× the configured 12 GB soft ceiling). When the pressure ticker sees
    /// RSS cross the high-water mark it calls this on the indexes selected by
    /// `service::server::tickers::run_pressure_sweep` (issue #3683 slice 2 —
    /// a budgeted, oldest-idle-first, recency-exempt-unless-desperate subset,
    /// NOT unconditionally every resident index) to shed the largest
    /// anonymous-heap consumers. Every structure cleared here is 100%
    /// recoverable from the durable redb corpus and lazily rehydrates on next
    /// access, so a pressure reclaim is non-destructive to the DATA. It is,
    /// however, a genuine race against any concurrent query: firing under
    /// active load (unlike idle-evict, which only ever races this window
    /// after the index's own idle threshold has elapsed) can land strictly
    /// between a racing `bm25_search` / `grep_fallback_search` call's own
    /// `ensure_*_loaded()`
    /// check and its read-lock acquisition. Both of those lanes now detect and
    /// retry that exact window (issue #2846 PR review — see their doc
    /// comments in `search/lanes.rs`), so the worst case for a racing query is
    /// a re-load latency spike (redb rehydrate before the retry's read),
    /// not a silently-lost lexical/grep lane — strictly preferable to an
    /// OOM-kill either way.
    /// What: unconditionally clears the in-memory chunk map, the BM25 corpus,
    /// and the per-file entity map, then demotes the HNSW vector store back to
    /// its mmap view (best-effort — a demote failure is logged at debug and
    /// never fatal). Returns the total in-memory entry count reclaimed (the sum
    /// of cleared chunks and BM25 documents). An index without a durable corpus
    /// (BM25-only / test) reclaims nothing (returns 0) — it has no source to
    /// rehydrate from, so clearing it would be data loss, not cache eviction.
    /// Test: `memory_pressure_reclaim_now_clears_caches` and
    /// `memory_pressure_reclaim_now_is_noop_without_corpus` in `indexer::tests`.
    pub async fn reclaim_memory_now(&self) -> usize {
        let mut reclaimed = self.clear_in_memory_chunks().await;
        reclaimed += self.clear_bm25_entities().await;
        if let Some(store) = &self.store {
            // Deliberately bypasses `hnsw_review_idle_enabled()` (unlike
            // `demote_vector_store_if_idle`): under active memory pressure,
            // reclaiming heap takes priority over an operator's idle-demotion
            // opt-out — `demote_to_view` itself still refuses to demote a
            // store with unpersisted writes, so this stays safe either way.
            if let Err(e) = store.demote_to_view().await {
                tracing::debug!(
                    "index '{}': memory-pressure HNSW demote-to-view failed ({e}); \
                     leaving heap-resident",
                    self.index_id
                );
            }
        }
        reclaimed
    }

    /// Repopulate the BM25 corpus and per-file entity map from the durable
    /// corpus if they were previously evicted while idle.
    ///
    /// Why: query lanes (`bm25_search`, `entity_exact_match`, `entities_for`)
    /// and ingest commit paths (`commit_parsed_batch`, `add_chunk_inner`)
    /// read or mutate `self.bm25` / `self.entities` directly; after an idle
    /// eviction both are empty and `bm25_entities_evicted` is set. Every call
    /// site guards itself the same way `ensure_chunks_loaded` call sites do,
    /// rather than relying on a single upstream gate.
    /// What (issue #3683 slice 1): thin wrapper delegating to
    /// [`Self::ensure_corpus_rehydrated`] — see that method and the module
    /// docs for the detached-task design that replaced this method's former
    /// inline scan.
    /// Test: `bm25_entities_idle_eviction_drops_and_lazily_rehydrates`;
    /// `detached_rehydrate_survives_caller_cancellation`.
    pub(super) async fn ensure_bm25_entities_loaded(&self) {
        self.ensure_corpus_rehydrated().await;
    }

    /// Repopulate the in-memory `chunks` map, BM25 corpus, and per-file entity
    /// map from the durable corpus in ONE detached, deduplicated scan, if any
    /// of them were previously evicted while idle (issue #3683 slice 1,
    /// #3684). Backs both [`Self::ensure_chunks_loaded`] and
    /// [`Self::ensure_bm25_entities_loaded`] — see the module docs for why
    /// they're safe to consolidate (both flags are always set together by
    /// the same idle-evict tick).
    ///
    /// Why: see the module docs' "Detached, deduplicated rehydrate" section
    /// — this is the core livelock fix. The old design ran the scan AND the
    /// map-publish/flag-clear inline inside whatever future called it; a
    /// query-timeout cancellation of that future discarded completed work.
    /// What: a fast no-op (two relaxed atomic loads) when nothing is
    /// evicted. Otherwise either joins an already-in-flight rehydrate for
    /// this index (deduping concurrent callers onto ONE scan) or becomes the
    /// leader and spawns [`spawn_detached_rehydrate`] as an independent
    /// `tokio::spawn` task, then waits up to [`rehydrate_wait_budget`] for a
    /// completion notification. Returns either way — on a bounded-wait
    /// timeout the flags are typically still set, so the caller's own
    /// `bm25_search`/`grep_fallback_search` retry loop
    /// (`REHYDRATE_RACE_RETRIES` in `search/lanes.rs`) will call this again
    /// and rejoin the same still-running task rather than triggering a
    /// second scan.
    ///
    /// Accepted race (documented, not a correctness bug): a caller that reads
    /// `Some(notify)` under the gate lock just before the detached task
    /// finishes and clears the gate can, in a vanishingly narrow window,
    /// construct its `notify.notified()` wait AFTER the task's
    /// `notify_waiters()` call already fired — missing that specific wakeup.
    /// The caller then simply waits out its full bounded budget before
    /// returning; the data is already committed by then (the task clears the
    /// `*_evicted` flags BEFORE it clears the gate / notifies), so the
    /// caller's own subsequent flag/emptiness check proceeds with fresh data
    /// exactly as if it had woken instantly — the only cost is one caller
    /// occasionally waiting the full budget instead of returning early.
    /// Test: `detached_rehydrate_survives_caller_cancellation`,
    /// `rehydrate_dedupes_concurrent_callers_onto_one_scan` in
    /// `indexer::cost_scaled_threshold_tests`.
    pub(super) async fn ensure_corpus_rehydrated(&self) {
        if !self.chunks_evicted.load(Ordering::Relaxed)
            && !self.bm25_entities_evicted.load(Ordering::Relaxed)
        {
            return;
        }
        let Some(corpus) = self.corpus.clone() else {
            // Defensive: eviction requires a wired corpus, so this shouldn't
            // happen in practice, but a missing corpus has nothing to
            // rehydrate from — just clear both flags rather than spin.
            self.chunks_evicted.store(false, Ordering::Relaxed);
            self.bm25_entities_evicted.store(false, Ordering::Relaxed);
            return;
        };

        let notify = {
            // Synchronous std-mutex lock (issue #3683 finding 1 — see
            // `rehydrate_inflight`'s doc comment for why): a short,
            // never-awaited-while-held critical section.
            let mut gate = self
                .rehydrate_inflight
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            match gate.clone() {
                Some(existing) => existing,
                None => {
                    let fresh = Arc::new(Notify::new());
                    *gate = Some(Arc::clone(&fresh));
                    drop(gate);
                    // Snapshot BEFORE spawning (issue #3683 finding 2): any
                    // clear_bm25_entities/clear_in_memory_chunks call that
                    // lands from here on invalidates this attempt's commit.
                    // A momentary race between this read and a concurrent
                    // evict bumping the counter around the same instant is
                    // fine either way — whichever value we snapshot, the
                    // commit-side compare (round-3 review) is what actually
                    // decides correctness, not this read.
                    let generation_at_start = *self
                        .rehydrate_generation
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    self.spawn_detached_rehydrate(corpus, Arc::clone(&fresh), generation_at_start);
                    fresh
                }
            }
        };

        // Bounded wait only — never let an interactive caller block on the
        // full O(corpus) scan. Whether this returns because the task
        // notified us or because the budget elapsed, the task itself is
        // untouched: it is not owned by this future and keeps running.
        let _ = tokio::time::timeout(rehydrate_wait_budget(), notify.notified()).await;
    }

    /// Spawn the detached rehydrate task itself (issue #3683 slice 1; panic
    /// safety + evict-race hardening from code-critic review round 2).
    ///
    /// Why: split out of `ensure_corpus_rehydrated` so the leader branch
    /// stays readable; also the natural place to hang the full "why detached"
    /// rationale referenced by the module docs.
    /// What: `tokio::spawn`s a task — NOT awaited by the caller, so it is
    /// never cancelled by a caller's own timeout. A [`RehydrateGateClearOnDrop`]
    /// guard is constructed FIRST, before any fallible work, so the gate
    /// clears on every exit path including a panic (finding 1). The task then
    /// runs `CorpusStore::load_all_chunks` + `load_all_entities` on
    /// `spawn_blocking`, sorts the chunks by their stable `id` (issue #3684 —
    /// makes which subset survives the BM25 cap deterministic across
    /// rehydrate cycles), then commits BM25 (via
    /// `Bm25Index::upsert_document_reporting`, logging a per-rebuild
    /// dropped-count instead of `upsert_document`'s process-wide log-once
    /// latch), the chunk map, and the entity map — in that order, mirroring
    /// `persist::load_chunks_from_redb`'s phase ordering (BM25 published
    /// before chunks, so a concurrent reader never observes chunks without a
    /// matching lexical lane). The final flag-clear only commits if
    /// `rehydrate_generation` is unchanged from `generation_at_start` —
    /// otherwise a concurrent evict/reclaim raced this rehydrate and the
    /// commit is skipped (finding 2), leaving `*_evicted` exactly as that
    /// newer clear left it.
    /// Test: `detached_rehydrate_survives_caller_cancellation`,
    /// `rehydrate_is_deterministic_across_repeated_cycles_over_cap`,
    /// `rehydrate_gate_clears_after_a_panic_in_the_commit_phase`,
    /// `rehydrate_commit_skips_flag_clear_when_evict_races_it`.
    fn spawn_detached_rehydrate(
        &self,
        corpus: Arc<CorpusStore>,
        notify: Arc<Notify>,
        generation_at_start: u64,
    ) {
        let index_id = self.index_id.clone();
        let chunks_map = Arc::clone(&self.chunks);
        let bm25 = Arc::clone(&self.bm25);
        let entities_map = Arc::clone(&self.entities);
        let chunks_evicted = Arc::clone(&self.chunks_evicted);
        let bm25_entities_evicted = Arc::clone(&self.bm25_entities_evicted);
        let inflight_gate = Arc::clone(&self.rehydrate_inflight);
        let rehydrate_generation = Arc::clone(&self.rehydrate_generation);
        let lane_degraded = Arc::clone(&self.lane_degraded);
        let last_rehydrate_cost_ms = Arc::clone(&self.last_rehydrate_cost_ms);

        tokio::spawn(async move {
            // Finding 1: constructed BEFORE any fallible work. Its `Drop`
            // clears `inflight_gate` + notifies waiters unconditionally, on
            // ANY exit from this async block — normal return, an early
            // `return`-equivalent, or a panic unwinding through everything
            // below. Do not remove/reorder this — see the struct's doc
            // comment and the module docs' "hazards" section.
            let _clear_guard = RehydrateGateClearOnDrop {
                gate: inflight_gate,
                notify,
            };

            // Issue #3683 slice 2: wall-clock the scan itself (not the whole
            // detached task, which also includes lock acquisition + map
            // publish below) so `last_rehydrate_cost_ms` reflects the actual
            // O(corpus) cost that drives `cost_scaled_idle_threshold`, not
            // incidental commit-phase overhead.
            let scan_started = Instant::now();
            let loaded = tokio::task::spawn_blocking(move || -> Result<RestoredCorpus> {
                #[cfg(test)]
                {
                    let delay_ms = TEST_REHYDRATE_DELAY_MS.load(Ordering::Relaxed);
                    if delay_ms > 0 {
                        std::thread::sleep(Duration::from_millis(delay_ms));
                    }
                }
                let mut chunks = corpus.load_all_chunks()?;
                // Issue #3684: sort by the stable chunk id BEFORE the
                // cap-truncated BM25 upsert loop below, so the surviving
                // subset of an over-cap corpus is the same every rehydrate —
                // independent of redb's B-tree iteration order, which shifts
                // as keys are added/removed between cycles.
                chunks.sort_by(|a, b| a.id.cmp(&b.id));
                let entities = corpus.load_all_entities()?;
                Ok((chunks, entities))
            })
            .await;
            let scan_elapsed_ms =
                scan_started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;

            #[cfg(test)]
            if TEST_PANIC_IN_COMMIT_PHASE.load(Ordering::Relaxed) {
                panic!("test-injected panic in rehydrate commit phase (issue #3683 finding 1)");
            }

            match loaded {
                Ok(Ok((chunks, entities))) => {
                    let n_chunks = chunks.len();
                    let n_files = entities.len();

                    // Issue #3683 slice 2: record the MEASURED scan cost so
                    // the next idle-eviction tick's `cost_scaled_idle_threshold`
                    // uses a real measurement instead of the chunk-count
                    // estimate — see `CodeIndexer::rehydrate_cost_estimate_ms`.
                    last_rehydrate_cost_ms.store(scan_elapsed_ms, Ordering::Relaxed);

                    // Phase 1: BM25, in the deterministic id-sorted order,
                    // reporting (not silently swallowing) cap drops.
                    let mut dropped = 0usize;
                    {
                        let mut bm25_guard = bm25.write().await;
                        for chunk in &chunks {
                            let text = CodeIndexer::bm25_doc_text(chunk);
                            if !bm25_guard.upsert_document_reporting(&chunk.id, &text) {
                                dropped += 1;
                            }
                        }
                    }
                    // Finding 5: always report the current dropped count —
                    // including zero — so the gauge doesn't hold a stale
                    // nonzero value from a PRIOR rehydrate after a later one
                    // drops nothing (e.g. the corpus shrank under the cap).
                    metrics::gauge!("trusty_bm25_docs_dropped", "index" => index_id.clone())
                        .set(dropped as f64);
                    if dropped > 0 {
                        tracing::warn!(
                            corpus_size = n_chunks,
                            dropped,
                            "index '{index_id}': BM25 corpus cap reached during rehydrate — \
                             {dropped} of {n_chunks} chunks are not lexically searchable \
                             (issue #3684; override with TRUSTY_BM25_CORPUS_CAP)"
                        );
                    }

                    // Phase 2: chunk map.
                    {
                        let mut map = chunks_map.write().await;
                        for chunk in chunks {
                            map.insert(chunk.id.clone(), chunk);
                        }
                    }

                    // Phase 3: per-file entities.
                    {
                        let mut emap = entities_map.write().await;
                        for (file, ents) in entities {
                            emap.insert(file, ents);
                        }
                    }

                    #[cfg(test)]
                    {
                        let delay_ms = TEST_DELAY_BEFORE_COMMIT_MS.load(Ordering::Relaxed);
                        if delay_ms > 0 {
                            tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                        }
                    }

                    // Commit (finding 2; hardened round-3 — remaining HIGH):
                    // only clear the flags if no concurrent evict/reclaim
                    // landed since this attempt started. The generation
                    // compare and the flag-clear now happen under ONE lock
                    // acquisition on `rehydrate_generation` (see that field's
                    // doc comment) — the earlier version read the counter via
                    // a bare atomic load, then stored the flags as separate
                    // operations, leaving a window in which a concurrent
                    // evict's own bump-and-set could land strictly between
                    // the read and the stores and get silently clobbered.
                    // Holding the lock across BOTH the compare and (when it
                    // matches) the flag-clears makes that interleaving
                    // impossible: either the evict's critical section runs
                    // entirely before this one (generation_now mismatches,
                    // we skip) or entirely after (our clears already
                    // committed and released the lock before the evict's
                    // `true` can land, so its `true` is the state that
                    // survives — never torn). A generation mismatch means a
                    // NEWER clear already ran (and re-set `*_evicted = true`,
                    // possibly over data this task just wrote) — skip the
                    // flag-clear so the next access re-triggers a fresh
                    // rehydrate instead of us silently overwriting that newer
                    // state.
                    let generation_guard = rehydrate_generation
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    let generation_now = *generation_guard;
                    // Round-3 test hook: see this static's doc comment — lets
                    // a test hold this exact lock across an injected delay
                    // placed STRICTLY BETWEEN the generation load above and
                    // the conditional flag-stores below, to prove a
                    // concurrent evict attempting the same critical section
                    // blocks (rather than racing in) until this one
                    // completes and releases the lock.
                    #[cfg(test)]
                    {
                        let delay_ms =
                            TEST_DELAY_IN_GENERATION_CRITICAL_SECTION_MS.load(Ordering::Relaxed);
                        if delay_ms > 0 {
                            std::thread::sleep(Duration::from_millis(delay_ms));
                        }
                    }
                    if generation_now == generation_at_start {
                        chunks_evicted.store(false, Ordering::Relaxed);
                        bm25_entities_evicted.store(false, Ordering::Relaxed);
                        // Finding 5: this commit is the "recovery" instant —
                        // the corpus is genuinely warm again, so clear the
                        // degraded signal here rather than waiting for some
                        // future query to notice, which would report a stale
                        // "degraded" reading for however long until the next
                        // query happens to land.
                        lane_degraded.store(false, Ordering::Relaxed);
                        metrics::gauge!("trusty_bm25_lane_degraded", "index" => index_id.clone())
                            .set(0.0);
                        tracing::info!(
                            "index '{index_id}': rehydrated {n_chunks} chunks + BM25 + \
                             {n_files} file entity lists from redb after idle eviction \
                             (detached task; survives caller cancellation, issue #3683)"
                        );
                    } else {
                        tracing::warn!(
                            generation_at_start,
                            generation_now,
                            "index '{index_id}': rehydrate commit SKIPPED — a concurrent \
                             evict/reclaim landed mid-rehydrate; leaving *_evicted set so the \
                             next access retriggers a fresh rehydrate (issue #3683 finding 2)"
                        );
                    }
                    drop(generation_guard);
                }
                Ok(Err(e)) => tracing::warn!(
                    "index '{index_id}': failed to rehydrate corpus from redb ({e}); \
                     will retry on next access"
                ),
                Err(e) => tracing::warn!(
                    "index '{index_id}': corpus rehydration task panicked ({e}); \
                     will retry on next access"
                ),
            }

            // `_clear_guard` drops here (or during an unwind above),
            // unconditionally clearing the gate and waking every waiter.
        });
    }

    /// Demote this index's HNSW vector store back to mmap-view mode when it
    /// has been idle longer than `idle_threshold` (issue #2164).
    ///
    /// Why: the #709 mmap-view optimization keeps a warm-booted HNSW index
    /// pageable, but any write promotes it to a full heap copy
    /// (`UsearchStore::ensure_mutable`) with no path back — so in practice
    /// almost every index a daemon ever writes to stays heap-resident
    /// forever, even long after it goes idle. This reclaims that heap the
    /// same way `evict_chunks_if_idle` / `evict_bm25_entities_if_idle`
    /// reclaim theirs, riding the same idle window and the same ticker tick
    /// so there is exactly one idle sweep, not a competing second one.
    /// What: a no-op when `idle_threshold` is zero, the
    /// [`crate::core::store_config::hnsw_review_idle_enabled`] env gate is
    /// off, no vector store is wired (BM25-only mode), or the index was
    /// recently active. Otherwise delegates to
    /// [`crate::core::store::VectorStore::demote_to_view`], which is the
    /// authoritative gate on safety (only demotes a store that is currently
    /// mutable AND has no unpersisted writes — see
    /// `UsearchStore::try_demote_to_view`). Logs an `info` on an actual
    /// demotion, a `warn` on failure (never fatal — demotion is an
    /// optimization). Returns `true` when a demotion happened.
    /// Test: `hnsw_idle_demotion_reviews_clean_promoted_store` and
    /// `hnsw_idle_demotion_skips_when_disabled_via_env` in `indexer::tests`.
    pub async fn demote_vector_store_if_idle(&self, idle_threshold: Duration) -> bool {
        if idle_threshold.is_zero() {
            return false;
        }
        if !crate::core::store_config::hnsw_review_idle_enabled() {
            return false;
        }
        let Some(store) = &self.store else {
            return false;
        };
        if self.idle_duration() < idle_threshold {
            return false;
        }
        match store.demote_to_view().await {
            Ok(true) => {
                tracing::info!(
                    "index '{}': demoted HNSW vector store to mmap-view after {}s idle",
                    self.index_id,
                    idle_threshold.as_secs(),
                );
                true
            }
            Ok(false) => false,
            Err(e) => {
                tracing::warn!(
                    "index '{}': HNSW demote-to-view failed ({e}); leaving heap-resident",
                    self.index_id
                );
                false
            }
        }
    }

    /// Estimate this index's rehydrate cost in milliseconds (issue #3683
    /// slice 2), used to scale the idle-eviction window — see
    /// [`Self::cost_scaled_idle_threshold`].
    ///
    /// Why: a flat idle-eviction window treats a 315K-chunk NFS-backed index
    /// (27-40s to rehydrate) identically to a handful-of-chunks index
    /// (sub-millisecond rehydrate) — see the #3683 production RCA. Scaling
    /// the window by how expensive THIS index is to rehydrate means the
    /// eviction sweep only thrashes indexes it can cheaply afford to
    /// re-warm.
    /// What: returns the most recently MEASURED rehydrate duration
    /// ([`super::CodeIndexer::last_rehydrate_cost_ms`], set by
    /// [`spawn_detached_rehydrate`] after a real redb scan) when nonzero.
    /// Otherwise ESTIMATES from the durable corpus's on-disk chunk count — a
    /// cheap O(1) redb metadata read (`CorpusStore::chunk_count`, not a
    /// scan — the same call `files::enumerate_chunks_after` already makes
    /// inline, without `spawn_blocking`) — via
    /// `helpers::estimate_rehydrate_cost_ms`. Returns `0` (no scaling) for an
    /// index with no durable corpus wired (nothing to rehydrate, nothing to
    /// cost).
    /// Test: `rehydrate_cost_estimate_ms_prefers_measured_over_estimated`,
    /// `rehydrate_cost_estimate_ms_falls_back_to_corpus_chunk_count_estimate`
    /// in `indexer::cost_scaled_threshold_tests`.
    pub(crate) fn rehydrate_cost_estimate_ms(&self) -> u64 {
        let measured = self.last_rehydrate_cost_ms.load(Ordering::Relaxed);
        if measured > 0 {
            return measured;
        }
        let Some(corpus) = &self.corpus else {
            return 0;
        };
        let chunk_count = corpus.chunk_count().unwrap_or(0) as u64;
        super::helpers::estimate_rehydrate_cost_ms(chunk_count)
    }

    /// This index's cost-scaled idle-eviction window (issue #3683 slice 2) —
    /// see `helpers::scaled_idle_evict_threshold` for the formula and
    /// [`Self::rehydrate_cost_estimate_ms`] for the cost input. Called once
    /// per index, per tick, by
    /// `service::server::tickers::run_idle_eviction_tick` in place of the
    /// single flat `idle_evict_secs()` window every index used to share.
    /// Test: `cost_scaled_idle_threshold_scales_with_rehydrate_cost` in
    /// `indexer::cost_scaled_threshold_tests`.
    pub(crate) fn cost_scaled_idle_threshold(&self, base_secs: u64) -> Duration {
        super::helpers::scaled_idle_evict_threshold(base_secs, self.rehydrate_cost_estimate_ms())
    }
}

#[cfg(test)]
impl CodeIndexer {
    /// Test-only: directly set the measured rehydrate-cost estimate (issue
    /// #3683 slice 2), bypassing an actual redb scan so tests can exercise
    /// cost-scaled idle-threshold behaviour without constructing a
    /// realistically large (hundreds-of-thousands-of-chunks) corpus fixture.
    pub(crate) fn set_rehydrate_cost_ms_for_test(&self, ms: u64) {
        self.last_rehydrate_cost_ms.store(ms, Ordering::Relaxed);
    }
}
