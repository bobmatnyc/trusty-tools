//! The detached corpus rehydrate task and its test hooks (#3683, #10193).
//!
//! Why: split out of `idle_evict.rs` so that file stays under the 500-SLOC
//! cap (#10193). #10193 also moves the BM25 build off the live index's write
//! guard: the build runs on the blocking pool beside the redb scan, and the
//! live index is replaced by a brief swap.
//! What: [`CodeIndexer::spawn_detached_rehydrate`], the
//! [`RehydrateGateClearOnDrop`] guard, the off-lock [`build_bm25`] /
//! [`publish_bm25`] pair, and the `cfg(test)` hooks the rehydrate tests arm.
//! Test: `indexer::rehydrate_tests`, `idle_evict::bm25_off_lock_tests`.

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex as StdMutex};
#[cfg(test)]
use std::time::Duration;
use std::time::Instant;

use anyhow::Result;
use tokio::sync::Notify;

use crate::core::bm25::CodeBm25Index;
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
/// What: holds the keys of the indexers armed via [`CommitPhasePanicArmed`];
/// an armed indexer's `spawn_detached_rehydrate` panics immediately after the
/// redb scan completes (inside the commit phase, after the
/// `RehydrateGateClearOnDrop` guard is already live) instead of running
/// Phase 1-3. A key is the address of the indexer's `rehydrate_inflight` gate.
// #9513: keyed per indexer, not a process-global bool — a global flag panicked
// every concurrent test's rehydrate (e.g. `warm_corpus_rehydrates_an_evicted_index`).
#[cfg(test)]
static TEST_PANIC_IN_COMMIT_PHASE: StdMutex<Vec<usize>> = StdMutex::new(Vec::new());

/// Test-only: arms the commit-phase panic for ONE indexer until dropped.
/// Test: `commit_phase_panic_injection_reaches_only_the_armed_indexer`,
/// `rehydrate_gate_clears_after_a_panic_in_the_commit_phase`.
#[cfg(test)]
pub(crate) struct CommitPhasePanicArmed(usize);

#[cfg(test)]
impl CommitPhasePanicArmed {
    /// Arm the injected commit-phase panic for `indexer` only.
    pub(crate) fn arm(indexer: &CodeIndexer) -> Self {
        let key = Arc::as_ptr(&indexer.rehydrate_inflight) as usize;
        armed_commit_panics().push(key);
        Self(key)
    }
}

#[cfg(test)]
impl Drop for CommitPhasePanicArmed {
    fn drop(&mut self) {
        let mut armed = armed_commit_panics();
        if let Some(pos) = armed.iter().position(|k| *k == self.0) {
            armed.swap_remove(pos);
        }
    }
}

#[cfg(test)]
fn armed_commit_panics() -> std::sync::MutexGuard<'static, Vec<usize>> {
    TEST_PANIC_IN_COMMIT_PHASE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

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

/// What one blocking rehydrate produced: the id-sorted chunks, the
/// per-file entities, and a BM25 index built from those chunks off every
/// lock (#10193), with its refused-document count.
struct RestoredCorpus {
    chunks: Vec<RawChunk>,
    entities: Vec<(String, Vec<RawEntity>)>,
    bm25: CodeBm25Index,
    bm25_dropped: usize,
    /// #10195: milliseconds per blocking stage — scan, sort, entities, BM25.
    stage_ms: [u64; 4],
}

/// Milliseconds since `lap`, then restart `lap` (#10195).
fn lap_ms(lap: &mut Instant) -> u64 {
    let now = Instant::now();
    let ms = u64::try_from(now.saturating_duration_since(*lap).as_millis()).unwrap_or(u64::MAX);
    *lap = now;
    ms
}

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

