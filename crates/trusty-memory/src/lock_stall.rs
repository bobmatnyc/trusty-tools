//! Holder-agnostic stall detector for each open palace's handle locks (#4001).
//!
//! Why: on 2026-09-13 a dream cycle held `PalaceHandle::write_mutex`, and
//! `memory.health` stayed `ok`. [`crate::worker_liveness`] sees only operations
//! that register with it. None of the inner lock's acquirers in `trusty-common`
//! registers: remember, forget, orphan compaction, the dream cycle's compact,
//! rebuild and KG passes, and share import. Writers queued behind the holder
//! give up at their bound, which sits below the wedge threshold, so no tracked
//! age ever crossed it.
//! What: a sweep `try_lock`s `write_mutex` and `commit_mutex` on every open
//! palace. A lock found held with no probe outstanding gets a stamp and a probe
//! task. The probe queues on the lock like a writer but never gives up, and
//! only its own acquisition clears the stamp. The stamp's age is therefore how
//! long a writer arriving at the stamp has waited, whoever holds the lock.
//! Each stamp also carries the identity of the mutex it was taken against, so
//! a stamp left by a handle the registry has since replaced cannot be read as
//! a stall of the live handle.
//!
//! A probe is a real writer, not a passive observer: it queues on the lock with
//! `lock().await`, and tokio assigns the permit at the holder's release. The
//! probe therefore owns the lock from that release until its task is next
//! polled, and any writer that arrives inside that window queues behind it. The
//! window is one scheduler hop, and it buys the guarantee the detector exists
//! for — the stamp clears only when a queued writer would really have been let
//! through. A `try_lock` poll loop would avoid the window at the cost of that
//! guarantee, since `try_lock` can succeed on a lock a queued writer is still
//! waiting for.
//! Test: `lock_stall_tests.rs`,
//! `tools::tests::write_liveness_tests::a_dream_cycle_holding_the_handle_write_mutex_reads_as_wedged`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::{Duration, Instant};

use trusty_common::memory_core::PalaceRegistry;

/// A ticker heartbeat older than this many intervals reads as stopped.
const TICKER_GRACE_INTERVALS: u32 = 3;

/// Which handle-level lock a stamp describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PalaceLock {
    /// `PalaceHandle::write_mutex`: the per-palace write critical section.
    Write,
    /// `PalaceHandle::commit_mutex`: the durable-commit tail (#6366), which can
    /// outlive a write that gave up.
    Commit,
}

/// One held lock, as first sighted.
#[derive(Debug, Clone)]
struct Stall {
    /// When a sweep first found the lock held with no probe outstanding.
    since: Instant,
    /// True while a probe task is queued on the lock.
    probing: bool,
    /// The mutex this stamp was taken against (#4001).
    ///
    /// Why: the key is `(palace id, lock)`, which a reopened palace reuses. A
    /// probe still queued on a superseded handle's mutex would otherwise keep
    /// the stamp alive against a healthy live handle, and would clear a live
    /// handle's stamp when its own lock finally freed.
    /// What: a `Weak` rather than a raw address — it keeps the allocation from
    /// being reused, so pointer identity cannot alias a later mutex.
    mutex: Weak<tokio::sync::Mutex<()>>,
}

/// Whether `stamped` was taken against `mutex`.
fn stamped_against(
    stamped: &Weak<tokio::sync::Mutex<()>>,
    mutex: &Arc<tokio::sync::Mutex<()>>,
) -> bool {
    std::ptr::eq(stamped.as_ptr(), Arc::as_ptr(mutex))
}

/// The longest-standing stall, as reported to `memory.health`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StalledLock {
    /// Palace id whose lock is held.
    pub palace: String,
    /// Which of the palace's locks.
    pub lock: PalaceLock,
    /// How long a writer arriving at the first sighting has waited.
    pub age: Duration,
}

type StallKey = (String, PalaceLock);

