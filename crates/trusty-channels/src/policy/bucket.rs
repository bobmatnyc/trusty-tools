//! Inbound rate limiting: a sliding-window admit log per (channel, binding
//! id), plus one log for unknown senders.
//!
//! Why: a routed sender may flood an open question, and an unknown sender
//! may flood the audit log (#7457 folded into #8454; Architect Q6). The
//! rule is "at most `limit` admits in ANY window of `window_secs`", which a
//! continuous token bucket breaks: it admits 100 at once and a 101st 0.6 s
//! later (Architect ruling 17:19Z).
//! What: [`SlidingWindow`] keeps the times of its last `limit` admits, so it
//! holds at most [`MAX_LIMIT`](crate::policy::MAX_LIMIT) timestamps. A take
//! admits only when fewer than `limit` admits fall in the window ending now.
//! [`RateLimiter`] keys windows by (channel, route name) and keeps the
//! unknown-sender window apart. State is in memory; time comes from an
//! injected [`Clock`], so tests never sleep.
//! Test: `src/policy/tests/bucket.rs`.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use crate::policy::types::{Channel, RateLimit, Route};

/// A monotonic time source: elapsed time since a fixed origin.
///
/// Why: the window needs time that never runs backwards, and tests need to
/// set it.
/// What: [`MonotonicClock`] in production; a test passes its own. A
/// [`SlidingWindow`] refuses a reading earlier than its latest one.
/// Test: `bucket_exhausted_does_not_refill_early`,
/// `backwards_clock_reading_never_readmits_early`.
pub trait Clock {
    /// Time since this clock's origin.
    fn now(&self) -> Duration;
}

/// The production [`Clock`], backed by [`Instant`].
#[derive(Debug, Clone, Copy)]
pub struct MonotonicClock {
    origin: Instant,
}

impl MonotonicClock {
    /// A clock whose origin is now.
    pub fn new() -> Self {
        Self {
            origin: Instant::now(),
        }
    }
}

impl Default for MonotonicClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for MonotonicClock {
    fn now(&self) -> Duration {
        self.origin.elapsed()
    }
}

/// The verdict of one take.
///
/// Why: exhaustion drops the message; there is no "defer" arm in v1
/// (#8454 plan §4).
/// What: `Admit` recorded an admit; `Exhausted` recorded nothing. A
/// backwards clock reading also answers `Exhausted`.
/// Test: `bucket_101st_in_a_minute_exhausted`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use]
pub enum BucketDecision {
    /// The admit was recorded; the message may proceed to the policy check.
    Admit,
    /// `limit` admits already fall in the current window; drop the message.
    Exhausted,
}

/// One sliding-window admit log.
///
/// Why: the #7457 conditions — 100 messages inside a minute pass, the 101st
/// does not — must hold for every window, not only one starting at zero.
/// What: keeps the times of the last `limit` admits, oldest first. A take
/// refuses while the admit `limit` places back is younger than the window,
/// so the first admit after a full window comes exactly `window_secs` after
/// the admit it replaces. A clock reading earlier than the latest one seen
/// is a clock fault: the take is refused, records nothing and leaves the
/// latest reading in place, so a backwards reading never admits and the
/// log stays in time order.
/// Test: `bucket_101st_in_a_minute_exhausted`,
/// `bucket_exhausted_does_not_refill_early`,
/// `backwards_clock_reading_never_readmits_early`.
#[derive(Debug, Clone)]
pub struct SlidingWindow {
    limit: RateLimit,
    admits: VecDeque<Duration>,
    last: Duration,
}

impl SlidingWindow {
    /// An empty log for `limit`, with `now` as the latest reading.
    pub fn new(limit: RateLimit, now: Duration) -> Self {
        Self {
            limit,
            admits: VecDeque::new(),
            last: now,
        }
    }

    /// The window's limit.
    pub fn limit(&self) -> RateLimit {
        self.limit
    }

