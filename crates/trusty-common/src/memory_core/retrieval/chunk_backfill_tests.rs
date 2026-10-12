//! #9735 PR 1: the chunk backfill slice, its progress view and its marker.
//!
//! Why: the slice writes vectors while writers, forgets and failures race it;
//! each test pins one rule that a wrong mechanism would break.
//! What: disk palaces with legacy single-row drawers, driven with chunking
//! mocks (window 12: ten words per chunk).
//! Test: this file IS the tests.

use super::chunk_backfill::{SliceBounds, SliceStop};
use super::chunk_health::{ChunkCheck, Sightings, first_sighting};
use super::chunk_progress::{CHUNK_MARKER_FILE, ChunkMarker, ChunkState};
use super::embed_store_tests::{Counting, DIM, NoTokenizer, handle, shape, words};
use super::handle::PalaceHandle;
use crate::embedder::{ChunkError, ChunkTokenizer, MockEmbedder};
use crate::memory_core::embed::Embedder;
use crate::memory_core::palace::Drawer;
use crate::memory_core::store::embed_ledger;
use anyhow::Result;
use async_trait::async_trait;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Notify;
use uuid::Uuid;

const WINDOW: usize = 12;
type Emb = Arc<dyn Embedder + Send + Sync>;

/// Add a drawer of `text`, created `age` ago, with `importance`.
fn add_aged(h: &PalaceHandle, text: &str, age: Duration, importance: f32) -> Uuid {
    let mut d = Drawer::new(Uuid::new_v4(), text);
    d.created_at -= chrono::Duration::from_std(age).unwrap();
    d.importance = importance;
    let id = d.id;
    h.add_drawer(d);
    id
}

/// A long drawer whose only row is the bare key (pre-chunking shape).
async fn legacy(h: &PalaceHandle, text: &str, importance: f32) -> Uuid {
    let id = add_aged(h, text, Duration::from_secs(3600), importance);
    h.vector_store
        .upsert_chunks(id, vec![vec![1.0; DIM]])
        .await
        .unwrap();
    id
}

fn chunking() -> Emb {
    Arc::new(Counting::chunking(WINDOW))
}

fn failing() -> Emb {
    let mut c = Counting::chunking(WINDOW);
    c.fail = true;
    Arc::new(c)
}

fn full(h: &PalaceHandle, id: Uuid) -> bool {
    shape(h, id).is_some_and(|(rows, _)| rows > 1)
}

/// Blocks every multi-chunk `embed_batch` until `release` is notified.
struct Gated {
    inner: MockEmbedder,
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

fn gated() -> (Emb, Arc<Notify>, Arc<Notify>) {
    let (entered, release) = (Arc::new(Notify::new()), Arc::new(Notify::new()));
    let e = Gated {
        inner: MockEmbedder::new(DIM).with_chunking(WINDOW),
        entered: entered.clone(),
        release: release.clone(),
    };
    (Arc::new(e), entered, release)
}

/// Fails any batch containing `BAD`; returns `DIM + 1` vectors when `wide`.
struct Picky {
    inner: MockEmbedder,
    wide: bool,
}

fn picky(wide: bool) -> Emb {
    Arc::new(Picky {
        inner: MockEmbedder::new(DIM).with_chunking(WINDOW),
        wide,
    })
}

macro_rules! delegate {
    () => {
        fn dimension(&self) -> usize {
            DIM
        }
        fn input_window(&self) -> Option<usize> {
            self.inner.input_window()
        }
        fn chunking_tokenizer(&self) -> Result<Option<Arc<ChunkTokenizer>>, ChunkError> {
            self.inner.chunking_tokenizer()
        }
    };
}

#[async_trait]
impl Embedder for Gated {
    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        if texts.len() > 1 {
            self.entered.notify_one();
            self.release.notified().await;
        }
        self.inner.embed_batch(texts).await
    }
    delegate!();
}

#[async_trait]
impl Embedder for Picky {
    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        if texts.iter().any(|t| t.contains("BAD")) {
            anyhow::bail!("simulated per-drawer failure");
        }
        if self.wide {
            return Ok(texts.iter().map(|_| vec![1.0; DIM + 1]).collect());
        }
        self.inner.embed_batch(texts).await
    }
    delegate!();
}