/// Test-only per-indexer count of started rehydrate scans.
///
/// Why: `rehydrate_dedupes_concurrent_callers_onto_one_scan` must observe how
/// many scans ran, not how long they took. Keyed by the indexer's
/// `rehydrate_inflight` address (as the #9513 panic hook is) so a concurrent
/// test's scan never reaches another test's count.
/// What: `(gate address, scans started)` pairs. A key exists only while a
/// [`RehydrateScanCounter`] guard for it is alive; the blocking scan bumps an
/// armed key and ignores any other. Compiled out of non-test builds.
/// Test: `rehydrate_dedupes_concurrent_callers_onto_one_scan`.
// #10015: scan counter replaces the wall-clock dedupe assertion. Arm/Drop (not a
// never-cleared map) so a later indexer reusing a freed address sees no stale count.
#[cfg(test)]
static TEST_REHYDRATE_SCAN_COUNTS: StdMutex<Vec<(usize, u64)>> = StdMutex::new(Vec::new());

#[cfg(test)]
fn rehydrate_scan_counts() -> std::sync::MutexGuard<'static, Vec<(usize, u64)>> {
    TEST_REHYDRATE_SCAN_COUNTS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
fn record_rehydrate_scan(key: usize) {
    if let Some((_, n)) = rehydrate_scan_counts().iter_mut().find(|(k, _)| *k == key) {
        *n += 1;
    }
}

/// Test-only: counts rehydrate scans of ONE indexer from `arm` until dropped.
/// Test: `rehydrate_dedupes_concurrent_callers_onto_one_scan`.
#[cfg(test)]
pub(crate) struct RehydrateScanCounter(usize);

#[cfg(test)]
impl RehydrateScanCounter {
    /// Start counting scans of `indexer` from zero.
    pub(crate) fn arm(indexer: &CodeIndexer) -> Self {
        let key = Arc::as_ptr(&indexer.rehydrate_inflight) as usize;
        let mut counts = rehydrate_scan_counts();
        counts.retain(|(k, _)| *k != key);
        counts.push((key, 0));
        Self(key)
    }

    /// Scans of the armed indexer started since `arm`.
    pub(crate) fn count(&self) -> u64 {
        rehydrate_scan_counts()
            .iter()
            .find(|(k, _)| *k == self.0)
            .map_or(0, |(_, n)| *n)
    }
}

#[cfg(test)]
impl Drop for RehydrateScanCounter {
    fn drop(&mut self) {
        rehydrate_scan_counts().retain(|(k, _)| *k != self.0);
    }
}

/// Test-only: delays the off-lock BM25 build of ONE indexer until dropped
/// (#10193).
///
/// Why: the regression test needs a BM25 build slow enough that a query
/// lands while it runs; the scan delay ([`TEST_REHYDRATE_DELAY_MS`]) sits
/// before the scan, not inside the build, and is process-global.
/// What: `(gate address, delay ms)` pairs, keyed like the #9513 panic hook.
/// The blocking rehydrate sleeps that long immediately before it builds BM25.
/// Test: `a_query_during_a_slow_bm25_build_answers_degraded_within_the_wait`.
#[cfg(test)]
static TEST_BM25_BUILD_DELAYS: StdMutex<Vec<(usize, u64)>> = StdMutex::new(Vec::new());

#[cfg(test)]
fn bm25_build_delays() -> std::sync::MutexGuard<'static, Vec<(usize, u64)>> {
    TEST_BM25_BUILD_DELAYS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// Test-only: arms a BM25-build delay for one indexer until dropped.
/// Test: `a_query_during_a_slow_bm25_build_answers_degraded_within_the_wait`.
#[cfg(test)]
pub(crate) struct Bm25BuildDelayArmed(usize);

#[cfg(test)]
impl Bm25BuildDelayArmed {
    /// Delay every BM25 build of `indexer` by `delay`.
    pub(crate) fn arm(indexer: &CodeIndexer, delay: Duration) -> Self {
        let key = Arc::as_ptr(&indexer.rehydrate_inflight) as usize;
        let mut delays = bm25_build_delays();
        delays.retain(|(k, _)| *k != key);
        delays.push((key, delay.as_millis() as u64));
        Self(key)
    }
}

