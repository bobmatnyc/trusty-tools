//! #9735 PR 2: the background chunk backfill ticker, the daemon's caller of
//! [`PalaceHandle::reembed_slice`].
//!
//! Why: drawers stored before chunking (#9275) keep one truncated vector until
//! something re-embeds them. PR 1 built a bounded slice and an advisory
//! progress marker, but only tests called them, so no running daemon repaired
//! a palace without an operator `palace_reembed`.
//! What: [`spawn_chunk_backfill_ticker`] runs one [`Ticker`] on the daemon's
//! shutdown watch. Each tick runs at most one slice on one palace, then
//! rewrites that palace's `chunk_backfill.json` from progress recomputed from
//! the index, so a failure can never advance it and a restart resumes from it.
//! Test: `chunk_backfill_ticker_tests`, `tests/chunk_backfill_ticker_9735.rs`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use tokio::sync::watch;
use tracing::{debug, info, warn};

use crate::engine::embed::Embedder;
use crate::engine::palace::{Palace, PalaceId};
use crate::engine::registry::PalaceRegistry;
use crate::engine::retrieval::{
    ChunkMarker, PalaceHandle, SliceBounds, SliceReport, SliceStop, shared_embedder,
    shared_embedder_initialized,
};

type Emb = Arc<dyn Embedder + Send + Sync>;

/// Set to any non-empty value to keep the ticker from starting.
pub(crate) const DISABLED_ENV: &str = "TRUSTY_CHUNK_BACKFILL_DISABLED";

/// Text of the probe embed run after a slice embed timed out.
const PROBE_TEXT: &str = "chunk backfill probe";

/// How long the ticker waits after each kind of tick.
///
/// Why: the backfill must yield to foreground work and must not hammer a
/// failing embedder or a busy palace.
/// What: one delay per [`Tick`] kind, each overridable by its env var; a
/// malformed value keeps the default. `probe_timeout` bounds the probe embed.
/// Test: `ticker_config_reads_its_env_overrides`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TickerConfig {
    /// After a slice that left work. Env `TRUSTY_CHUNK_BACKFILL_TICK_MS`, default 2 000.
    pub(crate) tick: Duration,
    /// After a busy palace or an embedder still warming. Env
    /// `TRUSTY_CHUNK_BACKFILL_BUSY_SECS`, default 30.
    pub(crate) busy: Duration,
    /// After an error. Env `TRUSTY_CHUNK_BACKFILL_ERROR_BACKOFF_SECS`, default 300.
    pub(crate) backoff: Duration,
    /// When no palace needs work. Env `TRUSTY_CHUNK_BACKFILL_IDLE_SECS`, default 600.
    pub(crate) idle: Duration,
    /// Bound on the probe embed. Env `TRUSTY_CHUNK_BACKFILL_PROBE_SECS`, default 30.
    pub(crate) probe_timeout: Duration,
}

impl Default for TickerConfig {
    fn default() -> Self {
        Self {
            tick: Duration::from_millis(2_000),
            busy: Duration::from_secs(30),
            backoff: Duration::from_secs(300),
            idle: Duration::from_secs(600),
            probe_timeout: Duration::from_secs(30),
        }
    }
}

impl TickerConfig {
    /// The defaults with each env override applied.
    pub(crate) fn from_env() -> Self {
        let d = Self::default();
        let secs = |name: &str, d: Duration| Duration::from_secs(env_u64(name, d.as_secs()));
        let tick_ms = u64::try_from(d.tick.as_millis()).unwrap_or(u64::MAX);
        Self {
            tick: Duration::from_millis(env_u64("TRUSTY_CHUNK_BACKFILL_TICK_MS", tick_ms)),
            busy: secs("TRUSTY_CHUNK_BACKFILL_BUSY_SECS", d.busy),
            backoff: secs("TRUSTY_CHUNK_BACKFILL_ERROR_BACKOFF_SECS", d.backoff),
            idle: secs("TRUSTY_CHUNK_BACKFILL_IDLE_SECS", d.idle),
            probe_timeout: secs("TRUSTY_CHUNK_BACKFILL_PROBE_SECS", d.probe_timeout),
        }
    }