/// Stamps for handle locks found held, plus the ticker's heartbeat.
///
/// Why: see the module docs. A `std` mutex guards the table because every
/// critical section is a map operation with no `.await`.
/// What: the stamp table, a poisoned flag, the health-path rate limit, and the
/// ticker heartbeat. A poisoned lock is recovered with `into_inner` so stamps
/// stay readable, and [`Self::degraded_at`] reports it from then on.
/// Test: `lock_stall_tests.rs`.
#[derive(Debug, Default)]
pub struct LockStallTracker {
    stalls: Mutex<HashMap<StallKey, Stall>>,
    poisoned: AtomicBool,
    last_sweep: Mutex<Option<Instant>>,
    /// `(last beat, interval)` once a ticker has started.
    ticker: Mutex<Option<(Instant, Duration)>>,
}

/// How often to sweep for a given wedge threshold.
///
/// Why: a stall is first stamped up to one interval after the holder took the
/// lock, so detection lags by at most this much. A quarter of the threshold
/// keeps that lag small next to the threshold itself.
/// What: `threshold / 4`, clamped to 10 ms ..= 30 s.
/// Test: `probe_interval_is_a_quarter_of_the_threshold_within_bounds`.
pub fn probe_interval(threshold: Duration) -> Duration {
    (threshold / 4).clamp(Duration::from_millis(10), Duration::from_secs(30))
}