fn bounds() -> SliceBounds {
    SliceBounds {
        min_age: Duration::ZERO,
        ..SliceBounds::default()
    }
}

fn ledger_ids(dir: &std::path::Path) -> Vec<Uuid> {
    embed_ledger::load(dir)
        .iter()
        .map(|f| f.drawer_id)
        .collect()
}

#[tokio::test]
async fn progress_counts_done_over_total_for_a_half_backfilled_palace() {
    let dir = tempfile::tempdir().unwrap();
    let h = handle(dir.path());
    for _ in 0..3 {
        legacy(&h, &words(100), 0.5).await;
    }
    let e = chunking();
    let b = SliceBounds {
        max_drawers: 1,
        ..bounds()
    };
    assert_eq!(h.reembed_slice(&e, &b).await.repaired, 1);
    // Derived, not counted: a fresh health recompute agrees.
    let p = h.embed_health_with(Some(e.as_ref())).chunk_progress();
    assert_eq!((p.total, p.done, p.pending, p.failed), (3, 1, 2, 0));
    assert_eq!(p.state, ChunkState::Backfilling);
}

#[tokio::test]
async fn progress_is_unknown_not_complete_when_the_chunk_check_is_unavailable() {
    let dir = tempfile::tempdir().unwrap();
    let h = handle(dir.path());
    legacy(&h, &words(100), 0.5).await;
    let health = h.embed_health_with(None);
    assert!(matches!(health.chunk_check, ChunkCheck::Unavailable(_)));
    assert_eq!(health.chunk_progress().state, ChunkState::Unknown);
}

#[tokio::test]
async fn crash_midway_through_n_drawers_resumes_and_finishes() {
    let dir = tempfile::tempdir().unwrap();
    let mut ids = Vec::new();
    {
        let h = handle(dir.path());
        for _ in 0..4 {
            ids.push(legacy(&h, &words(100), 0.5).await);
        }
        let b = SliceBounds {
            max_drawers: 2,
            ..bounds()
        };
        assert_eq!(h.reembed_slice(&chunking(), &b).await.repaired, 2);
    }
    // "Restart": a fresh handle over the same index; drawers re-added as the
    // drawer store would reload them.
    let h = handle(dir.path());
    let texts = words(100);
    for id in &ids {
        let mut d = Drawer::new(Uuid::new_v4(), &texts);
        d.id = *id;
        d.created_at -= chrono::Duration::hours(1);
        h.add_drawer(d);
    }
    let e = chunking();
    assert_eq!(
        h.embed_health_with(Some(e.as_ref())).chunk_progress().done,
        2
    );
    let mut embedded = 0;
    for _ in 0..4 {
        embedded += h.reembed_slice(&e, &bounds()).await.repaired;
    }
    assert_eq!(embedded, 2, "finished drawers are not re-embedded");
    let health = h.embed_health_with(Some(e.as_ref()));
    assert!(health.short_chunk_ids.is_empty() && health.missing_vector_ids.is_empty());
    assert_eq!(health.chunk_progress().state, ChunkState::Complete);
    assert!(health.alias_audit.is_clean(), "{:?}", health.alias_audit);
}

#[tokio::test]
async fn no_orphan_rows_after_a_forget_races_the_slice() {
    let dir = tempfile::tempdir().unwrap();
    let h = handle(dir.path());
    let id = legacy(&h, &words(100), 0.5).await;
    let (e, entered, release) = gated();
    let b = bounds();
    let (report, ()) = tokio::join!(h.reembed_slice(&e, &b), async {
        entered.notified().await;
        h.forget(id).await.unwrap();
        release.notify_one();
    });
    assert_eq!(report.stale, 1, "{report:?}");
    assert_eq!(shape(&h, id), None, "no row may survive the forget");
}

#[tokio::test]
async fn no_stale_vector_after_a_merge_races_the_slice() {
    let dir = tempfile::tempdir().unwrap();
    let h = handle(dir.path());
    let id = legacy(&h, &words(100), 0.5).await;
    let (e, entered, release) = gated();
    let b = bounds();
    let (report, ()) = tokio::join!(h.reembed_slice(&e, &b), async {
        entered.notified().await;
        let mut drawers = h.drawers.write();
        let d = drawers.iter_mut().find(|d| d.id == id).unwrap();
        d.set_content(words(30));
        drop(drawers);
        release.notify_one();
    });
    assert_eq!(report.stale, 1, "{report:?}");
    assert_eq!(
        shape(&h, id),
        Some((1, 0)),
        "the stale group is not committed"
    );
}

