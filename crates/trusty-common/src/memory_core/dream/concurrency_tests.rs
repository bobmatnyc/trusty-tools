//! Regression tests for the #7106 dream herd: concurrency cap, first-tick
//! stagger, and chunked embedding.
//!
//! Why: each test below fails on the pre-#7106 code and passes after. They are
//! kept out of `tests.rs` (1904 SLOC of an inherited 3000 cap) so the file that
//! covers the dream passes' behaviour stays readable.
//! What: permit-cap tests, the ten-palace peak-concurrency test, the stagger
//! formula and the loop that honours it, and the two dedup chunking tests.
//! Test: itself.

use super::concurrency::{
    DEFAULT_DREAM_MAX_CONCURRENT, DreamCycleGauge, acquire_dream_permit, dream_cycles_in_flight,
    dream_cycles_peak_in_flight, dream_max_concurrent, parse_max_concurrent, stagger_offset,
};
use super::config::DreamConfig;
use super::cycle::{DREAM_EMBED_CHUNK, dedup_pass_with_embedder};
use super::dreamer::Dreamer;
use crate::embedder::MockEmbedder;
use crate::memory_core::embed::{EMBED_DIM, Embedder};
use crate::memory_core::palace::{Drawer, Palace, PalaceId, RoomType};
use crate::memory_core::retrieval::{PalaceHandle, seed_shared_embedder_with_mock};
use crate::memory_core::semantic_consolidation::SemanticConsolidationConfig;
use anyhow::Result;
use async_trait::async_trait;
use chrono::Utc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tempfile::tempdir;
use uuid::Uuid;

// ── Fixtures ─────────────────────────────────────────────────────────────────

/// Open a real, empty palace handle backed by a leaked tempdir.
///
/// Why: every test here needs a `PalaceHandle`, and the mock embedder must be
/// seeded first so no HuggingFace download is attempted (issue #850).
/// What: mirrors `tests::open_test_handle`; the tempdir is leaked because these
/// tests are short and process-exit cleanup is enough.
/// Test: used by the tests below.
async fn open_handle(name: &str) -> Arc<PalaceHandle> {
    seed_shared_embedder_with_mock();
    let dir = tempdir().expect("tempdir");
    let palace = Palace {
        id: PalaceId::new(name),
        name: name.into(),
        description: None,
        created_at: Utc::now(),
        data_dir: dir.path().join(name),
    };
    std::fs::create_dir_all(&palace.data_dir).expect("palace data dir");
    let handle = PalaceHandle::open(&palace).expect("open palace");
    std::mem::forget(dir);
    handle
}

/// A [`DreamConfig`] that touches no network and does no on-disk compaction.
///
/// Why: these tests measure scheduling and memory shape, not consolidation
/// quality. Leaving the semantic phase on would read the machine's ambient
/// `OPENROUTER_API_KEY` and issue live calls — see `tests::dedup_only_config`.
/// What: default config with the semantic phase, the recall benchmark, and the
/// kg.redb compaction all off.
/// Test: used by the tests below.
fn quiet_config() -> DreamConfig {
    DreamConfig {
        semantic: SemanticConsolidationConfig {
            enabled: false,
            ..Default::default()
        },
        recall_benchmark_enabled: false,
        compact: false,
        ..DreamConfig::default()
    }
}

/// Push `count` distinct drawers straight into the palace's drawer table.
///
/// Why: the chunking tests need hundreds of drawers and only read the drawer
/// table; routing each one through `remember` would embed and index it, which
/// costs far more than the test measures.
/// What: appends `Drawer::new` rows under the write lock. No vectors are
/// written, so the dedup pass finds no neighbours and merges nothing — exactly
/// what these tests want, since they assert on the embed calls.
/// Test: used by `dedup_embeds_in_bounded_chunks`.
fn seed_drawers(handle: &Arc<PalaceHandle>, count: usize) {
    let room_id = Uuid::new_v4();
    let mut drawers = handle.drawers.write();
    for i in 0..count {
        drawers.push(Drawer::new(
            room_id,
            format!("drawer number {i} of {count}"),
        ));
    }
}

/// An [`Embedder`] that records the size of every batch it is handed.
///
/// Why: the #7106 contract is about batch SHAPE — no call larger than
/// [`DREAM_EMBED_CHUNK`], and the budget checked before each call is paid for.
/// Neither is observable from the pass's return value.
/// What: delegates to [`MockEmbedder`] after recording `texts.len()` and
/// optionally sleeping, so a test can make each chunk cost a known amount of
/// the cycle budget.
/// Test: `dedup_embeds_in_bounded_chunks`,
/// `the_cycle_budget_stops_dedup_between_chunks`.
struct RecordingEmbedder {
    inner: MockEmbedder,
    batches: Arc<Mutex<Vec<usize>>>,
    per_batch_delay: Duration,
}

