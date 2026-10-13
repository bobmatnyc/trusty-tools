//! Graceful recycle of the embed sidecar by footprint, age, or call count.
//!
//! Why (#9828): `EmbedderSupervisor` restarted its child only on a crash or a
//! wedge, so a sidecar under steady traffic lived as long as the daemon. The
//! Python/torch sidecar grew to 68 GB over 35 h that way. Whatever leaks inside
//! the child, a bounded lifetime caps the damage; this module supplies that
//! bound without depending on which leak is real.
//! What: [`RecycleConfig`] (three triggers plus the check cadence and the drain
//! bound, each with a `TRUSTY_EMBEDDERD_RECYCLE_*` env override),
//! [`RecycleHooks`] (the injectable footprint sampler, clock, and counters),
//! [`RecycleStats`] / [`RecycleSnapshot`] (what `/health` reports), the
//! per-generation [`RecycleWatch`] that decides when a recycle is due, the
//! [`TrackedClient`] wrapper that counts calls and in-flight calls, and
//! [`perform_recycle`], which spawns the replacement before it drains and
//! stops the old child.
//! Test: `supervisor_recycle_tests.rs`.

use std::path::Path;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use tokio::process::Child;
use tokio::sync::{RwLock, watch};

use super::supervisor_config::parse_env;
use super::{EmbedderClient, StdioEmbedderClient, SupervisorConfig};
use crate::embedder_client::EmbedderError;

/// Consecutive failed footprint reads that count as "cannot observe".
const FOOTPRINT_UNREADABLE_LIMIT: u32 = 3;
/// How long an idle old client must stay idle before it is stopped while a
/// caller still holds a clone of it (covers the clone-then-call window).
const DRAIN_SETTLE: Duration = Duration::from_secs(2);
/// Drain poll cadence.
const DRAIN_POLL: Duration = Duration::from_millis(20);

/// Recycle thresholds and cadence. `0` disables a trigger.
///
/// Why: three independent bounds, so a leak is capped whether it grows with
/// memory, wall time, or traffic.
/// What: see each field's env var; [`RecycleConfig::from_env`] reads them.
/// Test: `recycle_config_from_env_defaults_and_overrides`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecycleConfig {
    /// Recycle once the sidecar's footprint exceeds this many MB.
    /// Env: `TRUSTY_EMBEDDERD_RECYCLE_FOOTPRINT_MB` (default 4096).
    pub footprint_ceiling_mb: u64,
    /// Recycle once the sidecar is this old (seconds).
    /// Env: `TRUSTY_EMBEDDERD_RECYCLE_MAX_AGE_SECS` (default 21600 = 6 h).
    pub max_age_secs: u64,
    /// Recycle once the sidecar has served this many embed calls.
    /// Env: `TRUSTY_EMBEDDERD_RECYCLE_MAX_CALLS` (default 100000).
    pub max_calls: u64,
    /// How often the triggers are evaluated.
    /// Env: `TRUSTY_EMBEDDERD_RECYCLE_CHECK_SECS` (default 30, minimum 1).
    pub check_interval: Duration,
    /// Upper bound on waiting for the old sidecar's in-flight calls (seconds).
    /// Longer than the 30 s per-call timeout, so every call ends on its own.
    /// Env: `TRUSTY_EMBEDDERD_RECYCLE_DRAIN_SECS` (default 60).
    pub drain_secs: u64,
}

impl Default for RecycleConfig {
    fn default() -> Self {
        Self {
            footprint_ceiling_mb: 4096,
            max_age_secs: 21_600,
            max_calls: 100_000,
            check_interval: Duration::from_secs(30),
            drain_secs: 60,
        }
    }
}

