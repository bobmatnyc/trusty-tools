//! The poller's clock: wall time and sleeping behind one seam.
//!
//! Why: the prototype reads `time.time()`, `datetime.now()` and calls
//! `time.sleep()` inline, so its tests patch module globals. The port takes a
//! [`Clock`] instead, so a test fixes the time and sees every sleep without
//! waiting (#9171 A3b-1, the answers' "Gaps" line).
//! What: [`Clock::now_unix`] (seconds since the epoch, as Python's
//! `time.time()` float) and [`Clock::sleep`]. [`SystemClock`] is the real one;
//! [`FakeClock`] records each sleep, advances its own time by it, and never
//! blocks.
//! Test: `fake_clock_records_sleeps_and_advances`,
//! `system_clock_reads_the_epoch`.

use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Wall time and sleeping.
pub trait Clock {
    /// Seconds since the Unix epoch, fractional.
    fn now_unix(&self) -> f64;
    /// Waits `duration` (a fake records it instead).
    fn sleep(&self, duration: Duration);
}

/// The real clock: `SystemTime` and `std::thread::sleep`.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_unix(&self) -> f64 {
        // A clock set before 1970 reads as the epoch; nothing here compares
        // times across that edge.
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0.0, |d| d.as_secs_f64())
    }

    fn sleep(&self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

/// A clock for tests: fixed time that moves only by recorded sleeps.
#[derive(Debug, Default)]
pub struct FakeClock {
    inner: Mutex<(f64, Vec<Duration>)>,
}

impl FakeClock {
    /// A clock that reads `now` until something sleeps.
    pub fn new(now: f64) -> Self {
        Self {
            inner: Mutex::new((now, Vec::new())),
        }
    }

    /// Every duration passed to [`Clock::sleep`], oldest first.
    pub fn sleeps(&self) -> Vec<Duration> {
        self.lock().1.clone()
    }

    // A push and an add are the only mutations, so a poisoned guard still
    // holds a whole value.
    fn lock(&self) -> MutexGuard<'_, (f64, Vec<Duration>)> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Clock for FakeClock {
    fn now_unix(&self) -> f64 {
        self.lock().0
    }

    fn sleep(&self, duration: Duration) {
        let mut inner = self.lock();
        inner.0 += duration.as_secs_f64();
        inner.1.push(duration);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fake_clock_records_sleeps_and_advances() {
        let clock = FakeClock::new(100.0);
        assert_eq!(clock.now_unix(), 100.0);
        clock.sleep(Duration::from_millis(500));
        clock.sleep(Duration::from_secs(1));
        assert_eq!(clock.now_unix(), 101.5);
        assert_eq!(
            clock.sleeps(),
            [Duration::from_millis(500), Duration::from_secs(1)]
        );
    }

    #[test]
    fn system_clock_reads_the_epoch() {
        // 2025-01-01T00:00:00Z: any real clock is past it.
        assert!(SystemClock.now_unix() > 1_735_689_600.0);
    }
}