#[cfg(test)]
impl Drop for Bm25BuildDelayArmed {
    fn drop(&mut self) {
        bm25_build_delays().retain(|(k, _)| *k != self.0);
    }
}

#[cfg(test)]
fn sleep_if_bm25_build_delayed(key: usize) {
    let delay_ms = bm25_build_delays()
        .iter()
        .find(|(k, _)| *k == key)
        .map_or(0, |(_, ms)| *ms);
    if delay_ms > 0 {
        std::thread::sleep(Duration::from_millis(delay_ms));
    }
}

/// Test-only state of one armed [`RehydrateScanPause`].
#[cfg(test)]
#[derive(Default)]
struct ScanPauseState {
    scanned: Notify,
    released: StdMutex<bool>,
    wake: std::sync::Condvar,
}

#[cfg(test)]
impl ScanPauseState {
    fn release(&self) {
        *self.released.lock().unwrap_or_else(|e| e.into_inner()) = true;
        self.wake.notify_all();
    }
}

/// Test-only per-indexer scan pauses, keyed like the #9513 panic hook.
#[cfg(test)]
static TEST_SCAN_PAUSES: StdMutex<Vec<(usize, Arc<ScanPauseState>)>> = StdMutex::new(Vec::new());

#[cfg(test)]
fn scan_pauses() -> std::sync::MutexGuard<'static, Vec<(usize, Arc<ScanPauseState>)>> {
    TEST_SCAN_PAUSES.lock().unwrap_or_else(|e| e.into_inner())
}

/// Test-only: holds ONE indexer's rehydrate after its redb read, before its
/// publish, until released or dropped (#10209).
///
/// Why: the #10209 regression tests issue a write inside that window and
/// must know, without a sleep, that the scan already read the old rows.
/// What: the blocking scan signals [`Self::scanned`] after reading redb, then
/// blocks until [`Self::release`]. Dropping the guard releases it, so a
/// failed test never strands a blocking thread.
/// Test: `indexer::rehydrate_writes_tests`.
#[cfg(test)]
pub(crate) struct RehydrateScanPause {
    key: usize,
    state: Arc<ScanPauseState>,
}

#[cfg(test)]
impl RehydrateScanPause {
    /// Pause `indexer`'s next rehydrate after its redb read.
    pub(crate) fn arm(indexer: &CodeIndexer) -> Self {
        let key = Arc::as_ptr(&indexer.rehydrate_inflight) as usize;
        let state = Arc::new(ScanPauseState::default());
        let mut pauses = scan_pauses();
        pauses.retain(|(k, _)| *k != key);
        pauses.push((key, Arc::clone(&state)));
        Self { key, state }
    }

    /// Resolves once the paused rehydrate has read redb.
    pub(crate) async fn scanned(&self) {
        self.state.scanned.notified().await;
    }

    /// Let the paused rehydrate publish.
    pub(crate) fn release(&self) {
        self.state.release();
    }
}

#[cfg(test)]
impl Drop for RehydrateScanPause {
    fn drop(&mut self) {
        scan_pauses().retain(|(k, _)| *k != self.key);
        self.state.release();
    }
}

#[cfg(test)]
fn pause_if_scan_paused(key: usize) {
    let state = scan_pauses()
        .iter()
        .find(|(k, _)| *k == key)
        .map(|(_, s)| Arc::clone(s));
    let Some(state) = state else {
        return;
    };
    state.scanned.notify_one();
    let mut released = state.released.lock().unwrap_or_else(|e| e.into_inner());
    while !*released {
        released = state.wake.wait(released).unwrap_or_else(|e| e.into_inner());
    }
}

