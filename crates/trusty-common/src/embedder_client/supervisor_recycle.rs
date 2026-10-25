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
//! [`TrackedClient`] wrapper that counts calls and in-flight calls and, once
//! retired, forwards new calls to its successor. The recycle back-off lives
//! on [`RecycleWatch`]; `supervisor_recycle_swap.rs` holds `perform_recycle`,
//! which spawns the replacement before it drains and stops the old child.
//! #10207: the hard footprint limit above the soft ceiling is judged here
//! too, but its breach is a crash-path kill, not a recycle.
//! Test: `supervisor_recycle_tests.rs`.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use tokio::sync::watch;

use super::supervisor_config::parse_env;
use super::{EmbedderClient, StdioEmbedderClient};
use crate::embedder_client::EmbedderError;

// #9828: the swap / drain half, split out for the 500-SLOC cap.
#[path = "supervisor_recycle_swap.rs"]
mod swap;
pub(crate) use swap::{RecycleCtx, RecycleOutcome, perform_recycle};

/// Consecutive failed footprint reads that count as "cannot observe".
const FOOTPRINT_UNREADABLE_LIMIT: u32 = 3;
/// #9828: floor on the check interval; `tokio::time::interval` panics on zero.
const MIN_CHECK_INTERVAL: Duration = Duration::from_millis(10);
/// #9828: ceiling on the recycle back-off hold.
const MAX_BACKOFF: Duration = Duration::from_secs(3600);

/// Recycle thresholds and cadence. `0` disables a trigger.
///
/// Why: three independent bounds, so a leak is capped whether it grows with
/// memory, wall time, or traffic.
/// What: see each field's env var; [`RecycleConfig::from_env`] reads them.
/// `#[non_exhaustive]`: outside this crate, start from `default()` or
/// `from_env()` and set fields.
/// Test: `recycle_config_from_env_defaults_and_overrides`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
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
    /// How often the triggers are evaluated; also the recycle back-off base.
    /// Below 10 ms (zero included) it runs at 10 ms.
    /// Env: `TRUSTY_EMBEDDERD_RECYCLE_CHECK_SECS` (default 30, minimum 1).
    pub check_interval: Duration,
    /// How long the drain waits with no in-flight call on the old sidecar
    /// finishing before it stops that sidecar anyway (seconds). Never less
    /// than the per-call timeout plus 5 s, so `0` means exactly that floor.
    /// Env: `TRUSTY_EMBEDDERD_RECYCLE_DRAIN_SECS` (default 60).
    pub drain_secs: u64,
    /// #10207: hard footprint limit (MB), meant to sit above the soft
    /// ceiling. Over it, the sidecar is killed through the crash path, even
    /// during a recycle back-off hold, and the kill counts toward
    /// `max_restarts`. `None` = twice `footprint_ceiling_mb`; `Some(0)`
    /// disables it; a value not above the soft ceiling is ignored (`None`
    /// applies). See [`Self::effective_hard_footprint_mb`].
    /// Env: `TRUSTY_EMBEDDERD_RECYCLE_HARD_FOOTPRINT_MB` (default 2x the soft ceiling).
    pub hard_footprint_mb: Option<u64>,
    /// #10207: ceiling on the startup-probe load multiplier `1 + load1/ncpu`
    /// applied to recycle replacements, crash respawns and the first spawn.
    /// `0` or `1` turns load scaling off.
    /// Env: `TRUSTY_EMBEDDERD_STARTUP_LOAD_SCALE_MAX` (default 4).
    pub startup_load_scale_max: u32,
}

