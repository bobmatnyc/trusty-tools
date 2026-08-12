//! Shutdown-latency and shutdown-completeness tests for the reindex RSS pollers.
//!
//! Why: #5047 — `stop_pollers` cost 584–952ms of pure tick-wait on every reindex
//! teardown, because a poller parked in `Interval::tick()` could not observe its
//! stop flag until the tick expired, and the sidecar poller was not signalled
//! until the daemon poller had already joined. These tests pin both halves of the
//! fix: shutdown is prompt, and it still joins both tasks having done their work.
//! What: drives the real `spawn_memory_poller` / `spawn_embedderd_rss_poller`
//! tasks at their production 1s / 500ms cadences and times `stop_pollers`.
//! Against the pre-fix code the latency assertion fails at ~1400ms.
//! Test: this file — run via `cargo test -p trusty-search`.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::finish_teardown::stop_pollers;
use super::pollers::{spawn_embedderd_rss_poller, spawn_memory_poller};

/// Ceiling for a prompt shutdown.
///
/// Well under the 500ms sidecar tick (the shorter of the two production
/// cadences), so a regression back to waiting out a tick cannot pass by luck on
/// a slow machine — the pre-fix path takes ~1400ms from this fixture.
const PROMPT_SHUTDOWN_CEILING: Duration = Duration::from_millis(250);

/// Long enough for both pollers to take a sample and park mid-tick.
const SETTLE: Duration = Duration::from_millis(100);

struct Fixture {
    memory: (tokio::task::JoinHandle<()>, Arc<super::pollers::PollerStop>),
    sidecar: (tokio::task::JoinHandle<()>, Arc<super::pollers::PollerStop>),
    mem_abort: Arc<AtomicBool>,
    peak_rss: Arc<AtomicU64>,
    peak_sidecar_rss: Arc<AtomicU64>,
}

/// Spawn both pollers exactly as `runner::run_reindex` does.
///
/// The sidecar poller is pointed at this test process's own PID so it samples a
/// real, always-readable RSS rather than being skipped as a dead sidecar.
fn spawn_both(mem_limit: Option<u64>) -> Fixture {
    let mem_abort = Arc::new(AtomicBool::new(false));
    let peak_rss = Arc::new(AtomicU64::new(0));
    let peak_sidecar_rss = Arc::new(AtomicU64::new(0));
    let memory = spawn_memory_poller(
        mem_limit,
        Arc::clone(&mem_abort),
        Arc::clone(&peak_rss),
        "pollers-tests".to_string(),
    );
    let pid_slot = Arc::new(AtomicU32::new(std::process::id()));
    let sidecar = spawn_embedderd_rss_poller(pid_slot, Arc::clone(&peak_sidecar_rss));
    Fixture {
        memory,
        sidecar,
        mem_abort,
        peak_rss,
        peak_sidecar_rss,
    }
}