#[tokio::test]
async fn write_mutex_is_free_while_the_slice_embeds() {
    let dir = tempfile::tempdir().unwrap();
    let h = handle(dir.path());
    let id = legacy(&h, &words(100), 0.5).await;
    let (e, entered, release) = gated();
    let m = h.write_mutex_for_test();
    let b = bounds();
    let (report, free) = tokio::join!(h.reembed_slice(&e, &b), async {
        entered.notified().await;
        let free = m.try_lock().is_ok();
        release.notify_one();
        free
    });
    assert!(free, "the slice held write_mutex across the embed");
    assert_eq!(report.repaired, 1);
    assert!(full(&h, id));
}

#[tokio::test]
async fn cancelled_slice_leaves_the_old_group_and_a_free_write_mutex() {
    let dir = tempfile::tempdir().unwrap();
    let id = {
        let h = handle(dir.path());
        let id = legacy(&h, &words(100), 0.5).await;
        let (e, entered, _release) = gated();
        let b = bounds();
        tokio::select! {
            _ = h.reembed_slice(&e, &b) => panic!("the gate never opens"),
            () = entered.notified() => {}
        }
        assert!(h.write_mutex_for_test().try_lock().is_ok());
        assert_eq!(shape(&h, id), Some((1, 0)));
        id
    };
    let h = handle(dir.path());
    assert_eq!(shape(&h, id), Some((1, 0)), "reopened: old group intact");
}

#[tokio::test]
async fn a_busy_palace_makes_the_slice_stop_not_queue() {
    let dir = tempfile::tempdir().unwrap();
    let h = handle(dir.path());
    let id = legacy(&h, &words(100), 0.5).await;
    let m = h.write_mutex_for_test();
    let _held = m.lock().await;
    let b = SliceBounds {
        lock_wait: Duration::from_millis(100),
        ..bounds()
    };
    let report = h.reembed_slice(&chunking(), &b).await;
    let SliceStop::LockTimeout(msg) = &report.stop else {
        panic!("{report:?}");
    };
    assert!(msg.contains("chunk-store"), "names the palace: {msg}");
    assert_eq!(shape(&h, id), Some((1, 0)));
    assert!(ledger_ids(dir.path()).is_empty());
}

#[tokio::test]
async fn a_failed_embed_leaves_the_old_group_and_pending_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let h = handle(dir.path());
    let id = legacy(&h, &words(100), 0.5).await;
    let e = chunking();
    let before = h.embed_health_with(Some(e.as_ref())).chunk_progress();
    let report = h.reembed_slice(&failing(), &bounds()).await;
    assert_eq!((report.failed, report.repaired), (1, 0));
    assert_eq!(shape(&h, id), Some((1, 0)));
    let after = h.embed_health_with(Some(e.as_ref())).chunk_progress();
    assert_eq!(after.done, before.done, "a failure never advances progress");
    assert_eq!(after.pending + after.failed, before.pending);
}

#[tokio::test]
async fn a_failed_upsert_leaves_the_old_group() {
    let dir = tempfile::tempdir().unwrap();
    let h = handle(dir.path());
    let id = legacy(&h, &words(100), 0.5).await;
    let report = h.reembed_slice(&picky(true), &bounds()).await;
    assert!(
        matches!(report.stop, SliceStop::StoreError(_)),
        "{report:?}"
    );
    assert_eq!(shape(&h, id), Some((1, 0)));
    assert_eq!(ledger_ids(dir.path()), vec![id]);
}

#[tokio::test]
async fn tokenizer_failure_is_unknown_not_complete_and_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let h = handle(dir.path());
    let id = legacy(&h, &words(100), 0.5).await;
    let e: Emb = Arc::new(NoTokenizer);
    let report = h.reembed_slice(&e, &bounds()).await;
    assert!(
        matches!(report.stop, SliceStop::ChunkCheckUnavailable(_)),
        "{report:?}"
    );
    assert_eq!(report.attempted, 0);
    assert_eq!(shape(&h, id), Some((1, 0)));
    assert!(ledger_ids(dir.path()).is_empty());
    let p = h.embed_health_with(Some(e.as_ref())).chunk_progress();
    assert_eq!(p.state, ChunkState::Unknown);
}