impl RecycleConfig {
    /// Read the `TRUSTY_EMBEDDERD_RECYCLE_*` env vars over the defaults.
    /// Test: `recycle_config_from_env_defaults_and_overrides`.
    pub fn from_env() -> Self {
        let def = Self::default();
        let check_secs: u64 = parse_env(
            "TRUSTY_EMBEDDERD_RECYCLE_CHECK_SECS",
            def.check_interval.as_secs(),
        );
        Self {
            footprint_ceiling_mb: parse_env(
                "TRUSTY_EMBEDDERD_RECYCLE_FOOTPRINT_MB",
                def.footprint_ceiling_mb,
            ),
            max_age_secs: parse_env("TRUSTY_EMBEDDERD_RECYCLE_MAX_AGE_SECS", def.max_age_secs),
            max_calls: parse_env("TRUSTY_EMBEDDERD_RECYCLE_MAX_CALLS", def.max_calls),
            // A zero interval would turn the check into a busy loop.
            check_interval: Duration::from_secs(check_secs.max(1)),
            drain_secs: parse_env("TRUSTY_EMBEDDERD_RECYCLE_DRAIN_SECS", def.drain_secs),
        }
    }

    /// True when at least one trigger is on.
    pub fn enabled(&self) -> bool {
        self.footprint_ceiling_mb > 0 || self.max_age_secs > 0 || self.max_calls > 0
    }
}

/// Reads a process footprint in MB; `None` means the read failed.
pub type FootprintSampler = Arc<dyn Fn(u32) -> Option<u64> + Send + Sync>;
/// Monotonic time since an arbitrary fixed origin.
pub type RecycleClock = Arc<dyn Fn() -> Duration + Send + Sync>;

/// Injection seam for the recycle decision.
///
/// Why: a deterministic test needs to feed a footprint, move time, and read
/// the counters without a real leak or a real six-hour wait.
/// What: the default reads [`crate::sys_metrics::process_rss_mb`] (macOS
/// physical footprint, Linux `VmRSS`), a process-monotonic clock, and the
/// process-wide [`process_recycle_stats`].
/// Test: every test in `supervisor_recycle_tests.rs` injects one.
#[derive(Clone)]
pub struct RecycleHooks {
    pub sampler: FootprintSampler,
    pub clock: RecycleClock,
    pub stats: Arc<RecycleStats>,
}

impl Default for RecycleHooks {
    fn default() -> Self {
        static ORIGIN: OnceLock<Instant> = OnceLock::new();
        let origin = *ORIGIN.get_or_init(Instant::now);
        Self {
            sampler: Arc::new(crate::sys_metrics::process_rss_mb),
            clock: Arc::new(move || origin.elapsed()),
            stats: process_recycle_stats(),
        }
    }
}

/// The counters every supervisor in this process records into by default.
///
/// Why: a daemon builds a fresh supervisor per lazy spawn and per backend
/// swap; one process-wide set keeps `/health`'s totals across all of them
/// without threading a handle through each construction site.
pub fn process_recycle_stats() -> Arc<RecycleStats> {
    static STATS: OnceLock<Arc<RecycleStats>> = OnceLock::new();
    Arc::clone(STATS.get_or_init(|| Arc::new(RecycleStats::new())))
}

/// Why a recycle happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecycleTrigger {
    Footprint,
    MaxAge,
    MaxCalls,
    /// The footprint could not be read for `FOOTPRINT_UNREADABLE_LIMIT` checks.
    FootprintUnreadable,
}

impl RecycleTrigger {
    /// Stable snake_case name used in logs and `/health`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Footprint => "footprint",
            Self::MaxAge => "max_age",
            Self::MaxCalls => "max_calls",
            Self::FootprintUnreadable => "footprint_unreadable",
        }
    }
}

#[derive(Debug, Clone, Default)]
struct LastRecycle {
    trigger: Option<&'static str>,
    old_pid: Option<u32>,
    new_pid: Option<u32>,
    at_unix_secs: Option<u64>,
}

/// Recycle counters, shared between the supervision loop and `/health`.
/// Test: asserted through [`RecycleStats::snapshot`] in every recycle test.
#[derive(Debug, Default)]
pub struct RecycleStats {
    footprint: AtomicU64,
    max_age: AtomicU64,
    max_calls: AtomicU64,
    footprint_unreadable: AtomicU64,
    spawn_failures: AtomicU64,
    footprint_read_errors: AtomicU64,
    last: Mutex<LastRecycle>,
}