/// Poll `cond` until it holds or `budget` expires; returns whether it held.
///
/// Condition-based rather than a fixed sleep so the tests do not encode a guess
/// about how fast the first poll tick lands on a loaded machine.
async fn wait_for(budget: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + budget;
    loop {
        if cond() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// The fix itself: teardown must not wait out the current poll tick.
///
/// Pre-fix this measures ~1400ms (900ms left of the daemon poller's 1s tick,
/// then a further 500ms because the sidecar poller was only signalled after the
/// first join returned).
#[tokio::test]
async fn stop_pollers_returns_without_waiting_out_the_tick() {
    let fx = spawn_both(None);
    tokio::time::sleep(SETTLE).await;

    let started = Instant::now();
    stop_pollers(
        fx.memory.1,
        fx.memory.0,
        Some(fx.sidecar.1),
        Some(fx.sidecar.0),
    )
    .await;
    let elapsed = started.elapsed();

    println!("stop_pollers teardown = {}us", elapsed.as_micros());
    assert!(
        elapsed < PROMPT_SHUTDOWN_CEILING,
        "stop_pollers took {}ms; it must not wait out a poll tick (ceiling {}ms)",
        elapsed.as_millis(),
        PROMPT_SHUTDOWN_CEILING.as_millis(),
    );
}

/// Prompt is not enough — teardown must still have joined both tasks, and both
/// must have done the sampling they exist to do.
///
/// A shutdown that returned faster by abandoning the poller tasks, or by exiting
/// them before their first sample, would pass the latency test above and be a
/// regression. `stop_pollers` returning at all proves both `JoinHandle`s
/// resolved; the peak assertions prove neither task was cut short of its work.
#[tokio::test]
async fn stop_pollers_still_joins_both_pollers() {
    let fx = spawn_both(None);

    assert!(
        wait_for(Duration::from_secs(5), || {
            fx.peak_rss.load(Ordering::Acquire) > 0
                && fx.peak_sidecar_rss.load(Ordering::Acquire) > 0
        })
        .await,
        "both pollers must record a peak RSS before teardown",
    );
    let peak_before = fx.peak_rss.load(Ordering::Acquire);
    let sidecar_peak_before = fx.peak_sidecar_rss.load(Ordering::Acquire);

    stop_pollers(
        fx.memory.1,
        fx.memory.0,
        Some(fx.sidecar.1),
        Some(fx.sidecar.0),
    )
    .await;

    // Peaks are monotonic and must survive teardown — the terminal `complete`
    // SSE event reports them after `stop_pollers` returns.
    assert!(fx.peak_rss.load(Ordering::Acquire) >= peak_before);
    assert!(fx.peak_sidecar_rss.load(Ordering::Acquire) >= sidecar_peak_before);
}

/// The memory poller's reason for existing still works: it trips `mem_abort`
/// when RSS crosses the limit, and teardown after that is still prompt.
#[tokio::test]
async fn memory_poller_still_trips_abort_then_stops_promptly() {
    // 1 MB — this test process is always over it, so the first sample trips.
    let fx = spawn_both(Some(1));

    assert!(
        wait_for(Duration::from_secs(5), || fx
            .mem_abort
            .load(Ordering::Acquire))
        .await,
        "memory poller must trip mem_abort once RSS exceeds the limit",
    );

    let started = Instant::now();
    stop_pollers(
        fx.memory.1,
        fx.memory.0,
        Some(fx.sidecar.1),
        Some(fx.sidecar.0),
    )
    .await;
    assert!(started.elapsed() < PROMPT_SHUTDOWN_CEILING);
    assert!(
        fx.mem_abort.load(Ordering::Acquire),
        "abort flag must persist"
    );
}

/// A stop signalled before the poller parks must not be lost.
///
/// `Notify::notify_one` stores a permit when nothing is waiting, so the poller's
/// first `sleep_until_next_sample` consumes it instead of sleeping. Without that
/// property the wakeup would race the spawn and this would hang for a full tick.
#[tokio::test]
async fn stop_signalled_before_the_poller_parks_still_stops_it() {
    let fx = spawn_both(None);

    let started = Instant::now();
    stop_pollers(
        fx.memory.1,
        fx.memory.0,
        Some(fx.sidecar.1),
        Some(fx.sidecar.0),
    )
    .await;
    assert!(
        started.elapsed() < PROMPT_SHUTDOWN_CEILING,
        "teardown raced against spawn took {}ms",
        started.elapsed().as_millis(),
    );
}

/// The sidecar poller is absent whenever no embedderd PID slot exists; teardown
/// must handle the `None` arms and stay prompt.
#[tokio::test]
async fn stop_pollers_handles_absent_sidecar_poller() {
    let fx = spawn_both(None);
    fx.sidecar.1.signal();
    let _ = fx.sidecar.0.await;
    tokio::time::sleep(SETTLE).await;

    let started = Instant::now();
    stop_pollers(fx.memory.1, fx.memory.0, None, None).await;
    assert!(started.elapsed() < PROMPT_SHUTDOWN_CEILING);
}
