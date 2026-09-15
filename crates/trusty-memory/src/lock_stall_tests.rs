//! Tests for the handle-lock stall detector (issue #4001).
//!
//! Why: the detector is the only thing that sees a dream cycle, forget, or
//! import holding a palace's handle locks. Each fail-open path it guards
//! against needs its own pin.
//! What: drives the tracker with injected instants, so ageing a stamp needs no
//! sleep. Conditions that depend on a spawned probe are polled with a bound.
//! Test: this IS the test module.

use super::*;
use std::sync::Arc;
use std::time::{Duration, Instant};
use trusty_common::memory_core::palace::{Palace, PalaceId};

/// Upper bound on any single condition wait in this module.
const SETTLE: Duration = Duration::from_secs(10);

/// Poll `cond` every 5 ms until it holds, failing after [`SETTLE`].
async fn wait_for(what: &str, mut cond: impl FnMut() -> bool) {
    let started = Instant::now();
    while !cond() {
        assert!(started.elapsed() < SETTLE, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// A registry holding one open palace named `id`.
fn registry_with(
    id: &str,
) -> (
    Arc<PalaceRegistry>,
    Arc<trusty_common::memory_core::PalaceHandle>,
    tempfile::TempDir,
) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let registry = Arc::new(PalaceRegistry::new());
    let pid = PalaceId::new(id);
    let palace = Palace {
        id: pid.clone(),
        name: id.to_string(),
        description: None,
        created_at: chrono::Utc::now(),
        data_dir: tmp.path().join(pid.as_str()),
    };
    let handle = registry
        .create_palace(tmp.path(), palace)
        .expect("create palace");
    (registry, handle, tmp)
}

fn key(lock: PalaceLock) -> StallKey {
    ("p".to_string(), lock)
}

/// Why: the default 120 s threshold must sweep every 30 s, and a test
/// threshold must not spin.
/// What: checks the quarter rule and both clamps.
/// Test: itself.
#[test]
fn probe_interval_is_a_quarter_of_the_threshold_within_bounds() {
    assert_eq!(
        probe_interval(Duration::from_secs(120)),
        Duration::from_secs(30)
    );
    assert_eq!(
        probe_interval(Duration::from_secs(40)),
        Duration::from_secs(10)
    );
    assert_eq!(
        probe_interval(Duration::from_secs(3600)),
        Duration::from_secs(30)
    );
    assert_eq!(probe_interval(Duration::ZERO), Duration::from_millis(10));
}

/// Why (#4001): the core contract. A held lock ages from its first sighting,
/// reads under the threshold before it and over it after, and clears once the
/// holder lets go.
/// What: holds a mutex, observes at `t0`, reads the age at two injected
/// instants, releases, and waits for the probe to clear the stamp.
/// Test: itself.
#[tokio::test]
async fn a_lock_held_past_the_threshold_ages_past_it_and_clears_on_release() {
    let tracker = Arc::new(LockStallTracker::default());
    let mutex = Arc::new(tokio::sync::Mutex::new(()));
    let held = mutex.clone().lock_owned().await;
    let threshold = Duration::from_secs(120);
    let t0 = Instant::now();
    tracker.observe("p", PalaceLock::Write, &mutex, t0);

    let under = tracker
        .oldest_stall_at(t0 + Duration::from_secs(53))
        .expect("stamped");
    assert!(
        under.age <= threshold,
        "a 53 s hold is not past 120 s: {under:?}"
    );
    let over = tracker
        .oldest_stall_at(t0 + Duration::from_secs(121))
        .expect("stamped");
    assert!(over.age > threshold, "{over:?}");
    assert_eq!((over.palace.as_str(), over.lock), ("p", PalaceLock::Write));

    drop(held);
    wait_for("the probe to clear the stamp", || {
        tracker.oldest_stall_at(Instant::now()).is_none()
    })
    .await;
}

/// Why (fail-open row: panicking holder): a holder that panics releases its
/// tokio guard on unwind, so the lock is free and the stamp must clear rather
/// than read as a permanent wedge.
/// What: a task takes the lock, is observed holding it, then panics; asserts
/// the join is a panic, the lock is free, and the stamp clears.
/// Test: itself.
#[tokio::test]
async fn a_holder_that_panics_releases_and_clears_the_stall() {
    let tracker = Arc::new(LockStallTracker::default());
    let mutex = Arc::new(tokio::sync::Mutex::new(()));
    let (taken_tx, taken_rx) = tokio::sync::oneshot::channel::<()>();
    let (go_tx, go_rx) = tokio::sync::oneshot::channel::<()>();
    let holder_mutex = mutex.clone();
    let holder = tokio::spawn(async move {
        let _guard = holder_mutex.lock().await;
        let _ = taken_tx.send(());
        let _ = go_rx.await;
        panic!("holder panics while holding the palace lock");
    });
    taken_rx.await.expect("holder took the lock");
    tracker.observe("p", PalaceLock::Write, &mutex, Instant::now());
    assert!(tracker.oldest_stall_at(Instant::now()).is_some());

    let _ = go_tx.send(());
    assert!(holder.await.expect_err("holder panicked").is_panic());
    wait_for("the stamp to clear after the panic", || {
        tracker.oldest_stall_at(Instant::now()).is_none()
    })
    .await;
    assert!(
        mutex.try_lock().is_ok(),
        "the panicking holder released the lock"
    );
}

/// Why (fail-open row: probe dropped early): a probe dropped before it
/// acquires has not seen the lock free. Its stamp must keep its original age,
/// and only a later free sighting or probe acquisition may clear it.
/// What: claims and drops a token while the lock is held; asserts the stamp
/// survives unprobed, a re-observe keeps `since` and re-probes, and release
/// clears it.
/// Test: itself.
#[tokio::test]
async fn an_abandoned_probe_keeps_the_stamp_until_the_lock_is_seen_free() {
    let tracker = Arc::new(LockStallTracker::default());
    let mutex = Arc::new(tokio::sync::Mutex::new(()));
    let held = mutex.clone().lock_owned().await;
    let t0 = Instant::now();
    drop(
        tracker
            .claim(key(PalaceLock::Write), t0, &mutex)
            .expect("first claim"),
    );

    let later = t0 + Duration::from_secs(200);
    tracker.observe("p", PalaceLock::Write, &mutex, later);
    let stall = tracker
        .oldest_stall_at(later)
        .expect("the stamp survives the drop");
    assert_eq!(
        stall.age,
        Duration::from_secs(200),
        "`since` must stay at t0"
    );
    assert!(
        tracker
            .claim(key(PalaceLock::Write), later, &mutex)
            .is_none(),
        "the re-observe must have queued a live probe"
    );

    drop(held);
    wait_for("the re-spawned probe to clear the stamp", || {
        tracker.oldest_stall_at(Instant::now()).is_none()
    })
    .await;
}

/// Why: a probe spawned but not yet queued lets `try_lock` succeed while the
/// lock is about to be taken again. A free sighting must never erase that
/// probe's stamp; only an unprobed stamp may be cleared that way.
/// What: holds a live token, observes the free lock, asserts the stamp stays;
/// drops the token, observes again, asserts it clears.
/// Test: itself.
#[tokio::test]
async fn a_free_sighting_never_clears_a_live_probe_stamp() {
    let tracker = Arc::new(LockStallTracker::default());
    let mutex = Arc::new(tokio::sync::Mutex::new(()));
    let now = Instant::now();
    let token = tracker
        .claim(key(PalaceLock::Commit), now, &mutex)
        .expect("claim");
    tracker.observe("p", PalaceLock::Commit, &mutex, now);
    assert!(
        tracker.oldest_stall_at(now).is_some(),
        "live probe stamp kept"
    );

    drop(token);
    tracker.observe("p", PalaceLock::Commit, &mutex, now);
    assert!(
        tracker.oldest_stall_at(now).is_none(),
        "unprobed stamp cleared"
    );
}

/// Why (#4001, stamp identity): the stamp key is `(palace id, lock)`, which a
/// reopened palace reuses. A probe still queued on the evicted handle's mutex
/// leaves `probing` set forever, so a free sighting of the LIVE handle's lock
/// would keep reporting a stall against a healthy palace.
/// What: stamps a held mutex, then observes a second, free mutex under the same
/// key — the reopened handle — and asserts nothing is stamped.
/// Test: itself.
#[tokio::test]
async fn a_reopened_palace_clears_a_stamp_left_by_a_superseded_handle() {
    let tracker = Arc::new(LockStallTracker::default());
    let superseded = Arc::new(tokio::sync::Mutex::new(()));
    let _held = superseded.clone().lock_owned().await;
    let t0 = Instant::now();
    tracker.observe("p", PalaceLock::Write, &superseded, t0);
    assert!(
        tracker.oldest_stall_at(t0).is_some(),
        "the evicted handle's held lock is stamped"
    );

    // The palace is reopened: a new handle, a free lock. The probe queued on
    // the old mutex can never acquire it, so only this sighting can clear it.
    let reopened = Arc::new(tokio::sync::Mutex::new(()));
    let later = t0 + Duration::from_secs(200);
    tracker.observe("p", PalaceLock::Write, &reopened, later);
    assert_eq!(
        tracker.oldest_stall_at(later),
        None,
        "a free lock on the live handle leaves no stamp"
    );
}

/// Why (#4001, stamp identity): if the reopened handle's lock is also held, the
/// stamp must start from this sighting. Inheriting the superseded handle's
/// `since` would report a fresh hold as minutes old and wedge a healthy palace.
/// What: stamps a held mutex at `t0`, observes a second held mutex 200 s later,
/// and asserts the reported age is measured from the second sighting.
/// Test: itself.
#[tokio::test]
async fn a_reopened_palace_restamps_instead_of_inheriting_the_old_age() {
    let tracker = Arc::new(LockStallTracker::default());
    let superseded = Arc::new(tokio::sync::Mutex::new(()));
    let _old = superseded.clone().lock_owned().await;
    let t0 = Instant::now();
    tracker.observe("p", PalaceLock::Write, &superseded, t0);

    let reopened = Arc::new(tokio::sync::Mutex::new(()));
    let _new = reopened.clone().lock_owned().await;
    let later = t0 + Duration::from_secs(200);
    tracker.observe("p", PalaceLock::Write, &reopened, later);
    let stall = tracker
        .oldest_stall_at(later + Duration::from_secs(1))
        .expect("the live handle's held lock is stamped");
    assert_eq!(
        stall.age,
        Duration::from_secs(1),
        "the stamp must date from the live handle's sighting, not the old one"
    );
}

/// Why (fail-open row: tracking-state poisoning): a panic inside the stamp
/// table's critical section must not erase stamps, and must be reported.
/// What: stamps a held lock, poisons the table from a panicking thread, and
/// asserts the stamp still reads and `degraded_at` names the poisoning.
/// Test: itself.
#[tokio::test]
async fn a_poisoned_tracker_keeps_its_stamps_and_reports_degraded() {
    let tracker = Arc::new(LockStallTracker::default());
    let mutex = Arc::new(tokio::sync::Mutex::new(()));
    let _held = mutex.clone().lock_owned().await;
    let t0 = Instant::now();
    tracker.observe("p", PalaceLock::Write, &mutex, t0);
    assert!(tracker.degraded_at(t0).is_none());

    let poisoner = Arc::clone(&tracker);
    let joined = std::thread::spawn(move || {
        let _g = poisoner.stalls.lock();
        panic!("poison the stall table");
    })
    .join();
    assert!(joined.is_err(), "the poisoning thread panicked");

    let stall = tracker.oldest_stall_at(t0 + Duration::from_secs(1));
    assert!(stall.is_some(), "stamps survive poisoning");
    let reason = tracker.degraded_at(t0).expect("poisoning is reported");
    assert!(reason.contains("poisoned"), "{reason}");
}

/// Why (fail-open row: ticker stops): a dead ticker leaves the first health
/// read after a stall at age zero. That must read as degraded, not `ok`.
/// What: beats at `t0` with a 10 ms interval; asserts no degradation inside
/// three intervals and degradation past them. A tracker with no ticker never
/// reports it.
/// Test: itself.
#[test]
fn a_ticker_that_stops_beating_reports_degraded() {
    let tracker = LockStallTracker::default();
    let t0 = Instant::now();
    assert!(tracker.degraded_at(t0 + Duration::from_secs(60)).is_none());
    tracker.beat(t0, Duration::from_millis(10));
    assert!(tracker
        .degraded_at(t0 + Duration::from_millis(30))
        .is_none());
    let reason = tracker
        .degraded_at(t0 + Duration::from_millis(31))
        .expect("a silent ticker is degraded");
    assert!(reason.contains("ticker"), "{reason}");
}

/// Why: health polls once a second, and each poll must not sweep every palace
/// when the interval has not elapsed.
/// What: sweeps once, holds the lock, sweeps again inside the interval and
/// asserts no stamp was taken.
/// Test: itself.
#[tokio::test]
async fn health_sweeps_are_rate_limited_to_the_interval() {
    let (registry, handle, _tmp) = registry_with("rate");
    let tracker = Arc::new(LockStallTracker::default());
    tracker.sweep_if_due(&registry, Duration::from_secs(3600));
    let _held = handle.write_mutex.clone().lock_owned().await;
    tracker.sweep_if_due(&registry, Duration::from_secs(3600));
    assert!(tracker.oldest_stall_at(Instant::now()).is_none());
}

/// Why (#4001): every acquirer takes `write_mutex` or `commit_mutex` on the
/// handle, so a sweep must stamp both.
/// What: holds both locks of an open palace, sweeps, asserts one stamp each.
/// Test: itself.
#[tokio::test]
async fn a_sweep_stamps_both_handle_locks_of_an_open_palace() {
    let (registry, handle, _tmp) = registry_with("both");
    let tracker = Arc::new(LockStallTracker::default());
    let _w = handle.write_mutex.clone().lock_owned().await;
    let _c = handle.commit_mutex.clone().lock_owned().await;
    tracker.sweep_at(&registry, Instant::now());
    let stalls = tracker.stalls.lock().expect("not poisoned");
    assert!(stalls.contains_key(&("both".to_string(), PalaceLock::Write)));
    assert!(stalls.contains_key(&("both".to_string(), PalaceLock::Commit)));
}

/// Why: the daemon relies on the ticker, not health polls, to stamp a stall
/// before doctor's single read.
/// What: holds `commit_mutex`, spawns the ticker at 10 ms, waits for a stamp.
/// Test: itself.
#[tokio::test]
async fn the_ticker_stamps_a_held_lock_on_an_open_palace() {
    let (registry, handle, _tmp) = registry_with("ticked");
    let tracker = Arc::new(LockStallTracker::default());
    let _c = handle.commit_mutex.clone().lock_owned().await;
    let ticker = spawn_lock_stall_ticker(Arc::clone(&tracker), registry, Duration::from_millis(10));
    wait_for("the ticker to stamp the held commit lock", || {
        tracker
            .oldest_stall_at(Instant::now())
            .is_some_and(|s| s.palace == "ticked" && s.lock == PalaceLock::Commit)
    })
    .await;
    assert!(tracker.degraded_at(Instant::now()).is_none());
    ticker.abort();
}

/// Why (#4001): the ticker and the health path sweep the same palaces. A ticker
/// sweep that does not record itself leaves the next health poll sweeping again
/// immediately, doubling the work the rate limit exists to bound.
/// What: lets the ticker sweep once, stops it, then holds a second lock and
/// calls the health path with an hour-long interval; that sweep must be skipped
/// because the ticker's own sweep counts against it.
/// Test: itself.
#[tokio::test]
async fn a_ticker_sweep_records_itself_against_the_health_rate_limit() {
    let (registry, handle, _tmp) = registry_with("shared");
    let tracker = Arc::new(LockStallTracker::default());
    let _c = handle.commit_mutex.clone().lock_owned().await;
    let ticker = spawn_lock_stall_ticker(
        Arc::clone(&tracker),
        Arc::clone(&registry),
        Duration::from_millis(10),
    );
    wait_for("the ticker to sweep once", || {
        tracker.oldest_stall_at(Instant::now()).is_some()
    })
    .await;
    ticker.abort();
    assert!(
        ticker
            .await
            .expect_err("the ticker was aborted")
            .is_cancelled(),
        "the ticker must be stopped before the health sweep below"
    );

    let _w = handle.write_mutex.clone().lock_owned().await;
    tracker.sweep_if_due(&registry, Duration::from_secs(3600));
    let stalls = tracker.stalls.lock().expect("not poisoned");
    assert!(
        !stalls.contains_key(&("shared".to_string(), PalaceLock::Write)),
        "the health sweep must be rate limited by the ticker's own sweep: {stalls:?}"
    );
}