#[tokio::test]
async fn slice_embed_is_bounded_by_the_slice_timeout() {
    let dir = tempfile::tempdir().unwrap();
    let h = handle(dir.path());
    let id = legacy(&h, &words(100), 0.5).await;
    let (e, _entered, _release) = gated();
    let b = SliceBounds {
        embed_timeout: Duration::from_millis(200),
        ..bounds()
    };
    let started = std::time::Instant::now();
    let report = h.reembed_slice(&e, &b).await;
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(report.failed, 1, "{report:?}");
    assert_eq!(shape(&h, id), Some((1, 0)));
}

#[tokio::test]
async fn slice_stops_starting_drawers_past_the_chunk_cap() {
    let dir = tempfile::tempdir().unwrap();
    let h = handle(dir.path());
    for _ in 0..3 {
        legacy(&h, &words(100), 0.5).await;
    }
    let b = SliceBounds {
        max_chunks: 5,
        ..bounds()
    };
    let report = h.reembed_slice(&chunking(), &b).await;
    assert_eq!(report.stop, SliceStop::ChunkCap, "{report:?}");
    assert_eq!(report.repaired, 1);
}

#[tokio::test]
async fn host_level_slice_failure_writes_no_ledger_rows() {
    let dir = tempfile::tempdir().unwrap();
    let h = handle(dir.path());
    for _ in 0..3 {
        legacy(&h, &words(100), 0.5).await;
    }
    let report = h.reembed_slice(&failing(), &bounds()).await;
    assert_eq!(report.stop, SliceStop::HostFailure);
    assert!(report.host_failure);
    assert_eq!(report.attempted, 2, "stops after two failures");
    assert!(
        ledger_ids(dir.path()).is_empty(),
        "healthy drawers get no backoff"
    );
}

#[tokio::test]
async fn one_bad_drawer_among_good_ones_is_ledgered_and_the_rest_repair() {
    let dir = tempfile::tempdir().unwrap();
    let h = handle(dir.path());
    let bad = legacy(&h, &format!("BAD {}", words(100)), 0.9).await;
    let good = [
        legacy(&h, &words(100), 0.5).await,
        legacy(&h, &words(100), 0.4).await,
    ];
    let report = h.reembed_slice(&picky(false), &bounds()).await;
    assert_eq!((report.repaired, report.failed), (2, 1), "{report:?}");
    assert!(!report.host_failure);
    assert!(good.iter().all(|id| full(&h, *id)));
    assert_eq!(ledger_ids(dir.path()), vec![bad]);
}

/// Two failing drawers around an unsplittable one, then a good one.
async fn unsplittable_fixture(h: &PalaceHandle) -> (Uuid, Uuid) {
    // Over the 4096-chunk cap at ten words per chunk.
    let huge = legacy(h, &words(41_000), 0.9).await;
    legacy(h, &format!("BAD {}", words(100)), 0.8).await;
    let good = legacy(h, &words(100), 0.1).await;
    (huge, good)
}

#[tokio::test]
async fn an_unsplittable_drawer_does_not_count_as_a_host_failure() {
    let dir = tempfile::tempdir().unwrap();
    let h = handle(dir.path());
    let (huge, good) = unsplittable_fixture(&h).await;
    let report = h.reembed_slice(&picky(false), &bounds()).await;
    assert_ne!(report.stop, SliceStop::HostFailure, "{report:?}");
    assert!(full(&h, good));
    assert!(ledger_ids(dir.path()).contains(&huge));
}

#[tokio::test]
async fn an_unsplittable_drawer_is_backed_off_and_does_not_stall_the_queue() {
    let dir = tempfile::tempdir().unwrap();
    let h = handle(dir.path());
    let (huge, good) = unsplittable_fixture(&h).await;
    h.reembed_slice(&picky(false), &bounds()).await;
    assert!(full(&h, good));
    let again = h.reembed_slice(&chunking(), &bounds()).await;
    assert_eq!(
        again.attempted, 0,
        "both failed drawers are backed off: {again:?}"
    );
    assert_eq!(shape(&h, huge), Some((1, 0)));
}