impl LockStallTracker {
    /// Lock one of the tracker's own mutexes, surviving poison.
    ///
    /// Why: a panic inside a critical section must not erase the stamps. That
    /// would forget a lock that is still held.
    /// What: returns the guard; on poison sets the flag and recovers the data.
    /// Test: `a_poisoned_tracker_keeps_its_stamps_and_reports_degraded`.
    fn guard<'a, T>(&self, m: &'a Mutex<T>) -> MutexGuard<'a, T> {
        m.lock().unwrap_or_else(|poisoned| {
            self.poisoned.store(true, Ordering::Release);
            poisoned.into_inner()
        })
    }

    /// Sweep unless a sweep ran within `interval`.
    ///
    /// Why: `memory.health` is polled about once a second. Sweeping from it
    /// keeps stamps fresh even where no ticker runs, and the rate limit keeps
    /// the cheap path cheap. The ticker sweeps through here too, so the two
    /// paths share one rate limit instead of doubling each other's work.
    /// What: records the sweep time under the rate-limit lock, then sweeps.
    /// Test: `health_sweeps_are_rate_limited_to_the_interval`,
    /// `a_ticker_sweep_records_itself_against_the_health_rate_limit`.
    pub fn sweep_if_due(self: &Arc<Self>, registry: &PalaceRegistry, interval: Duration) {
        let now = Instant::now();
        {
            let mut last = self.guard(&self.last_sweep);
            if last.is_some_and(|t| now.saturating_duration_since(t) < interval) {
                return;
            }
            *last = Some(now);
        }
        self.sweep_at(registry, now);
    }

    /// Observe both handle locks of every open palace at `now`.
    ///
    /// What: `peek`s each handle so the LRU order is untouched, then
    /// [`Self::observe`]s `write_mutex` and `commit_mutex`. Probes keep only the
    /// mutex `Arc`, never the handle, so idle eviction is unaffected.
    /// Test: `a_sweep_stamps_both_handle_locks_of_an_open_palace`.
    pub(crate) fn sweep_at(self: &Arc<Self>, registry: &PalaceRegistry, now: Instant) {
        for id in registry.list() {
            let Some(handle) = registry.peek(&id) else {
                continue;
            };
            self.observe(id.as_str(), PalaceLock::Write, &handle.write_mutex, now);
            self.observe(id.as_str(), PalaceLock::Commit, &handle.commit_mutex, now);
        }
    }

    /// Observe one lock: clear an abandoned stamp if it is free, otherwise
    /// ensure a stamp and a live probe exist.
    ///
    /// What: a free lock removes only a stamp with no live probe, or one taken
    /// against a mutex this sighting has superseded — that probe can never
    /// report on the live handle. A live probe's stamp on this same mutex is
    /// left for the probe, which may not yet have queued. A held lock gets a
    /// stamp (an existing one for this same mutex keeps its older `since`) and
    /// a spawned probe unless one is already queued. Without a Tokio runtime
    /// the stamp is kept unprobed, and the next sweep retries.
    /// Test: `a_free_sighting_never_clears_a_live_probe_stamp`,
    /// `an_abandoned_probe_keeps_the_stamp_until_the_lock_is_seen_free`,
    /// `a_reopened_palace_clears_a_stamp_left_by_a_superseded_handle`.
    pub(crate) fn observe(
        self: &Arc<Self>,
        palace: &str,
        lock: PalaceLock,
        mutex: &Arc<tokio::sync::Mutex<()>>,
        now: Instant,
    ) {
        let key = (palace.to_string(), lock);
        if let Ok(free) = mutex.try_lock() {
            drop(free);
            let mut stalls = self.guard(&self.stalls);
            if stalls
                .get(&key)
                .is_some_and(|s| !s.probing || !stamped_against(&s.mutex, mutex))
            {
                stalls.remove(&key);
            }
            return;
        }
        let Some(token) = self.claim(key, now, mutex) else {
            return;
        };
        match tokio::runtime::Handle::try_current() {
            Ok(rt) => {
                rt.spawn(token.wait(Arc::clone(mutex)));
            }
            // Dropping the token marks the stamp unprobed; it is not removed.
            Err(_) => drop(token),
        }
    }

    /// Stamp `key` as probing against `mutex`, returning the probe's token, or
    /// `None` when a probe on that same mutex is already queued.
    ///
    /// What: a stamp taken against a different mutex is replaced outright — the
    /// palace was reopened, so the older `since` measures a wait on a lock no
    /// writer can queue on any more.
    /// Test: `a_reopened_palace_restamps_instead_of_inheriting_the_old_age`.
    pub(crate) fn claim(
        self: &Arc<Self>,
        key: StallKey,
        now: Instant,
        mutex: &Arc<tokio::sync::Mutex<()>>,
    ) -> Option<ProbeToken> {
        let mut stalls = self.guard(&self.stalls);
        match stalls.get_mut(&key) {
            Some(s) if stamped_against(&s.mutex, mutex) => {
                if s.probing {
                    return None;
                }
                s.probing = true;
            }
            _ => {
                stalls.insert(
                    key.clone(),
                    Stall {
                        since: now,
                        probing: true,
                        mutex: Arc::downgrade(mutex),
                    },
                );
            }
        }
        Some(ProbeToken {
            tracker: Arc::clone(self),
            key,
            mutex: Arc::downgrade(mutex),
            acquired: false,
        })
    }

    /// The oldest stamp's age at `now`, or `None` when nothing is stamped.
    ///
    /// Why: an injected `now` lets tests age a stamp without sleeping.
    /// Test: `a_lock_held_past_the_threshold_ages_past_it_and_clears_on_release`.
    pub fn oldest_stall_at(&self, now: Instant) -> Option<StalledLock> {
        let stalls = self.guard(&self.stalls);
        stalls
            .iter()
            .min_by_key(|(_, s)| s.since)
            .map(|((palace, lock), s)| StalledLock {
                palace: palace.clone(),
                lock: *lock,
                age: now.saturating_duration_since(s.since),
            })
    }

    /// Record a ticker heartbeat at `now` for a ticker running every `interval`.
    pub(crate) fn beat(&self, now: Instant, interval: Duration) {
        *self.guard(&self.ticker) = Some((now, interval));
    }

    /// Why the tracker cannot vouch for its stamps at `now`, if it cannot.
    ///
    /// Why: a stopped ticker or poisoned state leaves stall detection partial.
    /// Reporting nothing would make health read `ok` on a signal that is gone.
    /// What: `Some(reason)` when a started ticker has missed
    /// three beats (`TICKER_GRACE_INTERVALS`), or when tracking state was
    /// poisoned. The stamp table is touched first: the poison flag is set by
    /// [`Self::guard`], so a caller that has not read the stamps yet would
    /// otherwise see a stale `false` and report healthy.
    /// Test: `a_ticker_that_stops_beating_reports_degraded`,
    /// `a_poisoned_tracker_keeps_its_stamps_and_reports_degraded`,
    /// `degraded_at_sees_poison_without_a_prior_stamp_read`.
    pub fn degraded_at(&self, now: Instant) -> Option<String> {
        drop(self.guard(&self.stalls));
        let beat = *self.guard(&self.ticker);
        if self.poisoned.load(Ordering::Acquire) {
            return Some(
                "palace lock stall tracking was poisoned by a panic; stall ages may be \
                 incomplete (#4001)"
                    .to_string(),
            );
        }
        let (last, interval) = beat?;
        let silent = now.saturating_duration_since(last);
        (silent > interval * TICKER_GRACE_INTERVALS).then(|| {
            format!(
                "palace lock stall ticker has not run for {}s (interval {}ms); a held \
                 lock may go unnoticed between health polls (#4001)",
                silent.as_secs(),
                interval.as_millis()
            )
        })
    }
}

