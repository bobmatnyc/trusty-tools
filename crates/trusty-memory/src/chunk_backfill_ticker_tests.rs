//! #9735 PR 2: the chunk backfill ticker over real on-disk palaces.
//!
//! Why: the ticker is the only production caller of `reembed_slice`; each
//! test pins one rule a wrong loop would break — finish and stop, resume
//! after a restart, back off on every error arm, probe after a timeout, and
//! stop on shutdown.
//! What: a writer registry over a temp root, palaces holding legacy
//! single-row drawers, and a chunking mock (window 12: ten words per chunk)
//! whose mode a test flips between ok, fail, fail-on-`BAD` and hang.
//! Test: this file IS the tests.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use serial_test::serial;
use tokio::sync::watch;
use trusty_common::embedder::{ChunkError, ChunkTokenizer, MockEmbedder};
use uuid::Uuid;

use crate::chunk_backfill_ticker::{
    DISABLED_ENV, Source, Tick, Ticker, TickerConfig, Verdict, judge, run,
    spawn_chunk_backfill_ticker,
};
use crate::engine::embed::Embedder;
use crate::engine::palace::{Drawer, Palace, PalaceId};
use crate::engine::registry::PalaceRegistry;
use crate::engine::retrieval::{ChunkMarker, PalaceHandle, SliceBounds, SliceReport, SliceStop};
use crate::engine::store::embed_ledger;

const DIM: usize = 384;
const WINDOW: usize = 12;
const OK: u8 = 0;
const FAIL: u8 = 1;
const FAIL_BAD: u8 = 2;
const HANG: u8 = 3;

/// A chunking mock whose behaviour a test switches between calls.
struct Switch {
    inner: MockEmbedder,
    mode: AtomicU8,
    calls: AtomicUsize,
}

impl Switch {
    fn new(mode: u8) -> Arc<Self> {
        Arc::new(Self {
            inner: MockEmbedder::new(DIM).with_chunking(WINDOW),
            mode: AtomicU8::new(mode),
            calls: AtomicUsize::new(0),
        })
    }

