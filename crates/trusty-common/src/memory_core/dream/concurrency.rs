//! Process-wide dream concurrency bound and per-palace first-tick stagger.
//!
//! Why (#7106): the daemon spawned one dream loop per resident palace, every
//! loop slept the same `idle_secs` from the same startup instant, and nothing
//! limited how many cycles ran at once. On a host with 61 resident palaces all
//! 61 loops woke in the same second and each held its palace's whole corpus —
//! snapshot, embed inputs, and the embedder's own copies — while queued behind
//! one ONNX mutex. Live verification measured a 43 GB transient at launch+300 s
//! and an 8 GB one 300 s later. Bounding the steady state (#7115) did not touch
//! that peak, because the peak is `per-cycle working set x concurrency`.
//! What: a process-wide `tokio::sync::Semaphore` sized from
//! [`DREAM_MAX_CONCURRENT_ENV`] (default [`DEFAULT_DREAM_MAX_CONCURRENT`]) that
//! every dream cycle acquires before it runs and releases on drop — the idle
//! loops wait indefinitely, an interactive caller waits a bounded time and gets
//! [`DreamBusy`] instead of a silent hang; an
//! independent in-flight gauge so an operator (and the regression test) can see
//! how many cycles are actually running; and [`stagger_offset`], the
//! deterministic phase spread that keeps a cold start from firing every palace
//! in the same second.
//! Test: `concurrency_tests::dream_permits_cap_concurrent_holders`,
//! `concurrency_tests::ten_palaces_never_exceed_the_concurrency_cap`,
//! `concurrency_tests::stagger_offsets_spread_first_ticks_across_the_interval`.

use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// Environment variable that caps how many dream cycles run at once (#7106).
///
/// Why: the right number depends on the host — a 128 GB workstation with two
/// palaces can afford more than a 16 GB laptop with sixty. An env var keeps
/// that tunable without a recompile, matching `TRUSTY_MEMORY_REDB_CACHE_MB`
/// and `TRUSTY_MEMORY_STARTUP_OPEN_LIMIT`.
/// What: read once, at the first dream cycle or scheduler start, by
/// [`dream_max_concurrent`]. Unset, empty, `0`, or unparsable keeps the
/// default and logs a `warn` naming the rejected value.
/// Test: `parse_max_concurrent_rejects_junk_and_zero`.
pub const DREAM_MAX_CONCURRENT_ENV: &str = "TRUSTY_DREAM_MAX_CONCURRENT";

/// Dream cycles allowed to run concurrently when nothing overrides it.
///
/// Why: two keeps one slow palace from stalling every other palace's
/// consolidation while holding the transient peak to twice one cycle's working
/// set rather than sixty-one times it.
pub const DEFAULT_DREAM_MAX_CONCURRENT: usize = 2;

/// The semaphore and its permit count, resolved once per process.
static DREAM_LIMIT: OnceLock<(Arc<Semaphore>, usize)> = OnceLock::new();

/// Dream cycles currently holding a permit and running.
static IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);

/// High-water mark of [`IN_FLIGHT`] over this process's lifetime.
static PEAK_IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);

/// Interpret the raw [`DREAM_MAX_CONCURRENT_ENV`] value.
///
/// Why: the parse is the whole policy — which spellings mean "use the default"
/// and which are honoured — so it is a pure function the tests can pin without
/// mutating the process environment.
/// What: a positive decimal integer is honoured verbatim. `None`, empty, `0`,
/// and anything unparsable answer [`DEFAULT_DREAM_MAX_CONCURRENT`]; the two
/// cases where the operator clearly meant something warn first. Never returns
/// `0` — an unbounded fan-out is what this module exists to prevent.
/// Test: `parse_max_concurrent_rejects_junk_and_zero`.
pub(super) fn parse_max_concurrent(raw: Option<&str>) -> usize {
    let Some(text) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return DEFAULT_DREAM_MAX_CONCURRENT;
    };
    match text.parse::<usize>() {
        Ok(n) if n > 0 => n,
        _ => {
            tracing::warn!(
                env = DREAM_MAX_CONCURRENT_ENV,
                value = text,
                default = DEFAULT_DREAM_MAX_CONCURRENT,
                "unusable dream concurrency cap; keeping the default"
            );
            DEFAULT_DREAM_MAX_CONCURRENT
        }
    }
}