    /// The wait after `tick`.
    pub(crate) fn delay(&self, tick: &Tick) -> Duration {
        match tick {
            Tick::Worked => self.tick,
            Tick::Busy => self.busy,
            Tick::Backoff(_) => self.backoff,
            Tick::Idle => self.idle,
        }
    }
}

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(default)
}

/// What one [`Ticker::tick`] did; it picks the wait before the next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Tick {
    /// A palace was visited and the ticker may move on at once.
    Worked,
    /// No palace needs work, or another process holds maintenance.
    Idle,
    /// The embedder is still warming, or the palace is busy (lock or dream).
    Busy,
    /// An error: a failed probe, a host-level embed failure, a store or
    /// ledger error, an unrun chunk check, or a failed palace listing.
    Backoff(String),
}

/// Where the ticker gets its embedder.
pub(crate) enum Source {
    /// The daemon's shared embedder, once warm-up initialised it. The ticker
    /// never forces a cold init.
    Shared,
    /// A fixed embedder (tests).
    #[cfg(test)]
    Fixed(Emb),
}

/// The ticker's state across ticks.
///
/// Why: progress lives in the index and the marker; the ticker only keeps
/// what decides the next tick.
/// What: `cursor` indexes the on-disk palace list; `probe_pending` holds new
/// slices back after an embed timeout (PR 1 review, binding item 1);
/// `fruitless` holds palaces visited this pass with nothing they could do,
/// so a pass of them ends in [`Tick::Idle`] rather than a hot loop.
pub(crate) struct Ticker {
    registry: PalaceRegistry,
    data_root: PathBuf,
    source: Source,
    pub(crate) bounds: SliceBounds,
    probe_timeout: Duration,
    cursor: usize,
    pub(crate) probe_pending: bool,
    fruitless: HashSet<PalaceId>,
}

/// What [`judge`] decided about one visited palace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Verdict {
    pub(crate) tick: Tick,
    /// Keep the cursor on this palace.
    pub(crate) stay: bool,
    /// Probe the embedder before the next slice.
    pub(crate) probe: bool,
    /// Nothing more this palace can do this pass.
    pub(crate) fruitless: bool,
}

impl Ticker {
    /// A ticker over the palaces under `data_root`.
    pub(crate) fn new(
        registry: PalaceRegistry,
        data_root: PathBuf,
        source: Source,
        bounds: SliceBounds,
        probe_timeout: Duration,
    ) -> Self {
        Self {
            registry,
            data_root,
            source,
            bounds,
            probe_timeout,
            cursor: 0,
            probe_pending: false,
            fruitless: HashSet::new(),
        }
    }