impl RecordingEmbedder {
    fn new(per_batch_delay: Duration) -> Self {
        Self {
            inner: MockEmbedder::new(EMBED_DIM),
            batches: Arc::new(Mutex::new(Vec::new())),
            per_batch_delay,
        }
    }

    fn recorded(&self) -> Vec<usize> {
        self.batches.lock().expect("batch log not poisoned").clone()
    }
}

#[async_trait]
impl Embedder for RecordingEmbedder {
    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        self.batches
            .lock()
            .expect("batch log not poisoned")
            .push(texts.len());
        if !self.per_batch_delay.is_zero() {
            tokio::time::sleep(self.per_batch_delay).await;
        }
        self.inner.embed_batch(texts).await
    }

    fn dimension(&self) -> usize {
        self.inner.dimension()
    }
}

// ── (1) Concurrency cap ──────────────────────────────────────────────────────

/// Why: the cap's whole job is to keep N cycles' working sets from coexisting.
/// What: spawns 20 tasks that each hold a permit while a shared counter tracks
/// the live holders and their high-water mark; asserts the mark never exceeds
/// the configured cap.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dream_permits_cap_concurrent_holders() {
    let cap = dream_max_concurrent();
    let live = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));

    let mut joins = Vec::new();
    for _ in 0..20 {
        let live = Arc::clone(&live);
        let peak = Arc::clone(&peak);
        joins.push(tokio::spawn(async move {
            let _permit = acquire_dream_permit().await;
            let now = live.fetch_add(1, Ordering::SeqCst) + 1;
            peak.fetch_max(now, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(5)).await;
            live.fetch_sub(1, Ordering::SeqCst);
        }));
    }
    for join in joins {
        join.await.expect("permit task");
    }

    assert!(
        peak.load(Ordering::SeqCst) <= cap,
        "at most {cap} permit holders at once; observed {}",
        peak.load(Ordering::SeqCst)
    );
    assert_eq!(
        live.load(Ordering::SeqCst),
        0,
        "every permit released; a leak would leave holders counted"
    );
}

/// Why: the herd this fixes was 61 palaces' cycles running at once. This is
/// that scenario at test scale — ten palaces, all released together, with the
/// production in-flight gauge as the witness.
/// What: opens ten palaces, releases ten `dream_cycle` calls from one barrier
/// so they enter together, and asserts the process-wide peak never exceeded the
/// configured cap. The gauge is incremented inside `dream_cycle` independently
/// of the permit, so removing the permit leaves the gauge reporting the real
/// (uncapped) overlap and this test fails.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ten_palaces_never_exceed_the_concurrency_cap() {
    const PALACES: usize = 10;
    let cap = dream_max_concurrent();

    let mut handles = Vec::new();
    for i in 0..PALACES {
        let handle = open_handle(&format!("herd-palace-{i}")).await;
        seed_drawers(&handle, 300);
        handles.push(handle);
    }

    let barrier = Arc::new(tokio::sync::Barrier::new(PALACES));
    let mut joins = Vec::new();
    for handle in handles {
        let barrier = Arc::clone(&barrier);
        joins.push(tokio::spawn(async move {
            let dreamer = Dreamer::new(quiet_config());
            barrier.wait().await;
            dreamer.dream_cycle(&handle).await.expect("dream cycle");
        }));
    }
    for join in joins {
        join.await.expect("cycle task");
    }

    let peak = dream_cycles_peak_in_flight();
    assert!(
        peak <= cap,
        "{PALACES} palaces released together must never run more than {cap} cycles at once; \
         peak in flight was {peak}"
    );
    assert!(peak >= 1, "the in-flight gauge never moved: {peak}");
    // The gauge is process-wide and other tests in this binary run cycles
    // concurrently, so its instantaneous value is not this test's to assert on.
    // `the_in_flight_gauge_releases_on_drop` covers the release.
}

/// Why: the cap is only real if a cycle actually waits when the slots are gone.
/// A gauge that merely never exceeded the cap could also mean nothing ran.
/// What: holds every permit, starts one `dream_cycle`, asserts it has not
/// finished after 300 ms, then releases the permits and asserts it completes.
/// Pre-fix the cycle ignores the permits and finishes immediately.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_dream_cycle_waits_when_every_permit_is_held() {
    let handle = open_handle("permit-wait").await;
    let cap = dream_max_concurrent();

    let mut held = Vec::new();
    for _ in 0..cap {
        held.push(acquire_dream_permit().await);
    }

    let done = Arc::new(AtomicUsize::new(0));
    let done_task = Arc::clone(&done);
    let join = tokio::spawn(async move {
        let dreamer = Dreamer::new(quiet_config());
        dreamer.dream_cycle(&handle).await.expect("dream cycle");
        done_task.store(1, Ordering::SeqCst);
    });

    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        done.load(Ordering::SeqCst),
        0,
        "a dream cycle must queue behind the concurrency cap, not run through it"
    );

    drop(held);
    // Generous: other tests in this binary compete for the same permits.
    tokio::time::timeout(Duration::from_secs(60), join)
        .await
        .expect("cycle finished once a permit freed up")
        .expect("cycle task");
    assert_eq!(done.load(Ordering::SeqCst), 1);
}