    fn set(&self, mode: u8) {
        self.mode.store(mode, Ordering::SeqCst);
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl Embedder for Switch {
    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        match self.mode.load(Ordering::SeqCst) {
            FAIL => anyhow::bail!("simulated embedder failure"),
            FAIL_BAD if texts.iter().any(|t| t.contains("BAD")) => {
                anyhow::bail!("simulated per-drawer failure")
            }
            HANG => std::future::pending().await,
            _ => self.inner.embed_batch(texts).await,
        }
    }
    fn dimension(&self) -> usize {
        DIM
    }
    fn input_window(&self) -> Option<usize> {
        self.inner.input_window()
    }
    fn chunking_tokenizer(&self) -> Result<Option<Arc<ChunkTokenizer>>, ChunkError> {
        self.inner.chunking_tokenizer()
    }
}

fn words(n: usize) -> String {
    (0..n)
        .map(|i| format!("w{i}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn registry(root: &Path) -> PalaceRegistry {
    PalaceRegistry::new()
        .with_writer_intent()
        .with_data_root(root)
}

/// One slice re-embeds one drawer, so a test can stop between drawers.
fn one_drawer() -> SliceBounds {
    SliceBounds {
        max_drawers: 1,
        budget: Duration::MAX,
        ..SliceBounds::default()
    }
}

fn ticker(reg: &PalaceRegistry, root: &Path, e: &Arc<Switch>, bounds: SliceBounds) -> Ticker {
    let source = Source::Fixed(Arc::clone(e) as Arc<dyn Embedder + Send + Sync>);
    Ticker::new(
        reg.clone(),
        root.to_path_buf(),
        source,
        bounds,
        Duration::from_millis(200),
    )
}

/// A persisted palace holding one legacy (single-row) drawer per text.
async fn palace(
    reg: &PalaceRegistry,
    root: &Path,
    texts: &[String],
) -> (Arc<PalaceHandle>, Vec<Uuid>) {
    let id = PalaceId::new("ticker");
    let h = reg
        .create_palace(root, Palace::new(id, "ticker", root.join("ticker")))
        .expect("create palace");
    let mut ids = Vec::new();
    for text in texts {
        let mut d = Drawer::new(Uuid::new_v4(), text.as_str());
        d.created_at -= chrono::Duration::hours(1);
        h.kg.upsert_drawer(&d).await.expect("persist drawer");
        h.vector_store
            .upsert_chunks(d.id, vec![vec![1.0; DIM]])
            .await
            .expect("legacy row");
        ids.push(d.id);
        h.add_drawer(d);
    }
    (h, ids)
}

fn legacy_texts(n: usize) -> Vec<String> {
    (0..n).map(|_| words(100)).collect()
}

fn rows(h: &PalaceHandle, id: Uuid) -> usize {
    let shapes = h.vector_store.chunk_shapes().expect("chunk shapes");
    shapes.get(&id).map_or(0, |s| s.rows)
}

fn marker(h: &PalaceHandle) -> ChunkMarker {
    let dir = h.data_dir.clone().expect("data dir");
    ChunkMarker::read(&dir).expect("marker written")
}

/// Tick until `Idle`, at most `limit` times; the ticks taken.
async fn until_idle(t: &mut Ticker, limit: usize) -> Vec<Tick> {
    let mut seen = Vec::new();
    for _ in 0..limit {
        let tick = t.tick().await;
        let idle = tick == Tick::Idle;
        seen.push(tick);
        if idle {
            break;
        }
    }
    seen
}

#[tokio::test]
async fn ticker_reembeds_every_legacy_drawer_then_goes_idle() {
    let root = tempfile::tempdir().unwrap();
    let reg = registry(root.path());
    let (h, ids) = palace(&reg, root.path(), &legacy_texts(3)).await;
    let e = Switch::new(OK);
    let mut t = ticker(&reg, root.path(), &e, one_drawer());

    let seen = until_idle(&mut t, 12).await;
    assert_eq!(seen.last(), Some(&Tick::Idle), "{seen:?}");
    assert!(
        ids.iter().all(|id| rows(&h, *id) > 1),
        "every drawer is chunked"
    );
    let m = marker(&h);
    assert_eq!((m.total, m.pending, m.failed), (3, 0, 0));
    assert_eq!(
        m.state,
        Some(crate::engine::retrieval::ChunkState::Complete)
    );
    // #9881: re-embedding is not a recall; no access is recorded.
    assert!(
        h.drawers
            .read()
            .iter()
            .all(|d| d.access_count == 0 && d.last_accessed_at.is_none())
    );
}

#[tokio::test]
async fn ticker_resumes_from_the_marker_after_a_restart() {
    let root = tempfile::tempdir().unwrap();
    let ids = {
        let reg = registry(root.path());
        let (h, ids) = palace(&reg, root.path(), &legacy_texts(3)).await;
        let e = Switch::new(OK);
        let mut t = ticker(&reg, root.path(), &e, one_drawer());
        assert_eq!(t.tick().await, Tick::Worked);
        let m = marker(&h);
        assert_eq!(
            (m.total, m.pending),
            (3, 2),
            "one drawer done before the crash"
        );
        ids
    };
    // "Restart": every handle is gone; a new registry reopens from disk.
    let reg = registry(root.path());
    let e = Switch::new(OK);
    let mut t = ticker(&reg, root.path(), &e, one_drawer());
    let seen = until_idle(&mut t, 12).await;
    assert_eq!(seen.last(), Some(&Tick::Idle), "{seen:?}");
    let worked = seen.iter().filter(|s| **s == Tick::Worked).count();
    assert!(
        worked >= 2,
        "the two remaining drawers took a slice each: {seen:?}"
    );
    let h = reg
        .open_palace(root.path(), &PalaceId::new("ticker"))
        .expect("reopen");
    assert!(ids.iter().all(|id| rows(&h, *id) > 1));
    assert_eq!(marker(&h).pending, 0);
    drop(h);
    assert!(reg.release_if_unreferenced(&PalaceId::new("ticker")));

    // A complete marker is trusted: the next pass opens nothing.
    assert_eq!(t.tick().await, Tick::Idle);
    assert!(
        reg.peek(&PalaceId::new("ticker")).is_none(),
        "a finished palace stays closed"
    );
}

/// Fail-Open Check: a host-level embed failure must back off and leave the
/// marker at the index's count, never ahead of it.
#[tokio::test]
async fn a_failed_slice_backs_off_and_leaves_the_marker_backfilling() {
    let root = tempfile::tempdir().unwrap();
    let reg = registry(root.path());
    let (h, ids) = palace(&reg, root.path(), &legacy_texts(3)).await;
    let e = Switch::new(FAIL);
    let mut t = ticker(&reg, root.path(), &e, SliceBounds::default());

    let tick = t.tick().await;
    assert!(matches!(tick, Tick::Backoff(_)), "{tick:?}");
    let m = marker(&h);
    assert_eq!((m.total, m.pending), (3, 3), "a failure advances nothing");
    assert!(m.last_error.is_some());
    assert!(ids.iter().all(|id| rows(&h, *id) == 1));
    let dir = h.data_dir.clone().unwrap();
    assert!(
        embed_ledger::load(&dir).is_empty(),
        "a host fault backs no drawer off"
    );

    // Retried later: the embedder recovers and the same drawers finish.
    e.set(OK);
    let seen = until_idle(&mut t, 4).await;
    assert_eq!(seen.last(), Some(&Tick::Idle), "{seen:?}");
    assert!(ids.iter().all(|id| rows(&h, *id) > 1));
}

/// PR 1 review, binding item 2: a ledger error backs off even when the slice
/// repaired drawers, so a timed-out drawer is never retried in a hot loop.
#[tokio::test]
async fn a_ledger_error_backs_off_even_after_a_repair() {
    let root = tempfile::tempdir().unwrap();
    let reg = registry(root.path());
    let texts = vec![format!("BAD {}", words(99)), words(100)];
    let (h, ids) = palace(&reg, root.path(), &texts).await;
    let dir = h.data_dir.clone().unwrap();
    std::fs::write(dir.join("embed_failures.json"), b"{not json").unwrap();
    let e = Switch::new(FAIL_BAD);
    let mut t = ticker(&reg, root.path(), &e, SliceBounds::default());

    let tick = t.tick().await;
    let Tick::Backoff(reason) = &tick else {
        panic!("a ledger error must back off: {tick:?}");
    };
    assert!(reason.contains("ledger"), "{reason}");
    assert!(rows(&h, ids[1]) > 1, "the good drawer was still repaired");
    assert!(marker(&h).last_error.is_some());
}

/// PR 1 review, binding item 1: after an embed timeout no slice starts until
/// a probe embed succeeds.
#[tokio::test]
async fn a_failed_probe_starts_no_slice_until_it_succeeds() {
    let root = tempfile::tempdir().unwrap();
    let reg = registry(root.path());
    let (h, ids) = palace(&reg, root.path(), &legacy_texts(2)).await;
    let e = Switch::new(HANG);
    let bounds = SliceBounds {
        embed_timeout: Duration::from_millis(100),
        ..SliceBounds::default()
    };
    let mut t = ticker(&reg, root.path(), &e, bounds);

    assert_eq!(t.tick().await, Tick::Worked, "the timed-out slice");
    assert!(t.probe_pending);
    let before = e.calls();
    let tick = t.tick().await;
    assert!(matches!(tick, Tick::Backoff(_)), "{tick:?}");
    assert_eq!(
        e.calls(),
        before + 1,
        "only the probe ran; no slice started"
    );
    assert!(t.probe_pending, "still waiting on a good probe");

    e.set(OK);
    assert_eq!(t.tick().await, Tick::Worked);
    assert!(!t.probe_pending);
    // The timed-out drawer is ledgered; the other one is repaired.
    assert_eq!(ids.iter().filter(|id| rows(&h, **id) > 1).count(), 1);
}

fn report(stop: SliceStop) -> SliceReport {
    SliceReport {
        stop,
        ..SliceReport::default()
    }
}

#[test]
fn judge_backs_off_on_every_error_arm_and_stays() {
    let errors = [
        report(SliceStop::HostFailure),
        report(SliceStop::StoreError("refused".into())),
        report(SliceStop::ChunkCheckUnavailable("no tokenizer".into())),
        SliceReport {
            repaired: 1,
            attempted: 1,
            ledger_error: Some("malformed".into()),
            ..SliceReport::default()
        },
    ];
    for r in errors {
        let v = judge(&r, true);
        assert!(matches!(v.tick, Tick::Backoff(_)), "{r:?} -> {v:?}");
        assert!(v.stay && !v.fruitless, "{r:?} -> {v:?}");
    }
    let lock = judge(&report(SliceStop::LockTimeout("busy".into())), true);
    assert_eq!((lock.tick, lock.stay), (Tick::Busy, true));
    let timeout = judge(&report(SliceStop::EmbedTimeout), true);
    assert!(timeout.probe && timeout.stay);
}

#[test]
fn judge_moves_on_from_a_finished_or_fruitless_palace() {
    let acted = SliceReport {
        attempted: 1,
        repaired: 1,
        stop: SliceStop::DrawerCap,
        ..SliceReport::default()
    };
    let stay = |v: Verdict| (v.tick, v.stay, v.fruitless);
    assert_eq!(stay(judge(&acted, true)), (Tick::Worked, true, false));
    assert_eq!(stay(judge(&acted, false)), (Tick::Worked, false, false));
    let idle = report(SliceStop::QueueEmpty);
    assert_eq!(stay(judge(&idle, true)), (Tick::Worked, false, true));
    assert_eq!(
        stay(judge(&report(SliceStop::ReadOnly), true)),
        (Tick::Worked, false, true)
    );
}

fn fast() -> TickerConfig {
    TickerConfig {
        tick: Duration::from_millis(10),
        busy: Duration::from_secs(3600),
        backoff: Duration::from_secs(3600),
        idle: Duration::from_secs(3600),
        probe_timeout: Duration::from_secs(3600),
    }
}

#[tokio::test]
async fn ticker_stops_on_shutdown_mid_slice() {
    let root = tempfile::tempdir().unwrap();
    let reg = registry(root.path());
    let _palace = palace(&reg, root.path(), &legacy_texts(1)).await;
    let e = Switch::new(HANG);
    let bounds = SliceBounds {
        embed_timeout: Duration::from_secs(3600),
        ..SliceBounds::default()
    };
    let (tx, rx) = watch::channel(false);
    let task = tokio::spawn(run(ticker(&reg, root.path(), &e, bounds), fast(), rx));
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while e.calls() == 0 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the slice never embedded"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    // A dropped sender is a shutdown, exactly like `send(true)`.
    drop(tx);
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("the ticker must not hang inside a slice")
        .expect("the ticker task must not panic");
}

#[tokio::test]
async fn ticker_stops_on_shutdown_while_idle() {
    let root = tempfile::tempdir().unwrap();
    let reg = registry(root.path());
    let e = Switch::new(OK);
    let (tx, rx) = watch::channel(false);
    let task = tokio::spawn(run(ticker(&reg, root.path(), &e, one_drawer()), fast(), rx));
    tokio::time::sleep(Duration::from_millis(50)).await;
    tx.send(true).unwrap();
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("the ticker must leave its idle wait")
        .expect("the ticker task must not panic");
}

#[tokio::test]
#[serial]
async fn ticker_config_reads_its_env_overrides() {
    // #5937: lock order `#[serial]` (the attribute), then `env_test_lock()`.
    let _env = crate::commands::env_test_lock().lock().await;
    let vars = [
        ("TRUSTY_CHUNK_BACKFILL_TICK_MS", "50"),
        ("TRUSTY_CHUNK_BACKFILL_BUSY_SECS", "7"),
        ("TRUSTY_CHUNK_BACKFILL_ERROR_BACKOFF_SECS", "nope"),
        ("TRUSTY_CHUNK_BACKFILL_IDLE_SECS", "9"),
        (DISABLED_ENV, "1"),
    ];
    // SAFETY: serialised by `env_test_lock()`, held for the whole test.
    unsafe {
        for (k, v) in vars {
            std::env::set_var(k, v);
        }
    }
    let c = TickerConfig::from_env();
    let root = tempfile::tempdir().unwrap();
    let (_tx, rx) = watch::channel(false);
    let disabled = spawn_chunk_backfill_ticker(&registry(root.path()), rx);
    unsafe {
        for (k, _) in vars {
            std::env::remove_var(k);
        }
    }
    assert_eq!(c.tick, Duration::from_millis(50));
    assert_eq!(c.busy, Duration::from_secs(7));
    assert_eq!(
        c.backoff,
        TickerConfig::default().backoff,
        "malformed keeps the default"
    );
    assert_eq!(c.idle, Duration::from_secs(9));
    assert!(disabled.is_none(), "{DISABLED_ENV} starts no task");
}