impl Default for RecycleConfig {
    fn default() -> Self {
        Self {
            footprint_ceiling_mb: 4096,
            max_age_secs: 21_600,
            max_calls: 100_000,
            check_interval: Duration::from_secs(30),
            drain_secs: 60,
            hard_footprint_mb: None,
            startup_load_scale_max: 4,
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
        let footprint_ceiling_mb = parse_env(
            "TRUSTY_EMBEDDERD_RECYCLE_FOOTPRINT_MB",
            def.footprint_ceiling_mb,
        );
        Self {
            footprint_ceiling_mb,
            max_age_secs: parse_env("TRUSTY_EMBEDDERD_RECYCLE_MAX_AGE_SECS", def.max_age_secs),
            max_calls: parse_env("TRUSTY_EMBEDDERD_RECYCLE_MAX_CALLS", def.max_calls),
            // A zero interval would turn the check into a busy loop.
            check_interval: Duration::from_secs(check_secs.max(1)),
            drain_secs: parse_env("TRUSTY_EMBEDDERD_RECYCLE_DRAIN_SECS", def.drain_secs),
            hard_footprint_mb: hard_footprint_from_env(footprint_ceiling_mb),
            startup_load_scale_max: parse_env(
                "TRUSTY_EMBEDDERD_STARTUP_LOAD_SCALE_MAX",
                def.startup_load_scale_max,
            ),
        }
    }

    /// True when at least one trigger (or the hard limit) is on.
    pub fn enabled(&self) -> bool {
        self.footprint_ceiling_mb > 0
            || self.max_age_secs > 0
            || self.max_calls > 0
            || self.effective_hard_footprint_mb().is_some()
    }

    /// The hard footprint limit in force, in MB; `None` = off.
    ///
    /// Why (#10207): a footprint recycle whose replacement cannot start kept
    /// the old sidecar, and nothing bounded it after that; it reached 10 GB.
    /// What: `Some(0)` is off; an explicit value is used when the soft
    /// ceiling is off or the value is above it; otherwise (unset, or not
    /// above the soft ceiling) twice the soft ceiling, or off when the soft
    /// ceiling is off too.
    /// Test: `hard_footprint_limit_resolution`.
    pub fn effective_hard_footprint_mb(&self) -> Option<u64> {
        let soft = self.footprint_ceiling_mb;
        match self.hard_footprint_mb {
            Some(0) => None,
            Some(mb) if soft == 0 || mb > soft => Some(mb),
            _ => (soft > 0).then(|| soft.saturating_mul(2)),
        }
    }

    /// The check interval the supervisor runs, floored at 10 ms.
    ///
    /// Why (#9828): a hand-built `check_interval` of zero panicked the
    /// supervision task inside `tokio::time::interval_at`, and the sidecar
    /// died with it.
    /// Test: `zero_check_interval_does_not_panic_the_supervision_task`.
    pub fn effective_check_interval(&self) -> Duration {
        self.check_interval.max(MIN_CHECK_INTERVAL)
    }
}

/// #10207: read `TRUSTY_EMBEDDERD_RECYCLE_HARD_FOOTPRINT_MB`; a value not
/// above the soft ceiling is ignored with a WARN.
/// Test: `recycle_config_from_env_defaults_and_overrides`.
fn hard_footprint_from_env(soft: u64) -> Option<u64> {
    const VAR: &str = "TRUSTY_EMBEDDERD_RECYCLE_HARD_FOOTPRINT_MB";
    let mb: u64 = std::env::var(VAR).ok()?.trim().parse().ok()?;
    if mb != 0 && soft > 0 && mb <= soft {
        tracing::warn!(
            hard_mb = mb,
            soft_mb = soft,
            "EmbedderSupervisor: {VAR}={mb} is not above the recycle ceiling ({soft} MB) — \
             ignored; the hard limit stays at twice the ceiling"
        );
        return None;
    }
    Some(mb)
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
    /// #10207: the footprint is over the hard limit. Not a recycle: the
    /// supervisor kills the sidecar through its crash path.
    HardFootprint,
}

impl RecycleTrigger {
    /// Stable snake_case name used in logs and `/health`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Footprint => "footprint",
            Self::MaxAge => "max_age",
            Self::MaxCalls => "max_calls",
            Self::FootprintUnreadable => "footprint_unreadable",
            Self::HardFootprint => "hard_footprint",
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

/// #9828: the recycle back-off as `/health` shows it.
#[derive(Debug, Clone, Default)]
struct BackoffView {
    level: u32,
    cause: Option<&'static str>,
    until_unix_secs: Option<u64>,
}

fn unix_now() -> Option<u64> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs())
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
    hard_footprint_kills: AtomicU64,
    last: Mutex<LastRecycle>,
    backoff: Mutex<BackoffView>,
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
    /// #10207: sidecars killed for crossing the hard footprint limit. Not
    /// part of `recycles_total`; each kill counts toward `max_restarts`.
    pub hard_footprint_kills: u64,
    pub last_trigger: Option<String>,
    pub last_old_pid: Option<u32>,
    pub last_new_pid: Option<u32>,
    pub last_recycle_unix_secs: Option<u64>,
    /// #9828: consecutive back-off steps (premature recycles or failed
    /// replacement spawns); `0` = no back-off.
    pub backoff_level: u32,
    /// `premature_recycle` (a fresh sidecar already over a trigger) or
    /// `spawn_failure`; `None` when `backoff_level` is 0.
    pub backoff_cause: Option<String>,
    /// Wall-clock second the current hold ends; no recycle is tried before it.
    pub backoff_until_unix_secs: Option<u64>,
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
            RecycleTrigger::HardFootprint => &self.hard_footprint_kills,
        };
        counter.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut last) = self.last.lock() {
            *last = LastRecycle {
                trigger: Some(trigger.as_str()),
                old_pid: Some(old_pid),
                new_pid: Some(new_pid),
                at_unix_secs: unix_now(),
            };
        }
    }

    /// #10207: count one hard-limit kill.
    pub(crate) fn record_hard_kill(&self) {
        self.hard_footprint_kills.fetch_add(1, Ordering::Relaxed);
    }

    fn set_backoff(&self, view: BackoffView) {
        if let Ok(mut b) = self.backoff.lock() {
            *b = view;
        }
    }

    /// Copy the counters out.
    pub fn snapshot(&self) -> RecycleSnapshot {
        let footprint = self.footprint.load(Ordering::Relaxed);
        let max_age = self.max_age.load(Ordering::Relaxed);
        let max_calls = self.max_calls.load(Ordering::Relaxed);
        let unreadable = self.footprint_unreadable.load(Ordering::Relaxed);
        let last = self.last.lock().map(|l| l.clone()).unwrap_or_default();
        let backoff = self.backoff.lock().map(|b| b.clone()).unwrap_or_default();
        RecycleSnapshot {
            recycles_total: footprint + max_age + max_calls + unreadable,
            footprint,
            max_age,
            max_calls,
            footprint_unreadable: unreadable,
            spawn_failures: self.spawn_failures.load(Ordering::Relaxed),
            footprint_read_errors: self.footprint_read_errors.load(Ordering::Relaxed),
            hard_footprint_kills: self.hard_footprint_kills.load(Ordering::Relaxed),
            last_trigger: last.trigger.map(str::to_string),
            last_old_pid: last.old_pid,
            last_new_pid: last.new_pid,
            last_recycle_unix_secs: last.at_unix_secs,
            backoff_level: backoff.level,
            backoff_cause: backoff.cause.map(str::to_string),
            backoff_until_unix_secs: backoff.until_unix_secs,
        }
    }
}