    /// Record an admit at `now` unless `limit` admits already fall in the
    /// window ending at `now`, or `now` is earlier than the latest reading.
    pub fn take(&mut self, now: Duration) -> BucketDecision {
        // #8454: a reading earlier than the latest one means a broken clock;
        // fail closed rather than compute a window from it.
        if now < self.last {
            return BucketDecision::Exhausted;
        }
        self.last = now;
        let limit = self.limit.limit() as usize;
        let nth_back = self
            .admits
            .len()
            .checked_sub(limit)
            .and_then(|i| self.admits.get(i).copied());
        if let Some(oldest) = nth_back {
            if now.saturating_sub(oldest) < self.limit.window() {
                return BucketDecision::Exhausted;
            }
        }
        self.admits.push_back(now);
        while self.admits.len() > limit {
            self.admits.pop_front();
        }
        BucketDecision::Admit
    }

    /// Adopt a changed limit. The admit log is kept, so a lower limit takes
    /// effect against admits already made; nothing is refilled.
    fn retune(&mut self, limit: RateLimit) {
        self.limit = limit;
    }
}

/// The in-memory windows for one channel process.
///
/// Why: one window per binding, so one flooding route cannot spend
/// another's admits, and one separate window for unknown senders (Q6).
/// What: [`RateLimiter::take_route`] and [`RateLimiter::take_binding`] use
/// the window keyed by (channel, name), created empty on first use;
/// [`RateLimiter::take_unknown_sender`] uses the unknown-sender window.
/// Restarting the process resets every window.
/// Test: `unknown_sender_bucket_is_separate`,
/// `route_lower_limit_is_enforced_by_its_bucket`.
#[derive(Debug)]
pub struct RateLimiter<C: Clock> {
    clock: C,
    routes: HashMap<Channel, HashMap<String, SlidingWindow>>,
    unknown: SlidingWindow,
}

impl<C: Clock> RateLimiter<C> {
    /// A limiter whose unknown-sender window uses `unknown_limit`, normally
    /// [`ChannelPolicy::default_rate_limit`](crate::policy::ChannelPolicy::default_rate_limit).
    /// That limit is fixed for the limiter's life.
    pub fn new(unknown_limit: RateLimit, clock: C) -> Self {
        let now = clock.now();
        Self {
            clock,
            routes: HashMap::new(),
            unknown: SlidingWindow::new(unknown_limit, now),
        }
    }

    /// Record one admit in the window of `route`'s (channel, name).
    ///
    /// Why: runs before the inbound check for a sender with a route.
    /// What: a route whose limit changed (a rebuilt policy) switches to the
    /// new limit and keeps its admit log, so a lowered limit applies to
    /// admits already made.
    /// Test: `bucket_101st_in_a_minute_exhausted`,
    /// `rebuilt_lower_route_limit_keeps_the_log_and_refuses`.
    pub fn take_route(&mut self, route: &Route) -> BucketDecision {
        self.take_binding(route.channel(), route.name(), route.rate_limit())
    }

    /// Record one admit in the window of the binding (`channel`, `name`).
    ///
    /// Why: gchat keeps its own route table, with no policy [`Route`], and
    /// still needs the per-binding window (#8454 S3b).
    /// What: the window is created empty on first use with `limit`; a later
    /// call with a different `limit` retunes it and keeps its admit log.
    /// [`RateLimiter::take_route`] is this call with the route's fields.
    /// Test: `take_binding_matches_take_route`.
    pub fn take_binding(
        &mut self,
        channel: Channel,
        name: &str,
        limit: RateLimit,
    ) -> BucketDecision {
        let now = self.clock.now();
        let by_name = self.routes.entry(channel).or_default();
        let window = by_name
            .entry(name.to_string())
            .or_insert_with(|| SlidingWindow::new(limit, now));
        window.retune(limit);
        window.take(now)
    }

    /// The limiter's clock, so a caller reads the same time its windows use.
    pub fn clock(&self) -> &C {
        &self.clock
    }

    /// Record one admit in the unknown-sender window.
    ///
    /// Why: an unrouted sender is audited, and that audit path must not be
    /// floodable (Q6).
    /// What: one window for every unknown sender on every channel.
    /// Test: `unknown_sender_bucket_is_separate`.
    pub fn take_unknown_sender(&mut self) -> BucketDecision {
        let now = self.clock.now();
        self.unknown.take(now)
    }
}