/// A probe's claim on one stamp.
///
/// Why: a probe dropped before it acquires has not seen the lock free, so its
/// stamp must survive. Removing it would forget a lock that may still be held.
/// What: [`Self::wait`] queues on the lock and removes the stamp once
/// acquired. Dropping the token without acquiring only marks the stamp
/// unprobed, so the next sweep re-probes it and keeps the original `since`.
/// Both touch the stamp only while it is still the one this token was issued
/// for: a stamp restamped against a reopened palace's mutex belongs to a
/// different lock, and clearing it here would erase a live stall.
/// Test: `an_abandoned_probe_keeps_the_stamp_until_the_lock_is_seen_free`,
/// `a_reopened_palace_restamps_instead_of_inheriting_the_old_age`.
#[derive(Debug)]
pub(crate) struct ProbeToken {
    tracker: Arc<LockStallTracker>,
    key: StallKey,
    /// The mutex this token queues on; see [`Stall::mutex`].
    mutex: Weak<tokio::sync::Mutex<()>>,
    acquired: bool,
}

impl ProbeToken {
    /// Queue on `mutex` until it is acquired, then clear this token's stamp.
    ///
    /// The acquisition is a real one — see the module docs on the ownership
    /// window it opens between the holder's release and this task's next poll.
    pub(crate) async fn wait(mut self, mutex: Arc<tokio::sync::Mutex<()>>) {
        drop(mutex.lock().await);
        let mut stalls = self.tracker.guard(&self.tracker.stalls);
        if stalls
            .get(&self.key)
            .is_some_and(|s| Weak::ptr_eq(&s.mutex, &self.mutex))
        {
            stalls.remove(&self.key);
        }
        drop(stalls);
        self.acquired = true;
    }
}

impl Drop for ProbeToken {
    fn drop(&mut self) {
        if self.acquired {
            return;
        }
        let mut stalls = self.tracker.guard(&self.tracker.stalls);
        if let Some(s) = stalls
            .get_mut(&self.key)
            .filter(|s| Weak::ptr_eq(&s.mutex, &self.mutex))
        {
            s.probing = false;
        }
    }
}

/// Spawn the background sweep for the daemon.
///
/// Why: `tm doctor` reads health once. Without a ticker the first sweep would
/// happen on that read, and a held lock would show age zero.
/// What: beats, sleeps `interval`, beats, sweeps, forever. The sweep goes
/// through the same rate limit the health path uses, so a ticker round records
/// its work and a health poll arriving just after it does not sweep again. A
/// panicking sweep ends the task, and the stale heartbeat then reads as
/// degraded.
/// Test: `the_ticker_stamps_a_held_lock_on_an_open_palace`,
/// `a_ticker_sweep_records_itself_against_the_health_rate_limit`.
pub fn spawn_lock_stall_ticker(
    tracker: Arc<LockStallTracker>,
    registry: Arc<PalaceRegistry>,
    interval: Duration,
) -> tokio::task::JoinHandle<()> {
    tracker.beat(Instant::now(), interval);
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(interval).await;
            tracker.beat(Instant::now(), interval);
            tracker.sweep_if_due(&registry, interval);
        }
    })
}

#[cfg(test)]
#[path = "lock_stall_tests.rs"]
mod tests;