/// `StdioEmbedderClient` plus call, in-flight and finished counters.
///
/// Why: max-calls needs a count of real embed calls (never the startup probe,
/// which runs on the bare client before wrapping), and the drain needs to
/// know when the old sidecar has no call left and whether its calls progress.
/// What (#9828): once [`Self::retire`] names a successor, a new call forwards
/// there instead of reaching the old sidecar. `embed_batch` raises
/// `in_flight` before it reads `retired`, and `retire` sets `retired` before
/// the drain reads `in_flight`, both `SeqCst`: so a drain that reads zero
/// knows no later call can reach the old sidecar.
/// Test: `recycle_drain_waits_for_calls_slower_than_drain_secs`.
pub(crate) struct TrackedClient {
    inner: StdioEmbedderClient,
    calls: AtomicU64,
    in_flight: AtomicU32,
    finished: AtomicU64,
    retired: AtomicBool,
    successor: OnceLock<Arc<dyn EmbedderClient>>,
}

impl TrackedClient {
    pub(crate) fn new(inner: StdioEmbedderClient) -> Arc<Self> {
        Arc::new(Self {
            inner,
            calls: AtomicU64::new(0),
            in_flight: AtomicU32::new(0),
            finished: AtomicU64::new(0),
            retired: AtomicBool::new(false),
            successor: OnceLock::new(),
        })
    }

