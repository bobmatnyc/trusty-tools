//! When the service's next cycle is due: debounce, least event gap, tick.
//!
//! Why: busy panes change every second, so a debounce alone ran a cycle about
//! every 2 s (#9779 review). [`Pacer`] holds an event cycle until both the
//! debounce has passed and `min_gap` has passed since the last cycle; the
//! tick is unaffected. Every time is passed in, so a test drives it with a
//! fake clock.
//! What: [`Pacer::event`] opens the debounce window (later events join it),
//! [`Pacer::due`] names the trigger due at `now`, [`Pacer::ran`] records a
//! cycle and [`Pacer::wake`] is the next instant worth checking.
//! Test: `event_cycles_keep_the_least_gap`, `tick_runs_with_no_events`
//! (tests/serve_pace.rs).

use std::time::{Duration, Instant};

use super::Trigger;

/// The loop's cycle schedule; see the module docs.
#[derive(Debug, Clone)]
pub struct Pacer {
    debounce: Duration,
    tick: Duration,
    min_gap: Duration,
    pending: Option<Instant>,
    last_cycle: Instant,
    next_tick: Instant,
}

impl Pacer {
    /// A schedule whose last cycle ran at `now`.
    pub fn new(debounce: Duration, tick: Duration, min_gap: Duration, now: Instant) -> Self {
        Self {
            debounce,
            tick,
            min_gap,
            pending: None,
            last_cycle: now,
            next_tick: now + tick,
        }
    }

    /// A source changed at `now`.
    pub fn event(&mut self, now: Instant) {
        self.pending.get_or_insert(now + self.debounce);
    }

    /// True while an event waits for its cycle.
    pub fn pending(&self) -> bool {
        self.pending.is_some()
    }

    /// The earliest instant a pending event may start its cycle.
    fn event_at(&self) -> Option<Instant> {
        // #9779 review: the least gap between cycles bounds a busy fleet.
        self.pending
            .map(|at| at.max(self.last_cycle + self.min_gap))
    }

    /// The cycle due at `now`, if any.
    pub fn due(&self, now: Instant) -> Option<Trigger> {
        if self.event_at().is_some_and(|at| now >= at) {
            Some(Trigger::Events)
        } else if now >= self.next_tick {
            Some(Trigger::Tick)
        } else {
            None
        }
    }

    /// A cycle finished at `now`: it covered every pending event.
    pub fn ran(&mut self, now: Instant) {
        self.pending = None;
        self.last_cycle = now;
        self.next_tick = now + self.tick;
    }

    /// The next instant a cycle can become due.
    pub fn wake(&self) -> Instant {
        self.event_at()
            .map_or(self.next_tick, |at| at.min(self.next_tick))
    }
}
