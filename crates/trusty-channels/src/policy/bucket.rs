//! Inbound rate limiting: a token bucket per (channel, binding id), plus one
//! bucket for unknown senders.
//!
//! Why: a routed sender may flood an open question, and an unknown sender
//! may flood the audit log (#7457 folded into #8454; Architect Q6).
//! What: [`TokenBucket`] refills continuously at its limit's rate up to its
//! capacity; a take spends one whole token or reports
//! [`BucketDecision::Exhausted`]. [`RateLimiter`] keys buckets by
//! (channel, route name) and keeps the unknown-sender bucket apart. State is
//! in memory; time comes from an injected [`Clock`], so tests never sleep.
//! Test: `src/policy/tests/bucket.rs`.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::policy::types::{Channel, RateLimit, Route};

/// A monotonic time source: elapsed time since a fixed origin.
///
/// Why: the bucket needs time that never runs backwards, and tests need to
/// set it.
/// What: [`MonotonicClock`] in production; a test passes its own.
/// Test: `bucket_exhausted_does_not_refill_early`.
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
/// What: `Admit` spent one token; `Exhausted` spent nothing.
/// Test: `bucket_101st_in_a_minute_exhausted`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use]
pub enum BucketDecision {
    /// One token was spent; the message may proceed to the policy check.
    Admit,
    /// No whole token is left; drop the message.
    Exhausted,
}

/// One token bucket.
///
/// Why: the #7457 conditions — 100 messages inside a minute pass, the 101st
/// does not, and an empty bucket does not refill before its time.
/// What: starts full. A take first adds `elapsed * refill_per_sec` tokens,
/// capped at capacity, then spends one if a whole token is there. A clock
/// reading earlier than the last one adds nothing.
/// Test: `bucket_101st_in_a_minute_exhausted`,
/// `bucket_exhausted_does_not_refill_early`.
#[derive(Debug, Clone)]
pub struct TokenBucket {
    limit: RateLimit,
    tokens: f64,
    last: Duration,
}

impl TokenBucket {
    /// A full bucket for `limit`, last refilled at `now`.
    pub fn new(limit: RateLimit, now: Duration) -> Self {
        Self {
            limit,
            tokens: f64::from(limit.capacity()),
            last: now,
        }
    }

    /// The bucket's limit.
    pub fn limit(&self) -> RateLimit {
        self.limit
    }

    /// Refill for the time since the last take, then spend one token.
    pub fn take(&mut self, now: Duration) -> BucketDecision {
        self.refill(now);
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            BucketDecision::Admit
        } else {
            BucketDecision::Exhausted
        }
    }

    /// Adopt a changed limit without refilling: tokens are capped at the
    /// new capacity, never raised.
    fn retune(&mut self, limit: RateLimit, now: Duration) {
        if self.limit != limit {
            // #8454: settle the old rate up to now, then switch.
            self.refill(now);
            self.limit = limit;
            self.tokens = self.tokens.min(f64::from(limit.capacity()));
        }
    }

    /// Add the tokens earned since the last reading, capped at capacity.
    fn refill(&mut self, now: Duration) {
        let elapsed = now.saturating_sub(self.last);
        self.last = self.last.max(now);
        let capacity = f64::from(self.limit.capacity());
        self.tokens =
            (self.tokens + elapsed.as_secs_f64() * self.limit.refill_per_sec()).min(capacity);
    }
}

/// The in-memory buckets for one channel process.
///
/// Why: one bucket per binding, so one flooding route cannot spend
/// another's tokens, and one separate bucket for unknown senders (Q6).
/// What: [`RateLimiter::take_route`] uses the bucket keyed by the route's
/// (channel, name), created full on first use with the route's limit;
/// [`RateLimiter::take_unknown_sender`] uses the unknown-sender bucket.
/// Restarting the process resets every bucket.
/// Test: `unknown_sender_bucket_is_separate`,
/// `route_lower_limit_is_enforced_by_its_bucket`.
#[derive(Debug)]
pub struct RateLimiter<C: Clock> {
    clock: C,
    routes: HashMap<Channel, HashMap<String, TokenBucket>>,
    unknown: TokenBucket,
}

impl<C: Clock> RateLimiter<C> {
    /// A limiter whose unknown-sender bucket uses `unknown_limit`, normally
    /// [`ChannelPolicy::default_rate_limit`](crate::policy::ChannelPolicy::default_rate_limit).
    pub fn new(unknown_limit: RateLimit, clock: C) -> Self {
        let now = clock.now();
        Self {
            clock,
            routes: HashMap::new(),
            unknown: TokenBucket::new(unknown_limit, now),
        }
    }

    /// Take one token from the bucket of `route`'s (channel, name).
    ///
    /// Why: runs before the inbound check for a sender with a route.
    /// What: a route whose limit changed (a rebuilt policy) keeps its
    /// tokens, capped at the new capacity.
    /// Test: `bucket_101st_in_a_minute_exhausted`,
    /// `unknown_sender_bucket_is_separate`.
    pub fn take_route(&mut self, route: &Route) -> BucketDecision {
        let now = self.clock.now();
        let by_name = self.routes.entry(route.channel()).or_default();
        let bucket = by_name
            .entry(route.name().to_string())
            .or_insert_with(|| TokenBucket::new(route.rate_limit(), now));
        bucket.retune(route.rate_limit(), now);
        bucket.take(now)
    }

    /// Take one token from the unknown-sender bucket.
    ///
    /// Why: an unrouted sender is audited, and that audit path must not be
    /// floodable (Q6).
    /// What: one bucket for every unknown sender on every channel.
    /// Test: `unknown_sender_bucket_is_separate`.
    pub fn take_unknown_sender(&mut self) -> BucketDecision {
        let now = self.clock.now();
        self.unknown.take(now)
    }
}