#[test]
fn first_sighting_is_true_once_per_drawer_per_process() {
    let id = Uuid::new_v4();
    assert!(first_sighting(id));
    assert!(!first_sighting(id));
    assert!(first_sighting(Uuid::new_v4()));
}

#[test]
fn first_sighting_set_is_bounded() {
    let mut s = Sightings::new(2);
    assert!(s.first(Uuid::new_v4()) && s.first(Uuid::new_v4()));
    assert!(!s.first(Uuid::new_v4()), "past the cap nothing is kept");
}

#[tokio::test]
async fn repeated_health_calls_warn_once_for_an_unsplittable_drawer() {
    let dir = tempfile::tempdir().unwrap();
    let h = handle(dir.path());
    let huge = legacy(&h, &words(41_000), 0.5).await;
    let e = chunking();
    for _ in 0..5 {
        h.embed_health_with(Some(e.as_ref()));
    }
    assert!(
        !first_sighting(huge),
        "the health check recorded the sighting"
    );
}

#[test]
fn unparsable_marker_is_missing_not_complete() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(CHUNK_MARKER_FILE), b"{not json").unwrap();
    let read = ChunkMarker::read(dir.path());
    assert_eq!(read, None);
    assert!(ChunkMarker::needs_scan(
        read.as_ref(),
        Some(WINDOW),
        chrono::Utc::now()
    ));
    let progress = super::chunk_progress::ChunkProgress {
        total: 1,
        done: 1,
        pending: 0,
        failed: 0,
        state: ChunkState::Complete,
    };
    let m = ChunkMarker::from_progress(&progress, Some(WINDOW), None, chrono::Utc::now());
    m.write(dir.path()).unwrap();
    assert_eq!(ChunkMarker::read(dir.path()), Some(m));
}

fn complete_marker(window: Option<usize>) -> ChunkMarker {
    let progress = super::chunk_progress::ChunkProgress {
        total: 4,
        done: 4,
        pending: 0,
        failed: 0,
        state: ChunkState::Complete,
    };
    ChunkMarker::from_progress(&progress, window, None, chrono::Utc::now())
}

#[test]
fn window_change_forces_a_rescan() {
    let m = complete_marker(Some(WINDOW));
    let now = m.updated_at.unwrap();
    assert!(!ChunkMarker::needs_scan(Some(&m), Some(WINDOW), now));
    assert!(ChunkMarker::needs_scan(Some(&m), Some(512), now));
    let day = now + chrono::Duration::hours(25);
    assert!(
        ChunkMarker::needs_scan(Some(&m), Some(WINDOW), day),
        "24 h rescan"
    );
}

#[test]
fn two_writers_do_not_corrupt_the_marker() {
    let dir = tempfile::tempdir().unwrap();
    let writers: Vec<_> = [Some(WINDOW), Some(512)]
        .into_iter()
        .map(|w| {
            let path = dir.path().to_path_buf();
            std::thread::spawn(move || {
                for _ in 0..50 {
                    complete_marker(w).write(&path).unwrap();
                }
            })
        })
        .collect();
    for w in writers {
        w.join().unwrap();
    }
    let m = ChunkMarker::read(dir.path()).expect("a whole marker");
    assert!(m.window == Some(WINDOW) || m.window == Some(512));
}

#[tokio::test]
async fn a_failed_marker_write_does_not_change_progress() {
    let dir = tempfile::tempdir().unwrap();
    let h = handle(dir.path());
    legacy(&h, &words(100), 0.5).await;
    let e = chunking();
    let before = h.embed_health_with(Some(e.as_ref())).chunk_progress();
    std::fs::create_dir(dir.path().join(CHUNK_MARKER_FILE)).unwrap();
    let m = ChunkMarker::from_progress(&before, Some(WINDOW), None, chrono::Utc::now());
    assert!(m.write(dir.path()).is_err());
    assert_eq!(
        h.embed_health_with(Some(e.as_ref())).chunk_progress(),
        before
    );
}

#[tokio::test]
async fn queue_orders_missing_first_then_short_by_importance() {
    let dir = tempfile::tempdir().unwrap();
    let h = handle(dir.path());
    let low = legacy(&h, &words(100), 0.1).await;
    let high = legacy(&h, &words(100), 0.9).await;
    let missing = add_aged(&h, &words(5), Duration::from_secs(3600), 0.0);
    let health = h.embed_health_with(Some(chunking().as_ref()));
    let q = h.backfill_queue(&health, &SliceBounds::default(), chrono::Utc::now());
    assert_eq!(q, vec![missing, high, low]);
}