/// Point-in-time copy of [`RecycleStats`], serialized into `/health`.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct RecycleSnapshot {
    pub recycles_total: u64,
    pub footprint: u64,
    pub max_age: u64,
    pub max_calls: u64,
    pub footprint_unreadable: u64,
    /// Recycles abandoned because the replacement failed to start (the old
    /// sidecar kept serving).
    pub spawn_failures: u64,
    pub footprint_read_errors: u64,
    pub last_trigger: Option<String>,
    pub last_old_pid: Option<u32>,
    pub last_new_pid: Option<u32>,
    pub last_recycle_unix_secs: Option<u64>,
}

impl RecycleStats {
    pub fn new() -> Self {
        Self::default()
    }

    fn record(&self, trigger: RecycleTrigger, old_pid: u32, new_pid: u32) {
        let counter = match trigger {
            RecycleTrigger::Footprint => &self.footprint,
            RecycleTrigger::MaxAge => &self.max_age,
            RecycleTrigger::MaxCalls => &self.max_calls,
            RecycleTrigger::FootprintUnreadable => &self.footprint_unreadable,
        };
        counter.fetch_add(1, Ordering::Relaxed);
        let at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .map(|d| d.as_secs());
        if let Ok(mut last) = self.last.lock() {
            *last = LastRecycle {
                trigger: Some(trigger.as_str()),
                old_pid: Some(old_pid),
                new_pid: Some(new_pid),
                at_unix_secs: at,
            };
        }
    }

    /// Copy the counters out.
    pub fn snapshot(&self) -> RecycleSnapshot {
        let footprint = self.footprint.load(Ordering::Relaxed);
        let max_age = self.max_age.load(Ordering::Relaxed);
        let max_calls = self.max_calls.load(Ordering::Relaxed);
        let unreadable = self.footprint_unreadable.load(Ordering::Relaxed);
        let last = self.last.lock().map(|l| l.clone()).unwrap_or_default();
        RecycleSnapshot {
            recycles_total: footprint + max_age + max_calls + unreadable,
            footprint,
            max_age,
            max_calls,
            footprint_unreadable: unreadable,
            spawn_failures: self.spawn_failures.load(Ordering::Relaxed),
            footprint_read_errors: self.footprint_read_errors.load(Ordering::Relaxed),
            last_trigger: last.trigger.map(str::to_string),
            last_old_pid: last.old_pid,
            last_new_pid: last.new_pid,
            last_recycle_unix_secs: last.at_unix_secs,
        }
    }
}

/// `StdioEmbedderClient` plus a call counter and an in-flight counter.
///
/// Why: max-calls needs a count of real embed calls (never the startup probe,
/// which runs on the bare client before wrapping), and the drain needs to
/// know when the old sidecar has no call left.
pub(crate) struct TrackedClient {
    inner: StdioEmbedderClient,
    calls: AtomicU64,
    in_flight: Arc<AtomicU32>,
}

impl TrackedClient {
    pub(crate) fn new(inner: StdioEmbedderClient) -> Arc<Self> {
        Arc::new(Self {
            inner,
            calls: AtomicU64::new(0),
            in_flight: Arc::new(AtomicU32::new(0)),
        })
    }

    pub(crate) fn unhealthy_signal(&self) -> watch::Receiver<bool> {
        self.inner.unhealthy_signal()
    }
}

/// Decrements the in-flight count on every exit path, including cancellation.
struct InFlight(Arc<AtomicU32>);

impl Drop for InFlight {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

#[async_trait]
impl EmbedderClient for TrackedClient {
    async fn embed_batch(&self, texts: Vec<String>) -> Result<Vec<Vec<f32>>, EmbedderError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.in_flight.fetch_add(1, Ordering::AcqRel);
        let _guard = InFlight(Arc::clone(&self.in_flight));
        self.inner.embed_batch(texts).await
    }

    fn last_reported_device(&self) -> Option<String> {
        self.inner.last_reported_device()
    }
}