/// Build a BM25 index from id-sorted `chunks`, counting refused documents.
///
/// Why: #10193 — the rehydrate used to upsert every chunk into the LIVE
/// index under its write guard, on an async worker, so every query and
/// writer waited out the whole build.
/// What: upserts `chunks` in the order given into a fresh index — the same
/// order and cap as the old in-place loop over the emptied live index, so
/// the surviving subset of an over-cap corpus is unchanged (#3684). Returns
/// the index and the count `upsert_document_reporting` refused.
/// Test: `rehydrated_bm25_holds_the_pre_eviction_doc_set`,
/// `rehydrate_is_deterministic_across_repeated_cycles_over_cap`.
fn build_bm25(chunks: &[RawChunk]) -> (CodeBm25Index, usize) {
    let mut bm25 = CodeBm25Index::new();
    let mut dropped = 0usize;
    for chunk in chunks {
        let text = CodeIndexer::bm25_doc_text(chunk);
        if !bm25.upsert_document_reporting(&chunk.id, &text) {
            dropped += 1;
        }
    }
    (bm25, dropped)
}

/// Publish an off-lock BM25 build into the live index (#10193).
///
/// Why: the swap is the only step that needs the write guard, so the guard
/// is held for a pointer swap instead of the whole build.
/// What: when the live index is empty (the evicted state), swaps `built` in
/// and returns `built_dropped`. When a writer upserted into the live index
/// during the build, keeps that write by re-applying `chunks` onto the live
/// index under the guard (the pre-#10193 in-place merge) and returns that
/// pass's refused count. The displaced index is dropped after the guard is
/// released.
/// Test: `a_write_landing_during_the_bm25_build_survives_the_swap`.
async fn publish_bm25(
    live: &tokio::sync::RwLock<CodeBm25Index>,
    built: CodeBm25Index,
    built_dropped: usize,
    chunks: &[RawChunk],
) -> usize {
    let mut guard = live.write().await;
    if guard.is_empty() {
        let displaced = std::mem::replace(&mut *guard, built);
        drop(guard);
        drop(displaced);
        return built_dropped;
    }
    let mut dropped = 0usize;
    for chunk in chunks {
        let text = CodeIndexer::bm25_doc_text(chunk);
        if !guard.upsert_document_reporting(&chunk.id, &text) {
            dropped += 1;
        }
    }
    drop(guard);
    drop(built);
    dropped
}