    pub(crate) fn unhealthy_signal(&self) -> watch::Receiver<bool> {
        self.inner.unhealthy_signal()
    }

    /// Send every later call to `successor` (set before the flag).
    fn retire(&self, successor: Arc<dyn EmbedderClient>) {
        let _ = self.successor.set(successor);
        self.retired.store(true, Ordering::SeqCst);
    }
}

/// Ends one call on every exit path, including cancellation.
struct InFlight<'a>(&'a TrackedClient);

impl Drop for InFlight<'_> {
    fn drop(&mut self) {
        self.0.finished.fetch_add(1, Ordering::AcqRel);
        self.0.in_flight.fetch_sub(1, Ordering::SeqCst);
    }
}

#[async_trait]
impl EmbedderClient for TrackedClient {
    async fn embed_batch(&self, texts: Vec<String>) -> Result<Vec<Vec<f32>>, EmbedderError> {
        // #9828: count first, then look (see the type doc).
        self.in_flight.fetch_add(1, Ordering::SeqCst);
        if self.retired.load(Ordering::SeqCst)
            && let Some(next) = self.successor.get()
        {
            self.in_flight.fetch_sub(1, Ordering::SeqCst);
            return next.embed_batch(texts).await;
        }
        let _guard = InFlight(self);
        self.calls.fetch_add(1, Ordering::Relaxed);
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
/// `unreadable_latched` survives generations — see [`Self::due`]. The back-off
/// fields (#9828) also survive generations: `quick_streak` counts consecutive
/// recycles judged on a generation's first look, `fail_streak` consecutive
/// failed replacement spawns, and `hold_until` blocks every recycle until it
/// passes. The hold runs on tokio time, not the injectable clock.
pub(crate) struct RecycleWatch {
    born: Duration,
    ticks: u32,
    read_failures: u32,
    unreadable_latched: bool,
    warned_read_failure: bool,
    first_look: bool,
    quick_streak: u32,
    fail_streak: u32,
    hold_until: Option<tokio::time::Instant>,
}

impl RecycleWatch {
    pub(crate) fn new(hooks: &RecycleHooks) -> Self {
        Self {
            born: (hooks.clock)(),
            ticks: 0,
            read_failures: 0,
            unreadable_latched: false,
            warned_read_failure: false,
            first_look: false,
            quick_streak: 0,
            fail_streak: 0,
            hold_until: None,
        }
    }

    /// Start a new generation (keeps the unreadable latch and the back-off).
    pub(crate) fn reset(&mut self, hooks: &RecycleHooks) {
        self.born = (hooks.clock)();
        self.ticks = 0;
        self.read_failures = 0;
    }

    /// A recycle swapped in a new generation: reset, then back off when the
    /// old generation was recycled on its first judged check.
    ///
    /// Why (#9828): a ceiling below the sidecar's resting footprint made
    /// every fresh sidecar due on its first look — a recycle (and a model
    /// reload) about once a minute, forever.
    /// What: such a recycle raises `quick_streak` and holds the next recycle
    /// for [`backoff_delay`]; any other recycle clears both streaks.
    /// Test: `recycle_backs_off_when_a_fresh_sidecar_is_already_over_the_ceiling`.
    pub(crate) fn after_swap(&mut self, cfg: &RecycleConfig, hooks: &RecycleHooks) {
        self.reset(hooks);
        self.fail_streak = 0;
        self.quick_streak = if self.first_look {
            self.quick_streak.saturating_add(1)
        } else {
            0
        };
        self.hold(cfg, hooks, self.quick_streak, "premature_recycle");
    }

    /// The replacement failed to start: hold the next attempt.
    ///
    /// Why (#9828): the retry ran on every tick, each one blocking the loop
    /// for up to the startup timeout.
    /// Test: `recycle_spawn_failures_back_off`.
    pub(crate) fn after_spawn_failure(&mut self, cfg: &RecycleConfig, hooks: &RecycleHooks) {
        self.fail_streak = self.fail_streak.saturating_add(1);
        self.hold(cfg, hooks, self.fail_streak, "spawn_failure");
    }

    fn hold(&mut self, cfg: &RecycleConfig, hooks: &RecycleHooks, level: u32, cause: &'static str) {
        if level == 0 {
            self.hold_until = None;
            hooks.stats.set_backoff(BackoffView::default());
            return;
        }
        let delay = backoff_delay(cfg.effective_check_interval(), level);
        self.hold_until = Some(tokio::time::Instant::now() + delay);
        tracing::warn!(
            level,
            cause,
            hold_secs = delay.as_secs_f64(),
            "EmbedderSupervisor: recycle back-off — no recycle for {delay:?}"
        );
        hooks.stats.set_backoff(BackoffView {
            level,
            cause: Some(cause),
            until_unix_secs: unix_now().map(|t| t + delay.as_secs()),
        });
    }

    /// Decide whether the live sidecar is due for a recycle.
    ///
    /// Footprint-read failure policy (#9828): a failed read is "unknown",
    /// never "under the ceiling". `FOOTPRINT_UNREADABLE_LIMIT` consecutive
    /// failures recycle once (`footprint_unreadable`) and set a latch; while
    /// the latch is set, further failures (on any generation) recycle nothing
    /// — the sampler, not the sidecar, is then the likely fault, and the age
    /// and call bounds still apply. One successful read clears the latch.
    /// Nothing is judged while a back-off hold runs, except the hard limit.
    ///
    /// #10207: the hard limit is checked first, on every tick of every
    /// generation: during a back-off hold, after failed replacement spawns,
    /// and on a generation's first tick. One footprint read serves both
    /// limits.
    /// Test: `hard_limit_fires_during_a_backoff_hold`,
    /// `hard_limit_kills_a_sidecar_whose_replacement_cannot_start`.
    pub(crate) fn due(
        &mut self,
        cfg: &RecycleConfig,
        hooks: &RecycleHooks,
        pid: u32,
        tracked: &TrackedClient,
    ) -> Option<RecycleTrigger> {
        let hard = cfg.effective_hard_footprint_mb();
        let read = (pid != 0 && (cfg.footprint_ceiling_mb > 0 || hard.is_some()))
            .then(|| (hooks.sampler)(pid));
        if let (Some(limit), Some(Some(mb))) = (hard, read)
            && mb > limit
        {
            tracing::warn!(
                pid,
                footprint_mb = mb,
                hard_limit_mb = limit,
                "EmbedderSupervisor: sidecar footprint over the hard limit — killing it"
            );
            return Some(RecycleTrigger::HardFootprint);
        }
        if self
            .hold_until
            .is_some_and(|t| tokio::time::Instant::now() < t)
        {
            return None;
        }
        // Never judge a generation before its second check.
        self.ticks = self.ticks.saturating_add(1);
        if self.ticks < 2 {
            return None;
        }
        let trigger = self.judge(cfg, hooks, pid, read, tracked);
        // #9828: due on the first judged check feeds the back-off.
        self.first_look = trigger.is_some() && self.ticks == 2;
        trigger
    }

    /// `read` is the footprint read `due` took: `None` = not read.
    fn judge(
        &mut self,
        cfg: &RecycleConfig,
        hooks: &RecycleHooks,
        pid: u32,
        read: Option<Option<u64>>,
        tracked: &TrackedClient,
    ) -> Option<RecycleTrigger> {
        let age = (hooks.clock)().saturating_sub(self.born);
        if cfg.footprint_ceiling_mb > 0
            && let Some(read) = read
        {
            match read {
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

/// Back-off hold for step `level` (>= 1): `base * 2^level`, capped at 1 h.
/// Test: `backoff_delay_doubles_and_caps`.
pub(crate) fn backoff_delay(base: Duration, level: u32) -> Duration {
    base.saturating_mul(1u32 << level.min(16)).min(MAX_BACKOFF)
}