    /// Run at most one slice on the next palace that needs work.
    ///
    /// Why (#9735): drawers stored before chunking must be re-embedded with no
    /// operator command, without starving writers, recall or a sick embedder.
    /// What: returns [`Tick::Idle`] when this process does not hold
    /// maintenance, [`Tick::Busy`] while the shared embedder is uninitialised.
    /// After an embed timeout it probes the embedder first and starts no slice
    /// until the probe succeeds. It then lists the palaces, takes the first
    /// from the cursor whose marker [`ChunkMarker::needs_scan`] and that is
    /// not fruitless this pass, and visits it. A closed palace is opened
    /// through the LRU and released after. No recall runs, so no drawer
    /// access is recorded (#9881).
    /// Test: `chunk_backfill_ticker_tests` (e.g.
    /// `ticker_resumes_from_the_marker_after_a_restart`,
    /// `a_failed_probe_starts_no_slice_until_it_succeeds`).
    pub(crate) async fn tick(&mut self) -> Tick {
        if !self.registry.may_run_maintenance() {
            return Tick::Idle;
        }
        let Some(embedder) = self.embedder().await else {
            return Tick::Busy;
        };
        if self.probe_pending {
            if let Err(e) = probe(&embedder, self.probe_timeout).await {
                return Tick::Backoff(format!("embedder probe after a slice timeout: {e}"));
            }
            info!("#9735: embedder probe succeeded; the chunk backfill resumes");
            self.probe_pending = false;
        }
        let window = embedder.input_window();
        let palaces = match due_palaces(self.data_root.clone(), window).await {
            Ok(p) => p,
            Err(e) => return Tick::Backoff(format!("list palaces: {e}")),
        };
        let n = palaces.len();
        let start = self.cursor % n.max(1);
        let next = (0..n).map(|k| (start + k) % n).find(|&i| {
            let (palace, due) = &palaces[i];
            *due && !self.fruitless.contains(&palace.id)
        });
        let Some(due) = next else {
            self.fruitless.clear();
            return Tick::Idle;
        };
        let palace = &palaces[due].0;
        let verdict = self.visit(palace, &embedder, window).await;
        self.probe_pending |= verdict.probe;
        if verdict.fruitless {
            self.fruitless.insert(palace.id.clone());
        }
        self.cursor = if verdict.stay { due } else { due + 1 };
        verdict.tick
    }

    async fn embedder(&self) -> Option<Emb> {
        match &self.source {
            Source::Shared if shared_embedder_initialized() => shared_embedder().await.ok(),
            Source::Shared => None,
            #[cfg(test)]
            Source::Fixed(e) => Some(Arc::clone(e)),
        }
    }

    /// One slice on `palace`, then its marker from fresh progress.
    async fn visit(&self, palace: &Palace, embedder: &Emb, window: Option<usize>) -> Verdict {
        let id = &palace.id;
        let (handle, opened_here) = match self.handle(id).await {
            Ok(h) => h,
            Err(e) => {
                warn!(palace = %id, "#9735: chunk backfill could not open the palace: {e}");
                return verdict(Tick::Worked, false, true);
            }
        };
        let judged = if handle.is_compacting() {
            verdict(Tick::Busy, true, false)
        } else {
            let report = handle.reembed_slice(embedder, &self.bounds).await;
            // #9735 C2: progress is recomputed from the index, never counted,
            // so a failed slice cannot move the marker past unprocessed drawers.
            let progress = handle
                .embed_health_with(Some(embedder.as_ref()))
                .chunk_progress();
            let marker =
                ChunkMarker::from_progress(&progress, window, last_error(&report), Utc::now());
            write_marker(&palace.data_dir, marker).await;
            log_slice(id, &report, progress.pending, progress.total);
            judge(&report, progress.pending > 0)
        };
        drop(handle);
        if opened_here {
            self.registry.release_if_unreferenced(id);
        }
        judged
    }

    /// The resident handle, or one opened through the LRU (`true`).
    async fn handle(&self, id: &PalaceId) -> Result<(Arc<PalaceHandle>, bool), String> {
        if let Some(h) = self.registry.peek(id) {
            return Ok((h, false));
        }
        let (reg, root, pid) = (self.registry.clone(), self.data_root.clone(), id.clone());
        match tokio::task::spawn_blocking(move || reg.open_palace(&root, &pid)).await {
            Ok(Ok(h)) => Ok((h, true)),
            Ok(Err(e)) => Err(format!("{e:#}")),
            Err(e) => Err(format!("open task failed: {e}")),
        }
    }
}

fn verdict(tick: Tick, stay: bool, fruitless: bool) -> Verdict {
    Verdict {
        tick,
        stay,
        probe: false,
        fruitless,
    }
}