#[tokio::test]
async fn missing_vector_drawer_younger_than_two_minutes_is_skipped() {
    let dir = tempfile::tempdir().unwrap();
    let h = handle(dir.path());
    let young = add_aged(&h, &words(5), Duration::from_secs(10), 0.5);
    let old = add_aged(&h, &words(5), Duration::from_secs(600), 0.5);
    let health = h.embed_health_with(Some(chunking().as_ref()));
    let q = h.backfill_queue(&health, &SliceBounds::default(), chrono::Utc::now());
    assert_eq!(q, vec![old], "{young} is left to the deferred lane");
}

#[test]
fn slice_bounds_read_their_env_overrides() {
    // SAFETY: only this test sets these names.
    unsafe {
        std::env::set_var("TRUSTY_CHUNK_BACKFILL_MAX_CHUNKS", "7");
        std::env::set_var("TRUSTY_CHUNK_BACKFILL_EMBED_SECS", "not-a-number");
    }
    let b = SliceBounds::from_env();
    unsafe {
        std::env::remove_var("TRUSTY_CHUNK_BACKFILL_MAX_CHUNKS");
        std::env::remove_var("TRUSTY_CHUNK_BACKFILL_EMBED_SECS");
    }
    assert_eq!(b.max_chunks, 7);
    assert_eq!(b.embed_timeout, SliceBounds::default().embed_timeout);
}

// #9735 Q5: the operator path, `backfill_with`, through the recheck helper.

fn backfill_opts(
    attempts: u32,
    base_delay: Duration,
) -> super::embed_repair::VectorBackfillOptions {
    super::embed_repair::VectorBackfillOptions {
        dry_run: false,
        limit: None,
        retry: super::deferred_embed::RetryPolicy {
            attempts,
            base_delay,
        },
    }
}

#[tokio::test]
async fn backfill_forget_between_read_and_upsert_leaves_no_orphan_row() {
    let dir = tempfile::tempdir().unwrap();
    let h = handle(dir.path());
    let id = legacy(&h, &words(100), 0.5).await;
    let (e, entered, release) = gated();
    let (report, ()) = tokio::join!(
        h.backfill_with(backfill_opts(1, Duration::ZERO), &e),
        async {
            entered.notified().await;
            h.forget(id).await.unwrap();
            release.notify_one();
        }
    );
    report.unwrap();
    let orphan = h
        .vector_store
        .chunk_shapes()
        .unwrap()
        .keys()
        .any(|k| *k == id);
    assert!(!orphan, "a vector row survived the forget");
}

#[tokio::test]
async fn backfill_changed_text_is_not_overwritten_with_a_stale_group() {
    let dir = tempfile::tempdir().unwrap();
    let h = handle(dir.path());
    let id = legacy(&h, &words(100), 0.5).await;
    let (e, entered, release) = gated();
    let (report, ()) = tokio::join!(
        h.backfill_with(backfill_opts(1, Duration::ZERO), &e),
        async {
            entered.notified().await;
            let mut drawers = h.drawers.write();
            let d = drawers.iter_mut().find(|d| d.id == id).unwrap();
            d.set_content(format!("{} tail", words(100)));
            drop(drawers);
            release.notify_one();
        }
    );
    report.unwrap();
    assert_eq!(
        shape(&h, id),
        Some((1, 0)),
        "the group for the old text was committed"
    );
}

#[tokio::test]
async fn backfill_stale_drawer_is_not_failing_and_writes_no_ledger_row() {
    let dir = tempfile::tempdir().unwrap();
    let h = handle(dir.path());
    let id = legacy(&h, &words(100), 0.5).await;
    let (e, entered, release) = gated();
    let (report, ()) = tokio::join!(
        h.backfill_with(backfill_opts(1, Duration::ZERO), &e),
        async {
            entered.notified().await;
            h.forget(id).await.unwrap();
            release.notify_one();
        }
    );
    let report = report.unwrap();
    assert_eq!(
        (report.attempted, report.repaired, report.still_failing),
        (0, 0, 0),
        "{report:?}"
    );
    assert!(ledger_ids(dir.path()).is_empty());
}

