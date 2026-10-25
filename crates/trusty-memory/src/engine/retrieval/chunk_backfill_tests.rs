//! #9735 PR 1: the chunk backfill slice, its progress view and its marker.
//!
//! Why: the slice writes vectors while writers, forgets and failures race it;
//! each test pins one rule that a wrong mechanism would break.
//! What: disk palaces with legacy single-row drawers, driven with chunking
//! mocks (window 12: ten words per chunk).
//! Test: this file IS the tests.

use super::chunk_backfill::{Repair, SliceBounds, SliceStop, write_ledger};
use super::chunk_health::{ChunkCheck, Sightings, first_sighting};
use super::chunk_progress::{CHUNK_MARKER_FILE, ChunkMarker, ChunkState};
use super::deferred_embed::RetryPolicy;
use super::embed_store::BACKGROUND_EMBED_CALL_CAP;
use super::embed_store_tests::{Counting, DIM, NoTokenizer, handle, shape, words};
use super::handle::PalaceHandle;
use crate::engine::embed::Embedder;
use crate::engine::palace::{Drawer, PalaceId};
use crate::engine::store::embed_ledger::{self, EmbedFailure};
use crate::engine::store::hnsw_store::HnswStore;
use crate::engine::store::{kg::KnowledgeGraph, vector::UsearchStore};
use anyhow::Result;
use async_trait::async_trait;
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::sync::Notify;
use trusty_common::embedder::{ChunkError, ChunkTokenizer, MockEmbedder};
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

/// Whether `texts` opens a multi-chunk drawer embed.
///
/// #10008: a paced embed makes several `embed_batch` calls per drawer, so a
/// gate counts drawers by their first chunk; every `words` drawer opens `w0 `.
fn opens_a_drawer(texts: &[String]) -> bool {
    texts.len() > 1 && texts[0].starts_with("w0 ")
}