/// Decide the next tick from one slice.
///
/// Why (#9735): an error must back off and must keep the cursor on the
/// palace, so its drawers are retried later and never skipped for good.
/// What: a `ledger_error` backs off whatever else happened (PR 1 review,
/// binding item 2). An embed timeout sets `probe` (binding item 1). A host
/// failure, a store error or an unrun chunk check backs off; a lock timeout
/// is busy. A read-only handle, or a slice that did nothing, is fruitless.
/// A slice that did something stays on its palace while drawers remain.
/// Test: `judge_backs_off_on_every_error_arm_and_stays`,
/// `judge_moves_on_from_a_finished_or_fruitless_palace`.
pub(crate) fn judge(report: &SliceReport, remaining: bool) -> Verdict {
    let probe = report.stop == SliceStop::EmbedTimeout;
    let error = |reason: String| Verdict {
        tick: Tick::Backoff(reason),
        stay: true,
        probe,
        fruitless: false,
    };
    if let Some(e) = &report.ledger_error {
        return error(format!("embed ledger: {e}"));
    }
    match &report.stop {
        SliceStop::EmbedTimeout => Verdict {
            tick: Tick::Worked,
            stay: true,
            probe: true,
            fruitless: false,
        },
        SliceStop::HostFailure => error("embedder failed every drawer".to_string()),
        SliceStop::StoreError(e) => error(format!("vector store: {e}")),
        SliceStop::ChunkCheckUnavailable(e) => error(format!("chunk check unavailable: {e}")),
        SliceStop::LockTimeout(_) => verdict(Tick::Busy, true, false),
        SliceStop::ReadOnly => verdict(Tick::Worked, false, true),
        SliceStop::QueueEmpty | SliceStop::Budget | SliceStop::DrawerCap | SliceStop::ChunkCap => {
            let acted = report.attempted + report.stale > 0;
            verdict(Tick::Worked, acted && remaining, !acted)
        }
    }
}

/// The error a marker records for `report`, if any.
fn last_error(report: &SliceReport) -> Option<String> {
    let stop = match &report.stop {
        SliceStop::HostFailure => Some("embedder failed every drawer".to_string()),
        SliceStop::EmbedTimeout => Some("a drawer's embed timed out".to_string()),
        SliceStop::StoreError(e) | SliceStop::ChunkCheckUnavailable(e) => Some(e.clone()),
        _ => None,
    };
    report.ledger_error.clone().or(stop)
}

fn log_slice(id: &PalaceId, report: &SliceReport, pending: usize, total: usize) {
    if report.attempted + report.stale == 0 {
        debug!(palace = %id, stop = ?report.stop, pending, total, "#9735: chunk backfill slice did nothing");
        return;
    }
    info!(
        palace = %id,
        repaired = report.repaired,
        failed = report.failed,
        stale = report.stale,
        chunks = report.chunks_embedded,
        stop = ?report.stop,
        pending,
        total,
        "#9735: chunk backfill slice"
    );
}

/// Every palace under `root`, each with whether its marker asks for a scan.
async fn due_palaces(root: PathBuf, window: Option<usize>) -> Result<Vec<(Palace, bool)>, String> {
    let listed = tokio::task::spawn_blocking(move || {
        let now = Utc::now();
        PalaceRegistry::list_palaces(&root).map(|palaces| {
            palaces
                .into_iter()
                .map(|p| {
                    let due = ChunkMarker::needs_scan(
                        ChunkMarker::read(&p.data_dir).as_ref(),
                        window,
                        now,
                    );
                    (p, due)
                })
                .collect()
        })
    })
    .await;
    match listed {
        Ok(Ok(p)) => Ok(p),
        Ok(Err(e)) => Err(format!("{e:#}")),
        Err(e) => Err(format!("list task failed: {e}")),
    }
}

/// Rewrite the marker; a failure only costs a rescan, so it is logged.
async fn write_marker(dir: &Path, marker: ChunkMarker) {
    let dir = dir.to_path_buf();
    let shown = dir.clone();
    let written = tokio::task::spawn_blocking(move || marker.write(&dir)).await;
    let error = match written {
        Ok(Ok(())) => return,
        Ok(Err(e)) => format!("{e:#}"),
        Err(e) => format!("marker task failed: {e}"),
    };
    warn!(dir = %shown.display(), "#9735: chunk marker write failed: {error}");
}