/// Recycle bookkeeping for the live sidecar generation.
///
/// Why: the decision is pure given (footprint, age, calls), so it lives apart
/// from the process plumbing in [`perform_recycle`].
/// What: `born` and `ticks` reset on every new generation (spawn, crash
/// respawn, recycle); `read_failures` counts consecutive failed footprint reads;
/// `unreadable_latched` survives generations — see [`Self::due`].
pub(crate) struct RecycleWatch {
    born: Duration,
    ticks: u32,
    read_failures: u32,
    unreadable_latched: bool,
    warned_read_failure: bool,
}

impl RecycleWatch {
    pub(crate) fn new(hooks: &RecycleHooks) -> Self {
        Self {
            born: (hooks.clock)(),
            ticks: 0,
            read_failures: 0,
            unreadable_latched: false,
            warned_read_failure: false,
        }
    }

    /// Start a new generation (keeps the unreadable latch).
    pub(crate) fn reset(&mut self, hooks: &RecycleHooks) {
        self.born = (hooks.clock)();
        self.ticks = 0;
        self.read_failures = 0;
    }

    /// Decide whether the live sidecar is due for a recycle.
    ///
    /// Footprint-read failure policy (#9828): a failed read is "unknown",
    /// never "under the ceiling". `FOOTPRINT_UNREADABLE_LIMIT` consecutive
    /// failures recycle once (`footprint_unreadable`) and set a latch; while
    /// the latch is set, further failures (on any generation) recycle nothing
    /// — the sampler, not the sidecar, is then the likely fault, and the age
    /// and call bounds still apply. One successful read clears the latch.
    pub(crate) fn due(
        &mut self,
        cfg: &RecycleConfig,
        hooks: &RecycleHooks,
        pid: u32,
        tracked: &TrackedClient,
    ) -> Option<RecycleTrigger> {
        // Never judge a generation before its second check: a ceiling set
        // below the sidecar's resting size must not respawn on every tick.
        self.ticks = self.ticks.saturating_add(1);
        if self.ticks < 2 {
            return None;
        }
        let age = (hooks.clock)().saturating_sub(self.born);
        if cfg.footprint_ceiling_mb > 0 && pid != 0 {
            match (hooks.sampler)(pid) {
                Some(mb) => {
                    self.read_failures = 0;
                    self.unreadable_latched = false;
                    self.warned_read_failure = false;
                    if mb > cfg.footprint_ceiling_mb {
                        tracing::info!(
                            pid,
                            footprint_mb = mb,
                            ceiling_mb = cfg.footprint_ceiling_mb,
                            "EmbedderSupervisor: sidecar footprint over the recycle ceiling"
                        );
                        return Some(RecycleTrigger::Footprint);
                    }
                }
                None => {
                    hooks
                        .stats
                        .footprint_read_errors
                        .fetch_add(1, Ordering::Relaxed);
                    self.read_failures += 1;
                    if !self.warned_read_failure {
                        self.warned_read_failure = true;
                        tracing::warn!(
                            pid,
                            "EmbedderSupervisor: cannot read the sidecar footprint — \
                             recycling after {FOOTPRINT_UNREADABLE_LIMIT} failed reads"
                        );
                    }
                    if self.read_failures >= FOOTPRINT_UNREADABLE_LIMIT && !self.unreadable_latched
                    {
                        self.unreadable_latched = true;
                        return Some(RecycleTrigger::FootprintUnreadable);
                    }
                }
            }
        }
        if cfg.max_age_secs > 0 && age >= Duration::from_secs(cfg.max_age_secs) {
            return Some(RecycleTrigger::MaxAge);
        }
        if cfg.max_calls > 0 && tracked.calls.load(Ordering::Relaxed) >= cfg.max_calls {
            return Some(RecycleTrigger::MaxCalls);
        }
        None
    }
}

/// Everything the supervision loop threads into a recycle.
pub(crate) struct RecycleCtx<'a> {
    pub binary_path: &'a Path,
    pub config: &'a SupervisorConfig,
    pub hooks: &'a RecycleHooks,
    pub child_slot: &'a tokio::sync::Mutex<Option<Child>>,
    pub client_slot: &'a RwLock<Arc<dyn EmbedderClient>>,
    pub child_pid_slot: &'a AtomicU32,
    pub shutdown_rx: &'a watch::Receiver<bool>,
}