/// The semaphore and permit count for this process, initialising on first use.
fn limit() -> &'static (Arc<Semaphore>, usize) {
    DREAM_LIMIT.get_or_init(|| {
        let raw = std::env::var(DREAM_MAX_CONCURRENT_ENV).ok();
        let permits = parse_max_concurrent(raw.as_deref());
        tracing::info!(
            max_concurrent = permits,
            env = DREAM_MAX_CONCURRENT_ENV,
            "dream: concurrent-cycle cap resolved"
        );
        (Arc::new(Semaphore::new(permits)), permits)
    })
}

/// How many dream cycles this process permits to run at once.
///
/// Why: the scheduler logs it at startup so an operator can tell from the log
/// alone whether an override took effect.
/// What: the resolved [`DREAM_MAX_CONCURRENT_ENV`] value, read once per
/// process. Always at least 1.
/// Test: `the_default_cap_is_two`.
pub fn dream_max_concurrent() -> usize {
    limit().1
}

/// Dream cycles currently running.
///
/// Why: an operator looking at a memory spike wants to know whether the dreamer
/// is the cause, and the doctor has no other way to see it. Reading the
/// semaphore's free permits cannot answer this — it stays full when nothing
/// acquires — so the gauge is kept independently of the permit.
/// What: relaxed load of a process-wide counter that every cycle increments on
/// entry and decrements on drop.
/// Test: `the_in_flight_gauge_counts_a_held_cycle`.
pub fn dream_cycles_in_flight() -> usize {
    IN_FLIGHT.load(Ordering::SeqCst)
}

/// The most dream cycles that have ever run at once in this process.
///
/// Why: the transient this module bounds lasts seconds, so a poll of
/// [`dream_cycles_in_flight`] almost always misses it. The high-water mark is
/// the only reading that survives long enough to be checked.
/// What: monotonic; never decreases. `0` before the first cycle.
/// Test: `the_in_flight_gauge_counts_a_held_cycle`.
pub fn dream_cycles_peak_in_flight() -> usize {
    PEAK_IN_FLIGHT.load(Ordering::SeqCst)
}

/// A held slot in the process-wide dream concurrency bound.
///
/// Why: the release must happen on every arm a cycle can leave by — an early
/// `?`, a `bail!`, a panic — and the only way to get that for free is to tie it
/// to a value's lifetime. A permit returned by value and dropped by the
/// compiler cannot leak the way a paired acquire/release call can.
/// What: wraps an [`OwnedSemaphorePermit`]; dropping it returns the permit.
/// Test: `dream_permits_cap_concurrent_holders`.
#[derive(Debug)]
pub struct DreamPermit {
    _permit: OwnedSemaphorePermit,
}

/// Wait for a slot in the process-wide dream concurrency bound.
///
/// Why (#7106): every path that runs a dream cycle — the idle scheduler loop
/// and the on-demand `palace_dream` / `dream_consolidate_room` MCP tools — must
/// queue behind the same bound, or the bound is only advisory.
/// What: acquires one permit from the shared semaphore, waiting when all are
/// held. The returned [`DreamPermit`] releases on drop.
/// Test: `dream_permits_cap_concurrent_holders`,
/// `a_dream_cycle_waits_when_every_permit_is_held`.
pub async fn acquire_dream_permit() -> DreamPermit {
    let permit = dream_semaphore()
        .acquire_owned()
        .await
        // The semaphore is a process-wide static that nothing closes, and
        // `acquire_owned` fails only on a closed semaphore.
        .expect("dream semaphore is never closed");
    DreamPermit { _permit: permit }
}

/// Every dream slot was busy for the whole of an interactive caller's wait.
///
/// Why (#7106): the on-demand `palace_dream` / `dream_consolidate_room` tools
/// share the bound with the idle loops, and a semantic-consolidation call alone
/// is bounded at 120 s per palace, so a caller can sit behind minutes of work
/// it cannot see. An error naming the cap and the live count is something the
/// caller can retry on or report; an unbounded await is not.
/// What: carries the cap, the cycles in flight when the wait expired, and how
/// long the caller waited.
/// Test: `an_interactive_dream_errors_when_the_dreamer_is_busy`.
#[derive(Debug, thiserror::Error)]
#[error(
    "dreamer busy: {in_flight} dream cycle(s) in flight against a cap of {cap}; \
     no slot freed within {waited_secs}s — retry, or raise TRUSTY_DREAM_MAX_CONCURRENT"
)]
pub struct DreamBusy {
    /// Concurrent cycles this process allows.
    pub cap: usize,
    /// Cycles running when the wait expired.
    pub in_flight: usize,
    /// How long the caller waited, in seconds.
    pub waited_secs: f64,
}