/// Why: the parse decides which spellings disable the bound, and an accidental
/// `0` would mean an unbounded semaphore — the very state this module prevents.
/// What: pins the honoured, defaulted, and warned cases.
/// Test: itself.
#[test]
fn parse_max_concurrent_rejects_junk_and_zero() {
    assert_eq!(parse_max_concurrent(Some("4")), 4);
    assert_eq!(parse_max_concurrent(Some(" 8 ")), 8);
    assert_eq!(parse_max_concurrent(None), DEFAULT_DREAM_MAX_CONCURRENT);
    assert_eq!(parse_max_concurrent(Some("")), DEFAULT_DREAM_MAX_CONCURRENT);
    assert_eq!(
        parse_max_concurrent(Some("0")),
        DEFAULT_DREAM_MAX_CONCURRENT
    );
    assert_eq!(
        parse_max_concurrent(Some("lots")),
        DEFAULT_DREAM_MAX_CONCURRENT
    );
    assert_eq!(
        parse_max_concurrent(Some("-3")),
        DEFAULT_DREAM_MAX_CONCURRENT
    );
}

/// Why: lock the shipped default so raising it is a deliberate edit.
#[test]
fn the_default_cap_is_two() {
    assert_eq!(DEFAULT_DREAM_MAX_CONCURRENT, 2);
}

/// Why: the two readings the operator sees must be consistent — an in-flight
/// count above the peak would mean one of them is wrong.
/// What: holds a gauge and checks the in-flight count includes it and stays at
/// or below the recorded peak. The gauge is process-wide and other tests in
/// this binary run cycles concurrently, so only monotone facts are asserted;
/// the release itself is proven by `dream_permits_cap_concurrent_holders`,
/// which would deadlock its twenty tasks if a slot were never returned.
#[test]
fn the_in_flight_gauge_counts_a_held_cycle() {
    let held = DreamCycleGauge::enter();
    let during = dream_cycles_in_flight();
    assert!(during >= 1, "a held gauge counts at least itself: {during}");
    assert!(
        dream_cycles_peak_in_flight() >= during,
        "the peak is never below a count it has already seen"
    );
    drop(held);
}

// ── (2) First-tick stagger ───────────────────────────────────────────────────

/// Why: 61 loops sleeping the same interval from the same instant is what put
/// 61 `dream_stats.json` writes inside one 31-second window.
/// What: asserts the offsets for ten palaces are all distinct, strictly
/// increasing, start at zero, stay inside the interval, and match the stated
/// `interval * k / n` formula.
/// Test: itself.
#[test]
fn stagger_offsets_spread_first_ticks_across_the_interval() {
    let interval = Duration::from_secs(300);
    let total = 10;
    let offsets: Vec<Duration> = (0..total)
        .map(|k| stagger_offset(k, total, interval))
        .collect();

    assert_eq!(
        offsets[0],
        Duration::ZERO,
        "the first palace is not delayed"
    );
    for (k, offset) in offsets.iter().enumerate() {
        assert_eq!(
            *offset,
            Duration::from_secs(30 * k as u64),
            "palace {k} of {total} waits interval * k / n"
        );
        assert!(
            *offset < interval,
            "an offset never reaches a full interval"
        );
    }
    for pair in offsets.windows(2) {
        assert!(pair[0] < pair[1], "offsets strictly increase: {pair:?}");
    }

    // Degenerate inputs answer zero rather than panicking.
    assert_eq!(stagger_offset(0, 1, interval), Duration::ZERO);
    assert_eq!(stagger_offset(0, 0, interval), Duration::ZERO);
    assert_eq!(stagger_offset(9, 3, interval), Duration::ZERO);
}