/// Embed one short text under `timeout`.
///
/// Why (PR 1 review, binding item 1): after an embed timeout the model may
/// still be busy; starting another slice would ledger one healthy drawer per
/// slice and park one more blocking thread behind it.
/// What: `Ok` only when one vector comes back within `timeout`.
/// Test: `a_failed_probe_starts_no_slice_until_it_succeeds`.
async fn probe(embedder: &Emb, timeout: Duration) -> Result<(), String> {
    let input = [PROBE_TEXT.to_string()];
    match tokio::time::timeout(timeout, embedder.embed_batch(&input)).await {
        Ok(Ok(v)) if v.len() == 1 => Ok(()),
        Ok(Ok(v)) => Err(format!("{} vectors for one input", v.len())),
        Ok(Err(e)) => Err(format!("{e:#}")),
        Err(_) => Err(format!("timed out after {timeout:?}")),
    }
}

/// Resolve when `shutdown` reads `true` or its sender is gone.
async fn stopped(shutdown: &mut watch::Receiver<bool>) {
    // A dropped sender is a shutdown too (see `spawn_shutdown_bridge`).
    let _ = shutdown.wait_for(|stop| *stop).await;
}

/// Tick until shutdown.
///
/// Why: the daemon must stop promptly even while a slice runs.
/// What: races each tick and each wait against `shutdown`. A tick dropped
/// mid-slice is safe: each drawer commits in one upsert or not at all, ledger
/// rows not yet written only cost a retry, and progress is re-derived from
/// the index on the next start.
/// Test: `ticker_stops_on_shutdown_mid_slice`, `ticker_stops_on_shutdown_while_idle`.
pub(crate) async fn run(
    mut ticker: Ticker,
    config: TickerConfig,
    mut shutdown: watch::Receiver<bool>,
) {
    loop {
        let tick = tokio::select! {
            biased;
            () = stopped(&mut shutdown) => break,
            tick = ticker.tick() => tick,
        };
        if let Tick::Backoff(reason) = &tick {
            warn!(
                wait = ?config.backoff,
                "#9735: chunk backfill backs off: {reason}"
            );
        }
        tokio::select! {
            biased;
            () = stopped(&mut shutdown) => break,
            () = tokio::time::sleep(config.delay(&tick)) => {}
        }
    }
    info!("#9735: chunk backfill ticker shutting down");
}

/// Spawn the chunk backfill ticker on the daemon's shutdown watch.
///
/// Why (#9735): without it `reembed_slice` had no caller in the binary.
/// What: `None` (no task) when [`DISABLED_ENV`] is set or the registry has no
/// data root; otherwise one task running [`run`] with the env-configured
/// [`SliceBounds`] and [`TickerConfig`] over the shared embedder.
/// Test: `background_maintenance_reembeds_pre_chunking_drawers` in
/// `tests/chunk_backfill_ticker_9735.rs`.
pub(crate) fn spawn_chunk_backfill_ticker(
    registry: &PalaceRegistry,
    shutdown: watch::Receiver<bool>,
) -> Option<tokio::task::JoinHandle<()>> {
    if std::env::var(DISABLED_ENV).is_ok_and(|v| !v.is_empty()) {
        info!(env = DISABLED_ENV, "#9735: chunk backfill ticker disabled");
        return None;
    }
    let Some(root) = registry.data_root() else {
        debug!("#9735: no data root; chunk backfill ticker not started");
        return None;
    };
    let config = TickerConfig::from_env();
    let ticker = Ticker::new(
        registry.clone(),
        root.to_path_buf(),
        Source::Shared,
        SliceBounds::from_env(),
        config.probe_timeout,
    );
    info!(tick = ?config.tick, "#9735: chunk backfill ticker started");
    Some(tokio::spawn(run(ticker, config, shutdown)))
}