/// Replace the live sidecar with a fresh one, then drain and stop the old.
///
/// Why (#9828): a recycle must never fail or drop a caller's call and must
/// never count as a crash. Spawning first means there is never a moment with
/// no sidecar; draining before the kill means no in-flight call is lost.
/// What: spawns and probes the replacement; on failure logs WARN, counts a
/// `spawn_failure`, and leaves the old sidecar serving (returns `Err(old)`). On
/// success swaps `client_slot`, moves the new child into `child_slot`,
/// publishes the new pid, records the recycle in [`RecycleStats`], then waits until no caller holds the old client
/// (or it has been idle for `DRAIN_SETTLE`), bounded by `drain_secs` and cut
/// short by a shutdown request; then kills and reaps the old child. Logs the
/// trigger and both pids at INFO. Returns the new tracked client. `old` is
/// taken by value so the drain can see when it holds the last reference.
/// Test: `recycle_on_footprint_ceiling_keeps_inflight_call_and_counts_no_failure`.
pub(crate) async fn perform_recycle(
    ctx: RecycleCtx<'_>,
    trigger: RecycleTrigger,
    old: Arc<TrackedClient>,
) -> Result<Arc<TrackedClient>, Arc<TrackedClient>> {
    let old_pid = ctx.child_pid_slot.load(Ordering::Acquire);
    let (new_child, new_client) = match super::spawn_child(ctx.binary_path, ctx.config).await {
        Ok(pair) => pair,
        Err(e) => {
            ctx.hooks
                .stats
                .spawn_failures
                .fetch_add(1, Ordering::Relaxed);
            tracing::warn!(
                trigger = trigger.as_str(),
                old_pid,
                "EmbedderSupervisor: recycle replacement failed to start — keeping the \
                     current sidecar: {e:#}"
            );
            return Err(old);
        }
    };
    let new_pid = new_child.id().unwrap_or(0);
    let tracked = TrackedClient::new(new_client);
    *ctx.client_slot.write().await = Arc::clone(&tracked) as Arc<dyn EmbedderClient>;
    let old_child = ctx.child_slot.lock().await.replace(new_child);
    ctx.child_pid_slot.store(new_pid, Ordering::Release);
    tracing::info!(
        trigger = trigger.as_str(),
        old_pid,
        new_pid,
        "EmbedderSupervisor: recycling sidecar (old pid={old_pid}, new pid={new_pid}, \
         trigger={})",
        trigger.as_str()
    );

    // Counted once the new sidecar serves; the drain below only tidies up.
    ctx.hooks.stats.record(trigger, old_pid, new_pid);
    drain(&old, ctx.config.recycle.drain_secs, ctx.shutdown_rx).await;
    drop(old);
    if let Some(mut child) = old_child {
        if let Err(e) = child.start_kill() {
            tracing::debug!("EmbedderSupervisor: recycled sidecar already exited: {e}");
        }
        let _ = child.wait().await;
    }
    Ok(tracked)
}

/// Wait until no caller can still be using `old`.
async fn drain(old: &Arc<TrackedClient>, drain_secs: u64, shutdown_rx: &watch::Receiver<bool>) {
    let start = tokio::time::Instant::now();
    let bound = Duration::from_secs(drain_secs);
    let mut idle_since: Option<tokio::time::Instant> = None;
    loop {
        // Only this function holds the Arc: nobody can start a call on it.
        if Arc::strong_count(old) == 1 || *shutdown_rx.borrow() {
            return;
        }
        let now = tokio::time::Instant::now();
        if old.in_flight.load(Ordering::Acquire) == 0 {
            let since = *idle_since.get_or_insert(now);
            if now.duration_since(since) >= DRAIN_SETTLE {
                return;
            }
        } else {
            idle_since = None;
        }
        if now.duration_since(start) >= bound {
            tracing::warn!(
                in_flight = old.in_flight.load(Ordering::Acquire),
                "EmbedderSupervisor: recycle drain bound ({drain_secs}s) reached — \
                 stopping the old sidecar"
            );
            return;
        }
        tokio::time::sleep(DRAIN_POLL).await;
    }
}