/// Why: the formula is only worth anything if the loop actually waits it.
/// What: spawns one loop with `idle_secs = 1` and a 3-second stagger, and
/// asserts no cycle has run 1.6 s in — well past the un-staggered first tick at
/// 1 s. Pre-fix the loop ignores the offset and `dream_stats.json` exists by
/// then.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_dream_loop_waits_its_stagger_before_the_first_cycle() {
    let handle = open_handle("stagger-loop").await;
    let data_dir = handle
        .data_dir
        .clone()
        .expect("palace handle has a data dir");
    let id = handle.id.clone();
    let registry = crate::memory_core::PalaceRegistry::new();
    registry.register_arc(handle);

    let dreamer = Arc::new(Dreamer::new(DreamConfig {
        idle_secs: 1,
        ..quiet_config()
    }));
    let (tx, rx) = tokio::sync::watch::channel(false);
    let join = dreamer.start_with_shutdown(registry, id, Duration::from_secs(3), rx);

    tokio::time::sleep(Duration::from_millis(1_600)).await;
    let stats = super::config::PersistedDreamStats::load(&data_dir).expect("load stats");
    assert!(
        stats.is_none(),
        "a staggered loop must not have run its first cycle 1.6s in \
         (un-staggered it fires at 1.0s)"
    );

    let _ = tx.send(true);
    let _ = tokio::time::timeout(Duration::from_secs(2), join).await;
}

// ── (3) Chunked embedding ────────────────────────────────────────────────────

/// Why: the dedup pass embedded a palace's whole corpus in one call, so the
/// transient scaled with palace size and nothing capped it.
/// What: seeds 600 drawers, runs the pass with a generous budget, and asserts
/// the recorded batch sizes are exactly `[256, 256, 88]` — three calls, none
/// larger than [`DREAM_EMBED_CHUNK`]. Pre-fix there is one call of 600.
/// Test: itself.
#[tokio::test]
async fn dedup_embeds_in_bounded_chunks() {
    let handle = open_handle("chunked-dedup").await;
    seed_drawers(&handle, 600);

    let embedder = RecordingEmbedder::new(Duration::ZERO);
    dedup_pass_with_embedder(
        &handle,
        Instant::now(),
        Duration::from_secs(60),
        0.95,
        &embedder,
    )
    .await
    .expect("dedup pass");

    let batches = embedder.recorded();
    assert_eq!(
        batches,
        vec![
            DREAM_EMBED_CHUNK,
            DREAM_EMBED_CHUNK,
            600 - 2 * DREAM_EMBED_CHUNK
        ],
        "600 drawers must embed as bounded chunks, not one whole-corpus call"
    );
    assert!(
        batches.iter().all(|n| *n <= DREAM_EMBED_CHUNK),
        "no batch may exceed the chunk size: {batches:?}"
    );
}

/// Why: the budget check used to sit AFTER the single whole-corpus embed, so a
/// cycle could overrun `max_cycle_ms` by minutes in one unbreakable step.
/// What: gives each chunk a 200 ms cost and the pass a 50 ms budget, then
/// asserts exactly one chunk of 256 was embedded — the budget stopped the pass
/// between chunks rather than after everything. Pre-fix the single call embeds
/// all 600 regardless of the budget.
/// Test: itself.
#[tokio::test]
async fn the_cycle_budget_stops_dedup_between_chunks() {
    let handle = open_handle("budget-dedup").await;
    seed_drawers(&handle, 600);

    let embedder = RecordingEmbedder::new(Duration::from_millis(200));
    dedup_pass_with_embedder(
        &handle,
        Instant::now(),
        Duration::from_millis(50),
        0.95,
        &embedder,
    )
    .await
    .expect("dedup pass");

    assert_eq!(
        embedder.recorded(),
        vec![DREAM_EMBED_CHUNK],
        "the budget must stop the pass between chunks, after exactly one \
         chunk-sized embed"
    );
}

/// Why: chunking must not change what the pass does — a drawer that used to be
/// deduped still must be.
/// What: writes two identical drawers through `remember` (so they are embedded
/// and indexed) and asserts the pass still merges them.
/// Test: itself.
#[tokio::test]
async fn chunking_preserves_dedup_behaviour() {
    let handle = open_handle("chunk-behaviour").await;
    for importance in [0.7_f32, 0.6] {
        handle
            .remember(
                "Rust uses HNSW for vector search".into(),
                RoomType::Backend,
                vec!["rust".into()],
                importance,
            )
            .await
            .expect("remember");
    }
    assert_eq!(handle.drawers.read().len(), 2);

    let embedder = RecordingEmbedder::new(Duration::ZERO);
    let merged = dedup_pass_with_embedder(
        &handle,
        Instant::now(),
        Duration::from_secs(60),
        0.95,
        &embedder,
    )
    .await
    .expect("dedup pass");

    assert_eq!(merged, 1, "two identical drawers still collapse to one");
    assert_eq!(handle.drawers.read().len(), 1);
    assert_eq!(embedder.recorded(), vec![2], "one chunk holds both drawers");
}