impl CodeIndexer {
    /// Spawn the detached rehydrate task itself (issue #3683 slice 1; panic
    /// safety + evict-race hardening from code-critic review round 2; BM25
    /// built off the write lock, #10193).
    ///
    /// Why: split out of `ensure_corpus_rehydrated` so the leader branch
    /// stays readable. #10193: the BM25 build used to hold the live index's
    /// write guard for its whole duration, so a cold query blocked behind it.
    /// What: `tokio::spawn`s a task — NOT awaited by the caller, so it is
    /// never cancelled by a caller's own timeout. A [`RehydrateGateClearOnDrop`]
    /// guard is constructed FIRST, before any fallible work, so the gate
    /// clears on every exit path including a panic (finding 1). On
    /// `spawn_blocking` the task runs `CorpusStore::load_all_chunks` +
    /// `load_all_entities`, sorts the chunks by their stable `id` (#3684), and
    /// builds a fresh BM25 index from them ([`build_bm25`]). It then commits,
    /// in order: BM25 (a brief swap, [`publish_bm25`]), the chunk map, and the
    /// entity map — BM25 before chunks, mirroring
    /// `persist::load_chunks_from_redb`. The final flag-clear only commits if
    /// `rehydrate_generation` is unchanged from `generation_at_start` —
    /// otherwise a concurrent evict/reclaim raced this rehydrate and the
    /// commit is skipped (finding 2), leaving `*_evicted` exactly as that
    /// newer clear left it.
    /// Test: `detached_rehydrate_survives_caller_cancellation`,
    /// `rehydrate_is_deterministic_across_repeated_cycles_over_cap`,
    /// `rehydrate_gate_clears_after_a_panic_in_the_commit_phase`,
    /// `rehydrate_commit_skips_flag_clear_when_evict_races_it`,
    /// `a_query_during_a_slow_bm25_build_answers_degraded_within_the_wait`.
    pub(super) fn spawn_detached_rehydrate(
        &self,
        corpus: Arc<CorpusStore>,
        notify: Arc<Notify>,
        generation_at_start: u64,
    ) {
        let index_id = self.index_id.clone();
        let chunks_map = Arc::clone(&self.chunks);
        let bm25_live = Arc::clone(&self.bm25);
        let entities_map = Arc::clone(&self.entities);
        let chunks_evicted = Arc::clone(&self.chunks_evicted);
        let bm25_entities_evicted = Arc::clone(&self.bm25_entities_evicted);
        let inflight_gate = Arc::clone(&self.rehydrate_inflight);
        #[cfg(test)]
        let commit_panic_key = Arc::as_ptr(&inflight_gate) as usize;
        let rehydrate_generation = Arc::clone(&self.rehydrate_generation);
        let lane_degraded = Arc::clone(&self.lane_degraded);
        // #5917: the two failure arms below used to report an unreadable corpus
        // to the log only, leaving every lane to answer from empty maps.
        let corpus_read_fault = Arc::clone(&self.corpus_read_fault);
        let last_rehydrate_cost_ms = Arc::clone(&self.last_rehydrate_cost_ms);
        let scan_gate = Arc::clone(&self.corpus_scan_gate);

        tokio::spawn(async move {
            // Finding 1: constructed BEFORE any fallible work. Its `Drop`
            // clears `inflight_gate` + notifies waiters unconditionally, on
            // ANY exit from this async block — normal return, an early
            // `return`-equivalent, or a panic unwinding through everything
            // below. Do not remove/reorder this.
            let _clear_guard = RehydrateGateClearOnDrop {
                gate: inflight_gate,
                notify,
            };
            // #10110: scan and publish under the gate, so a batch removal
            // never deletes rows this scan already read. #10209: the write
            // side, so a single-file write (read side, `rehydrate_write_fence`)
            // lands before the scan or after the publish, never between.
            let _scan_gate = scan_gate.write().await;

            // #10192: time the whole rehydrate (scan, sort, BM25, maps,
            // commit) for `cost_scaled_idle_threshold`. Starts after the
            // scan-gate wait, which is contention, not cost.
            let rehydrate_started = Instant::now();
            let loaded = tokio::task::spawn_blocking(move || -> Result<RestoredCorpus> {
                #[cfg(test)]
                {
                    record_rehydrate_scan(commit_panic_key);
                    let delay_ms = TEST_REHYDRATE_DELAY_MS.load(Ordering::Relaxed);
                    if delay_ms > 0 {
                        std::thread::sleep(Duration::from_millis(delay_ms));
                    }
                }
                let mut lap = Instant::now();
                let mut chunks = corpus.load_all_chunks()?;
                let scan_ms = lap_ms(&mut lap);
                // Issue #3684: sort by the stable chunk id BEFORE the
                // cap-truncated BM25 build, so the surviving subset of an
                // over-cap corpus is the same every rehydrate.
                chunks.sort_by(|a, b| a.id.cmp(&b.id));
                let sort_ms = lap_ms(&mut lap);
                let entities = corpus.load_all_entities()?;
                let entities_ms = lap_ms(&mut lap);
                #[cfg(test)]
                pause_if_scan_paused(commit_panic_key);
                // #10193: build BM25 here, off the live index's write guard
                // and off the async workers.
                #[cfg(test)]
                sleep_if_bm25_build_delayed(commit_panic_key);
                let (bm25, bm25_dropped) = build_bm25(&chunks);
                let bm25_ms = lap_ms(&mut lap);
                Ok(RestoredCorpus {
                    chunks,
                    entities,
                    bm25,
                    bm25_dropped,
                    stage_ms: [scan_ms, sort_ms, entities_ms, bm25_ms],
                })
            })
            .await;

            #[cfg(test)]
            if armed_commit_panics().contains(&commit_panic_key) {
                panic!("test-injected panic in rehydrate commit phase (issue #3683 finding 1)");
            }

            match loaded {
                Ok(Ok(restored)) => {
                    let RestoredCorpus {
                        chunks,
                        entities,
                        bm25: built,
                        bm25_dropped,
                        stage_ms: [scan_ms, sort_ms, entities_ms, bm25_ms],
                    } = restored;
                    // #10195: the publish-and-commit phase, timed from here.
                    let mut commit_lap = Instant::now();
                    let n_chunks = chunks.len();
                    let n_files = entities.len();

                    // Phase 1: BM25 — #10193: a brief swap, not the build.
                    let dropped = publish_bm25(&bm25_live, built, bm25_dropped, &chunks).await;
                    // Finding 5: always report the current dropped count —
                    // including zero — so the gauge doesn't hold a stale
                    // nonzero value from a PRIOR rehydrate.
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

                    // Commit (finding 2; hardened round-3): the generation
                    // compare and the flag-clear happen under ONE lock
                    // acquisition on `rehydrate_generation`, so a concurrent
                    // evict's bump-and-set runs entirely before (we skip) or
                    // entirely after (its `true` survives) — never torn. A
                    // mismatch means a NEWER clear already ran; skip the
                    // flag-clear so the next access re-triggers a rehydrate.
                    let generation_guard = rehydrate_generation
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    let generation_now = *generation_guard;
                    // Round-3 test hook: holds this exact lock across a delay
                    // STRICTLY BETWEEN the generation load and the stores.
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
                        // #5917: this scan read the whole corpus, so any fault
                        // recorded by an earlier failed read is over.
                        corpus_read_fault.clear();
                        // Finding 5: this commit is the "recovery" instant, so
                        // the degraded signal clears here, not on a later query.
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

                    // #10192: store the whole-rehydrate wall time once, at the
                    // end of the success arm. A skipped commit still did the
                    // full work, so its cost counts; the failure arms store
                    // nothing, so a failed read cannot lower the window.
                    let rehydrate_ms = rehydrate_started
                        .elapsed()
                        .as_millis()
                        .min(u128::from(u64::MAX)) as u64;
                    last_rehydrate_cost_ms.store(rehydrate_ms, Ordering::Relaxed);
                    // #10195: the per-stage split beside the whole-rehydrate time.
                    let commit_ms = lap_ms(&mut commit_lap);
                    tracing::info!(
                        index_id = %index_id,
                        rehydrate_ms,
                        scan_ms,
                        sort_ms,
                        entities_ms,
                        bm25_build_ms = bm25_ms,
                        commit_ms,
                        chunks = n_chunks,
                        "index '{index_id}': rehydrate cost {rehydrate_ms} ms recorded \
                         for idle-evict scaling"
                    );
                }
                // #5917: record the fault, not just a log line. Both arms leave
                // `*_evicted` set, so every lane answers from an empty map; the
                // record is what makes `search_with_drops` refuse instead. A
                // later successful rehydrate clears it above.
                Ok(Err(e)) => {
                    corpus_read_fault.record(format!("{e:#}"));
                    tracing::warn!(
                        "index '{index_id}': failed to rehydrate corpus from redb ({e:#}); \
                         will retry on next access"
                    );
                }
                Err(e) => {
                    corpus_read_fault.record(format!("rehydrate task panicked: {e}"));
                    tracing::warn!(
                        "index '{index_id}': corpus rehydration task panicked ({e}); \
                         will retry on next access"
                    );
                }
            }

            // `_clear_guard` drops here (or during an unwind above),
            // unconditionally clearing the gate and waking every waiter.
        });
    }
}