/// Fails the first multi-chunk batch, then blocks the next on `release`.
struct FlakyGate {
    inner: MockEmbedder,
    calls: std::sync::atomic::AtomicU32,
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

#[async_trait]
impl Embedder for FlakyGate {
    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        if texts.len() > 1 {
            if self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                anyhow::bail!("transient embed failure");
            }
            self.entered.notify_one();
            self.release.notified().await;
        }
        self.inner.embed_batch(texts).await
    }
    delegate!();
}

/// Sample `try_lock` every 10 ms until `stop` fires; `(samples, held)`.
async fn sample_lock(m: Arc<tokio::sync::Mutex<()>>, stop: &Notify) -> (u32, u32) {
    let (mut samples, mut held) = (0, 0);
    loop {
        tokio::select! {
            () = stop.notified() => break,
            () = tokio::time::sleep(Duration::from_millis(10)) => {
                samples += 1;
                held += u32::from(m.try_lock().is_err());
            }
        }
    }
    (samples, held)
}

#[tokio::test]
async fn backfill_embed_and_retry_sleeps_run_outside_write_mutex() {
    let dir = tempfile::tempdir().unwrap();
    let h = handle(dir.path());
    let id = legacy(&h, &words(100), 0.5).await;
    let (entered, release) = (Arc::new(Notify::new()), Arc::new(Notify::new()));
    let e: Emb = Arc::new(FlakyGate {
        inner: MockEmbedder::new(DIM).with_chunking(WINDOW),
        calls: Default::default(),
        entered: entered.clone(),
        release: release.clone(),
    });
    let m = h.write_mutex_for_test();
    let opts = backfill_opts(2, Duration::from_millis(200));
    let (report, (samples, held)) = tokio::join!(h.backfill_with(opts, &e), async {
        let (mut samples, mut held) = sample_lock(m.clone(), &entered).await;
        samples += 1;
        held += u32::from(m.try_lock().is_err());
        release.notify_one();
        (samples, held)
    });
    assert!(samples >= 5, "the retry sleep was sampled: {samples}");
    assert_eq!(held, 0, "write_mutex held during embed or sleep");
    assert_eq!(report.unwrap().repaired, 1);
    assert!(full(&h, id));
}

#[tokio::test]
async fn backfill_lock_timeout_returns_an_error_naming_the_palace() {
    let dir = tempfile::tempdir().unwrap();
    let h = handle(dir.path());
    let id = legacy(&h, &words(100), 0.5).await;
    let m = h.write_mutex_for_test();
    let held = m.lock().await;
    // Precedent: `timeout_tests` sets this name the same way.
    unsafe { std::env::set_var("TRUSTY_WRITE_LOCK_TIMEOUT_SECS", "0") };
    let result = h
        .backfill_with(backfill_opts(1, Duration::ZERO), &chunking())
        .await;
    unsafe { std::env::remove_var("TRUSTY_WRITE_LOCK_TIMEOUT_SECS") };
    drop(held);
    let err = result.expect_err("a lock timeout must not be skipped silently");
    assert!(format!("{err:#}").contains("chunk-store"), "{err:#}");
    assert_eq!(shape(&h, id), Some((1, 0)));
}

#[tokio::test]
async fn backfill_upsert_failure_is_retried_without_holding_the_lock_between_attempts() {
    let dir = tempfile::tempdir().unwrap();
    let h = handle(dir.path());
    let id = legacy(&h, &words(100), 0.5).await;
    let m = h.write_mutex_for_test();
    let done = Notify::new();
    let opts = backfill_opts(3, Duration::from_millis(200));
    let (report, (samples, held)) = tokio::join!(
        async {
            let r = h.backfill_with(opts, &picky(true)).await;
            done.notify_one();
            r
        },
        sample_lock(m, &done)
    );
    let report = report.unwrap();
    assert_eq!(report.still_failing, 1);
    assert!(samples >= 20, "{samples}");
    assert!(
        held * 10 < samples,
        "lock held between attempts: {held}/{samples}"
    );
    let ledger = embed_ledger::load(dir.path());
    assert_eq!((ledger[0].drawer_id, ledger[0].attempts), (id, 3));
}