/// Blocks every multi-chunk drawer embed until `release` is notified.
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
        if opens_a_drawer(texts) {
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

/// `bounds()` with a slice budget that cannot expire. A test that asserts a
/// whole queue was visited in one slice must not depend on how fast the runner
/// is: under coverage instrumentation the 20 s default ran out (#9735 PR 1b).
fn unexpiring() -> SliceBounds {
    SliceBounds {
        budget: Duration::MAX,
        ..bounds()
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

/// Embeds in `spawn_blocking` under one model lock, as `FastEmbedder` does
/// (`embedder/fast_embedder.rs`); a batch naming `SLOW` holds it for `slow`.
/// A timeout drops the future, not the blocking job, so the lock stays held.
struct Blocking {
    inner: MockEmbedder,
    model: Arc<std::sync::Mutex<()>>,
    slow: Duration,
}

#[async_trait]
impl Embedder for Blocking {
    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let model = self.model.clone();
        let hold = if texts.iter().any(|t| t.contains("SLOW")) {
            self.slow
        } else {
            Duration::ZERO
        };
        tokio::task::spawn_blocking(move || {
            let _model = model
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            std::thread::sleep(hold);
        })
        .await?;
        self.inner.embed_batch(texts).await
    }
    delegate!();
}

/// #9735 (critic HIGH): a timed-out embed keeps the model, so the next
/// drawer's embed times out too; that pair must not read as a host failure.
#[tokio::test]
async fn a_timed_out_drawer_is_ledgered_and_the_next_slice_moves_past_it() {
    let dir = tempfile::tempdir().unwrap();
    let h = handle(dir.path());
    let slow = legacy(&h, &format!("SLOW {}", words(100)), 0.9).await;
    let next = legacy(&h, &words(100), 0.5).await;
    let e: Emb = Arc::new(Blocking {
        inner: MockEmbedder::new(DIM).with_chunking(WINDOW),
        model: Arc::default(),
        slow: Duration::from_millis(1500),
    });
    let tight = SliceBounds {
        embed_timeout: Duration::from_millis(200),
        ..bounds()
    };
    let first = h.reembed_slice(&e, &tight).await;
    assert_eq!(first.stop, SliceStop::EmbedTimeout, "{first:?}");
    assert!(!first.host_failure, "{first:?}");
    assert_eq!(ledger_ids(dir.path()), vec![slow], "{first:?}");
    // The orphaned embed still holds the model; this bound waits it out.
    let second = h.reembed_slice(&e, &bounds()).await;
    assert_eq!(second.repaired, 1, "{second:?}");
    assert!(full(&h, next));
    assert_eq!(
        shape(&h, slow),
        Some((1, 0)),
        "the timed-out drawer is backed off"
    );
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

/// A failing drawer after an oversized one, then a good one.
async fn oversized_fixture(h: &PalaceHandle) -> (Uuid, Uuid) {
    // Over the 4096-chunk cap at ten words per chunk.
    let huge = legacy(h, &words(41_000), 0.9).await;
    legacy(h, &format!("BAD {}", words(100)), 0.8).await;
    let good = legacy(h, &words(100), 0.1).await;
    (huge, good)
}

/// #9735 PR 1b: an oversized drawer is split, not ledgered as a failure.
#[tokio::test]
async fn an_oversized_drawer_does_not_count_as_a_host_failure() {
    let dir = tempfile::tempdir().unwrap();
    let h = handle(dir.path());
    let (huge, good) = oversized_fixture(&h).await;
    let report = h.reembed_slice(&picky(false), &unexpiring()).await;
    assert_ne!(report.stop, SliceStop::HostFailure, "{report:?}");
    assert!(full(&h, good));
    assert!(
        !ledger_ids(dir.path()).contains(&huge),
        "an oversized drawer is split, not ledgered"
    );
}

/// #9735 PR 1b: the split takes one slice; the next embeds every part.
#[tokio::test]
async fn an_oversized_drawer_is_split_and_does_not_stall_the_queue() {
    let dir = tempfile::tempdir().unwrap();
    let h = handle(dir.path());
    let (huge, good) = oversized_fixture(&h).await;
    h.reembed_slice(&picky(false), &unexpiring()).await;
    assert!(full(&h, good));
    let tag = format!("split-of:{huge}");
    let parts: Vec<Uuid> = h
        .drawers
        .read()
        .iter()
        .filter(|d| d.id == huge || d.tags.contains(&tag))
        .map(|d| d.id)
        .collect();
    assert_eq!(parts.len(), 5, "41 000 words split into five parts");
    let uncapped = SliceBounds {
        max_chunks: usize::MAX,
        ..unexpiring()
    };
    let again = h.reembed_slice(&chunking(), &uncapped).await;
    assert_eq!(again.repaired, 5, "every part embeds: {again:?}");
    assert!(parts.iter().all(|id| full(&h, *id)));
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
    // #9271: the lib target's one env lock (#9735), now trusty-memory's.
    let _env = crate::env_test_lock::env_test_lock().blocking_lock();
    // SAFETY: serialised by `env_test_lock`, held above for the whole test.
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

/// Fails the first multi-chunk drawer embed, then blocks the next on `release`.
struct FlakyGate {
    inner: MockEmbedder,
    calls: std::sync::atomic::AtomicU32,
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

#[async_trait]
impl Embedder for FlakyGate {
    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        if opens_a_drawer(texts) {
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
    // #9271: the lib target's one env lock (#9735), now trusty-memory's.
    let _env = crate::env_test_lock::env_test_lock().lock().await;
    let dir = tempfile::tempdir().unwrap();
    let h = handle(dir.path());
    let id = legacy(&h, &words(100), 0.5).await;
    let m = h.write_mutex_for_test();
    let held = m.lock().await;
    // SAFETY: serialised by `env_test_lock`, held above for the whole test.
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

// #9735 fix round: one error-arm test per failure branch of the slice.

fn failure(drawer_id: Uuid) -> EmbedFailure {
    EmbedFailure {
        drawer_id,
        failed_at: chrono::Utc::now(),
        attempts: 1,
        reason: "earlier failure".into(),
    }
}

#[test]
fn a_ledger_fault_on_one_row_still_records_the_rest_and_clears() {
    let rows: Vec<EmbedFailure> = (0..3).map(|_| failure(Uuid::new_v4())).collect();
    let bad = rows[0].drawer_id;
    let (mut recorded, mut cleared) = (Vec::new(), false);
    let err = write_ledger(
        rows.clone(),
        &HashSet::from([Uuid::new_v4()]),
        |row| {
            anyhow::ensure!(row.drawer_id != bad, "injected record fault");
            recorded.push(row.drawer_id);
            Ok(())
        },
        |_| {
            cleared = true;
            Ok(())
        },
    );
    let rest: Vec<Uuid> = rows[1..].iter().map(|r| r.drawer_id).collect();
    assert_eq!(recorded, rest, "the other rows are still recorded");
    assert!(cleared, "the clear still runs after a record fault");
    assert!(err.is_some_and(|e| e.contains("injected record fault")));
}

#[tokio::test]
async fn a_malformed_ledger_is_reported_on_the_slice() {
    let dir = tempfile::tempdir().unwrap();
    let h = handle(dir.path());
    legacy(&h, &format!("BAD {}", words(100)), 0.9).await;
    let good = legacy(&h, &words(100), 0.5).await;
    std::fs::write(dir.path().join("embed_failures.json"), b"{not json").unwrap();
    let report = h.reembed_slice(&picky(false), &bounds()).await;
    assert_eq!((report.repaired, report.failed), (1, 1), "{report:?}");
    assert!(full(&h, good));
    assert!(report.ledger_error.is_some(), "{report:?}");
}

/// Why: the clear-error arm of `write_ledger` was unreached; `clear` returns
/// `Ok` early on a malformed file, so only a fault inside the locked update
/// gets there.
/// What: ledgers the drawer with an old row, then makes the ledger's lock
/// sidecar a directory so `clear` cannot open it; the repair succeeds and the
/// clear failure must surface on `SliceReport.ledger_error`.
/// Test: this test.
#[tokio::test]
async fn a_failed_ledger_clear_is_reported_on_the_slice() {
    let dir = tempfile::tempdir().unwrap();
    let h = handle(dir.path());
    let id = legacy(&h, &words(100), 0.5).await;
    let mut row = failure(id);
    row.failed_at -= chrono::Duration::days(2);
    std::fs::write(
        dir.path().join("embed_failures.json"),
        serde_json::to_vec(&[row]).unwrap(),
    )
    .unwrap();
    std::fs::create_dir(dir.path().join("embed_failures.json.lock")).unwrap();
    let report = h.reembed_slice(&chunking(), &bounds()).await;
    assert_eq!((report.repaired, report.failed), (1, 0), "{report:?}");
    assert!(full(&h, id));
    let err = report.ledger_error.as_deref().unwrap_or_default();
    assert!(err.contains("clear embed failures"), "{report:?}");
}

#[tokio::test]
async fn slice_on_a_read_only_handle_is_a_noop() {
    let dir = tempfile::tempdir().unwrap();
    let id = legacy(&handle(dir.path()), &words(100), 0.5).await;
    // Hold the vector redb's lock so the open falls back to a snapshot.
    let _live = redb::Database::create(dir.path().join("idx.usearch.redb")).unwrap();
    let vs = UsearchStore::new(dir.path().join("idx.usearch"), DIM).unwrap();
    let kg = KnowledgeGraph::open(&dir.path().join("kg.db")).unwrap();
    let mut h = PalaceHandle::new(PalaceId::new("chunk-store"), String::new(), vs, kg);
    h.data_dir = Some(dir.path().to_path_buf());
    assert!(h.is_read_only(), "precondition: a snapshot handle");
    let mut d = Drawer::new(Uuid::new_v4(), words(100));
    d.id = id;
    d.created_at -= chrono::Duration::hours(1);
    h.add_drawer(d);
    let report = h.reembed_slice(&chunking(), &bounds()).await;
    assert_eq!(report.stop, SliceStop::ReadOnly, "{report:?}");
    assert_eq!(report.attempted, 0);
    assert_eq!(shape(&h, id), Some((1, 0)));
    assert!(ledger_ids(dir.path()).is_empty());
}

/// Blocks the `nth` multi-chunk drawer embed (from 1) until `release`.
struct NthGate {
    inner: MockEmbedder,
    nth: u32,
    calls: std::sync::atomic::AtomicU32,
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

#[async_trait]
impl Embedder for NthGate {
    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let seq = std::sync::atomic::Ordering::SeqCst;
        if opens_a_drawer(texts) && self.calls.fetch_add(1, seq) + 1 == self.nth {
            self.entered.notify_one();
            self.release.notified().await;
        }
        self.inner.embed_batch(texts).await
    }
    delegate!();
}

#[tokio::test]
async fn backfill_lock_timeout_clears_the_repaired_rows_first() {
    // #9271: the lib target's one env lock (#9735), now trusty-memory's.
    let _env = crate::env_test_lock::env_test_lock().lock().await;
    let dir = tempfile::tempdir().unwrap();
    let h = handle(dir.path());
    let ids = [
        legacy(&h, &words(100), 0.5).await,
        legacy(&h, &words(100), 0.5).await,
    ];
    for id in ids {
        embed_ledger::record(dir.path(), failure(id)).unwrap();
    }
    let (entered, release) = (Arc::new(Notify::new()), Arc::new(Notify::new()));
    let e: Emb = Arc::new(NthGate {
        inner: MockEmbedder::new(DIM).with_chunking(WINDOW),
        nth: 2,
        calls: Default::default(),
        entered: entered.clone(),
        release: release.clone(),
    });
    let m = h.write_mutex_for_test();
    // SAFETY: serialised by `env_test_lock`, held above for the whole test.
    unsafe { std::env::set_var("TRUSTY_WRITE_LOCK_TIMEOUT_SECS", "1") };
    let (result, _held) = tokio::join!(
        h.backfill_with(backfill_opts(1, Duration::ZERO), &e),
        async {
            entered.notified().await;
            let held = m.clone().lock_owned().await;
            release.notify_one();
            held
        }
    );
    unsafe { std::env::remove_var("TRUSTY_WRITE_LOCK_TIMEOUT_SECS") };
    result.expect_err("the second commit's lock wait ends the run");
    let (done, busy): (Vec<Uuid>, Vec<Uuid>) = ids.iter().copied().partition(|id| full(&h, *id));
    assert_eq!(
        (done.len(), busy.len()),
        (1, 1),
        "one drawer repaired first"
    );
    assert_eq!(
        ledger_ids(dir.path()),
        busy,
        "the repaired drawer's row is cleared before the error"
    );
}

#[tokio::test]
async fn a_redb_fault_in_the_upsert_loses_no_update_and_leaves_no_orphan_rows() {
    let dir = tempfile::tempdir().unwrap();
    let h = handle(dir.path());
    let done = legacy(&h, &words(100), 0.9).await;
    let faulted = legacy(&h, &words(100), 0.5).await;
    let untouched = legacy(&h, &words(100), 0.1).await;
    HnswStore::fail_next_group_commit_for_test(&faulted.to_string());
    let report = h.reembed_slice(&chunking(), &bounds()).await;
    assert!(
        matches!(report.stop, SliceStop::StoreError(_)),
        "{report:?}"
    );
    assert_eq!((report.repaired, report.failed), (1, 1), "{report:?}");
    assert!(full(&h, done), "the earlier commit is kept");
    assert_eq!(
        shape(&h, faulted),
        Some((1, 0)),
        "rolled back: no chunk row"
    );
    assert_eq!(shape(&h, untouched), Some((1, 0)), "the slice stopped");
    let groups = h.vector_store.chunk_shapes().unwrap();
    assert_eq!(groups.len(), 3, "no orphan group: {groups:?}");
    assert_eq!(ledger_ids(dir.path()), vec![faulted]);
    let retry = SliceBounds {
        ledger_backoff: Duration::ZERO,
        ..bounds()
    };
    h.reembed_slice(&chunking(), &retry).await;
    assert!(
        full(&h, faulted),
        "the store is writable after the rollback"
    );
}

// #10008: a background embed sends at most `BACKGROUND_EMBED_CALL_CAP`
// chunks per `embed_batch` call, so the shared model lock is released between
// calls and a recall query embed does not wait behind a whole drawer.

/// Words in a drawer that splits into about 30 chunks at `WINDOW`.
const PACED_WORDS: usize = 300;

/// Every call carried at most the cap, and there was more than one.
fn assert_paced(calls: &[usize], chunks: usize) {
    assert!(chunks > 2 * BACKGROUND_EMBED_CALL_CAP, "{chunks} chunks");
    assert!(
        calls.iter().all(|&n| n <= BACKGROUND_EMBED_CALL_CAP),
        "every background embed_batch call carries at most {BACKGROUND_EMBED_CALL_CAP} \
         inputs: {calls:?}"
    );
    assert_eq!(calls.iter().sum::<usize>(), chunks, "{calls:?}");
}

#[tokio::test]
async fn slice_embed_calls_carry_at_most_the_background_cap() {
    let dir = tempfile::tempdir().unwrap();
    let h = handle(dir.path());
    let id = legacy(&h, &words(PACED_WORDS), 0.5).await;
    let c = Arc::new(Counting::chunking(WINDOW));
    let e: Emb = c.clone();
    let report = h.reembed_slice(&e, &unexpiring()).await;
    assert_eq!((report.repaired, report.failed), (1, 0), "{report:?}");
    assert!(full(&h, id), "the drawer has its whole group");
    assert_paced(&c.calls.lock(), report.chunks_embedded);
}

#[tokio::test]
async fn repair_embed_calls_carry_at_most_the_background_cap() {
    let dir = tempfile::tempdir().unwrap();
    let h = handle(dir.path());
    let text = words(PACED_WORDS);
    let id = legacy(&h, &text, 0.5).await;
    let c = Arc::new(Counting::chunking(WINDOW));
    let e: Emb = c.clone();
    let out = h
        .repair_one(
            &e,
            id,
            &text,
            &RetryPolicy::instant(1),
            Duration::from_secs(5),
        )
        .await;
    assert!(matches!(out, Repair::Repaired(1)), "{out:?}");
    let rows = shape(&h, id).expect("group").0;
    assert_paced(&c.calls.lock(), rows);
}

/// Fails the `fail_on`-th `embed_batch` call (1-based); counts every call.
struct FailsCall {
    inner: MockEmbedder,
    fail_on: usize,
    calls: AtomicUsize,
}

#[async_trait]
impl Embedder for FailsCall {
    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        if self.calls.fetch_add(1, Ordering::SeqCst) + 1 == self.fail_on {
            anyhow::bail!("simulated failure on a later paced call");
        }
        self.inner.embed_batch(texts).await
    }
    delegate!();
}

#[tokio::test]
async fn a_failed_later_embed_call_commits_no_partial_group() {
    let dir = tempfile::tempdir().unwrap();
    let h = handle(dir.path());
    let id = legacy(&h, &words(PACED_WORDS), 0.5).await;
    let e = Arc::new(FailsCall {
        inner: MockEmbedder::new(DIM).with_chunking(WINDOW),
        fail_on: 2,
        calls: AtomicUsize::new(0),
    });
    let emb: Emb = e.clone();
    let report = h.reembed_slice(&emb, &unexpiring()).await;
    assert_eq!(
        (report.repaired, report.failed, report.chunks_embedded),
        (0, 1, 0),
        "the drawer failed and no chunk counts as embedded: {report:?}"
    );
    assert_eq!(
        e.calls.load(Ordering::SeqCst),
        2,
        "no call after the failure"
    );
    assert_eq!(shape(&h, id), Some((1, 0)), "the legacy row is untouched");
    assert_eq!(ledger_ids(dir.path()), vec![id], "the drawer is ledgered");
}

/// Lets each drawer's first group through, then fails, or wedges when
/// `wedge`, every later group; counts the groups that returned vectors.
struct LaterGroup {
    inner: MockEmbedder,
    wedge: bool,
    done: AtomicUsize,
}

fn later_group(wedge: bool) -> Arc<LaterGroup> {
    Arc::new(LaterGroup {
        inner: MockEmbedder::new(DIM).with_chunking(WINDOW),
        wedge,
        done: AtomicUsize::new(0),
    })
}

#[async_trait]
impl Embedder for LaterGroup {
    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        if !texts[0].starts_with("w0 ") {
            if self.wedge {
                std::future::pending::<()>().await;
            }
            anyhow::bail!("simulated failure on a later paced group");
        }
        let v = self.inner.embed_batch(texts).await?;
        self.done.fetch_add(1, Ordering::SeqCst);
        Ok(v)
    }
    delegate!();
}

/// `repair_one`, then `backfill_with`, against `e`: both fail closed.
async fn assert_repair_fails_closed(e: Arc<LaterGroup>) {
    let dir = tempfile::tempdir().unwrap();
    let h = handle(dir.path());
    let text = words(PACED_WORDS);
    let id = legacy(&h, &text, 0.5).await;
    let emb: Emb = e.clone();
    let out = h
        .repair_one(
            &emb,
            id,
            &text,
            &RetryPolicy::instant(1),
            Duration::from_secs(5),
        )
        .await;
    assert!(
        matches!(
            out,
            Repair::Failed(super::deferred_embed::EmbedLoss::Embed { .. })
        ),
        "{out:?}"
    );
    assert_eq!(e.done.load(Ordering::SeqCst), 1, "the first group finished");
    assert_eq!(shape(&h, id), Some((1, 0)), "the legacy row is untouched");
    let report = h
        .backfill_with(backfill_opts(1, Duration::ZERO), &emb)
        .await
        .unwrap();
    assert_eq!(
        (report.repaired, report.still_failing),
        (0, 1),
        "{report:?}"
    );
    assert_eq!(shape(&h, id), Some((1, 0)), "no chunk row is stored");
    assert_eq!(ledger_ids(dir.path()), vec![id], "the drawer is ledgered");
}

#[tokio::test]
async fn a_failed_later_repair_embed_call_commits_no_partial_group() {
    assert_repair_fails_closed(later_group(false)).await;
}

#[tokio::test]
async fn a_repair_embed_timing_out_on_a_later_group_commits_no_partial_group() {
    let _env = crate::env_test_lock::env_test_lock().lock().await;
    // SAFETY: serialised by `env_test_lock`, held for the whole test.
    // A 1 s bound over the whole paced embed: the first group finishes, the
    // second wedges, and the timeout fires.
    unsafe {
        std::env::set_var("TRUSTY_EMBED_BATCH_TIMEOUT_SECS", "1");
        std::env::set_var("TRUSTY_EMBED_CHUNKED_CAP_SECS", "1");
    }
    assert_repair_fails_closed(later_group(true)).await;
    unsafe {
        std::env::remove_var("TRUSTY_EMBED_BATCH_TIMEOUT_SECS");
        std::env::remove_var("TRUSTY_EMBED_CHUNKED_CAP_SECS");
    }
}