/// Wait at most `wait` for a slot in the process-wide dream bound.
///
/// Why (#7106): [`acquire_dream_permit`] is right for the idle loop, which
/// nothing is waiting on and which may queue indefinitely. It is wrong for a
/// user-invoked dream: before this bound existed that path had no shared gate
/// at all, so an unbounded wait here would turn a working interactive call into
/// a silent hang behind two slow idle cycles.
/// What: races the acquire against `wait`; returns [`DreamBusy`] naming the cap
/// and the live count when the wait expires. The permit, when granted, releases
/// on drop exactly as [`acquire_dream_permit`]'s does.
/// Test: `an_interactive_dream_errors_when_the_dreamer_is_busy`.
pub async fn acquire_dream_permit_within(wait: Duration) -> Result<DreamPermit, DreamBusy> {
    match tokio::time::timeout(wait, acquire_dream_permit()).await {
        Ok(permit) => Ok(permit),
        Err(_) => Err(DreamBusy {
            cap: dream_max_concurrent(),
            in_flight: dream_cycles_in_flight(),
            waited_secs: wait.as_secs_f64(),
        }),
    }
}

/// The process-wide dream semaphore.
///
/// Why: exposed so a caller that needs to hold several slots, or to observe the
/// bound, does not construct a second semaphore and silently opt out of it.
/// What: a clone of the `Arc` initialised on first use from
/// [`DREAM_MAX_CONCURRENT_ENV`].
/// Test: `dream_permits_cap_concurrent_holders`.
pub fn dream_semaphore() -> Arc<Semaphore> {
    Arc::clone(&limit().0)
}

/// RAII counter for [`dream_cycles_in_flight`] / [`dream_cycles_peak_in_flight`].
///
/// Why: kept separate from [`DreamPermit`] on purpose. If the gauge rode on the
/// permit it would read zero the moment the permit was removed, and the
/// regression test that proves the cap holds would pass against code with no
/// cap at all.
/// What: increments on [`Self::enter`], updates the peak, decrements on drop.
/// Test: `the_in_flight_gauge_counts_a_held_cycle`.
#[derive(Debug)]
pub struct DreamCycleGauge {
    _private: (),
}

impl DreamCycleGauge {
    /// Record that one more dream cycle is running.
    pub fn enter() -> Self {
        let now = IN_FLIGHT.fetch_add(1, Ordering::SeqCst) + 1;
        PEAK_IN_FLIGHT.fetch_max(now, Ordering::SeqCst);
        Self { _private: () }
    }
}

impl Drop for DreamCycleGauge {
    fn drop(&mut self) {
        IN_FLIGHT.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Extra delay before palace `index` of `total` takes its first dream tick.
///
/// Why (#7106): every loop slept the same `idle_secs` from the same startup
/// instant, so `idle_secs` after launch all of them woke together. Spreading
/// the first tick spreads every later tick too, because each loop keeps ticking
/// at the same `idle_secs` cadence from wherever its first tick landed.
/// What: `interval * index / total` — palace `k` of `n` waits
/// `interval + interval * k / n` before its first cycle and `interval` between
/// every cycle after that. Deterministic (no RNG, so a restart reproduces the
/// same schedule) and `Duration::ZERO` for a single palace or an out-of-range
/// index. Which palace gets which slot is the caller's `index`, and the
/// scheduler's comes from `PalaceRegistry::list()` — LRU-recency order, not a
/// stable one. That only decides who goes first; the spread is the point.
/// Test: `stagger_offsets_spread_first_ticks_across_the_interval`.
pub fn stagger_offset(index: usize, total: usize, interval: Duration) -> Duration {
    if total <= 1 || index >= total {
        return Duration::ZERO;
    }
    interval.mul_f64(index as f64 / total as f64)
}
