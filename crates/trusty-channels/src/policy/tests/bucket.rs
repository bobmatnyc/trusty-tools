//! `RateLimiter` and `TokenBucket`: 100 per minute per binding, no early
//! refill, and a separate bucket for unknown senders.

use super::{limit, policy, spec, three_routes, TestClock, BOB_SLACK};
use crate::policy::{BucketDecision, Channel, MessageKind, RateLimit, RateLimiter};

use BucketDecision::{Admit, Exhausted};

#[test]
fn bucket_101st_in_a_minute_exhausted() {
    let p = policy(three_routes());
    let bob = &p.routes()[1];
    let clock = TestClock::default();
    let mut limiter = RateLimiter::new(p.default_rate_limit(), clock.clone());
    // 100 messages, one per millisecond: all pass.
    for ms in 0..100 {
        clock.set_ms(ms);
        assert_eq!(
            limiter.take_route(bob),
            Admit,
            "message {} at {ms} ms",
            ms + 1
        );
    }
    // The 101st, still inside the first second, is refused.
    clock.set_ms(100);
    assert_eq!(limiter.take_route(bob), Exhausted, "message 101");
}

#[test]
fn bucket_exhausted_does_not_refill_early() {
    let p = policy(three_routes());
    let bob = &p.routes()[1];
    let clock = TestClock::default();
    let mut limiter = RateLimiter::new(p.default_rate_limit(), clock.clone());
    for _ in 0..100 {
        assert_eq!(limiter.take_route(bob), Admit);
    }
    // One token takes 600 ms at 100 per 60 s. Refused takes before then
    // neither refill the bucket nor lose the time already earned.
    for ms in [0, 0, 100, 300, 599] {
        clock.set_ms(ms);
        assert_eq!(limiter.take_route(bob), Exhausted, "at {ms} ms");
    }
    clock.set_ms(601);
    assert_eq!(limiter.take_route(bob), Admit, "one token at 601 ms");
    assert_eq!(limiter.take_route(bob), Exhausted, "and only one");
}

#[test]
fn unknown_sender_bucket_is_separate() {
    let p = policy(three_routes());
    let (bob_dm, bob_tg) = (&p.routes()[1], &p.routes()[2]);
    let mut limiter = RateLimiter::new(p.default_rate_limit(), TestClock::default());
    // Drain the unknown-sender bucket.
    for _ in 0..100 {
        assert_eq!(limiter.take_unknown_sender(), Admit);
    }
    assert_eq!(limiter.take_unknown_sender(), Exhausted);
    // A routed binding still has its own full bucket.
    for _ in 0..100 {
        assert_eq!(limiter.take_route(bob_dm), Admit);
    }
    assert_eq!(limiter.take_route(bob_dm), Exhausted);
    // Draining one binding leaves another binding full.
    assert_eq!(limiter.take_route(bob_tg), Admit);
    // And the unknown-sender bucket stays empty.
    assert_eq!(limiter.take_unknown_sender(), Exhausted);
}

#[test]
fn route_lower_limit_is_enforced_by_its_bucket() {
    let mut route = spec(
        Channel::Slack,
        "bob-dm",
        BOB_SLACK,
        &[MessageKind::Question],
    );
    route.rate_limit = Some(limit(2, 0.5));
    let p = policy(vec![route]);
    let bob = &p.routes()[0];
    let clock = TestClock::default();
    let mut limiter = RateLimiter::new(p.default_rate_limit(), clock.clone());
    assert_eq!(limiter.take_route(bob), Admit);
    assert_eq!(limiter.take_route(bob), Admit);
    assert_eq!(limiter.take_route(bob), Exhausted);
    // 0.5 per second: one token after 2 s.
    clock.set_ms(1_999);
    assert_eq!(limiter.take_route(bob), Exhausted);
    clock.set_ms(2_001);
    assert_eq!(limiter.take_route(bob), Admit);
    assert_eq!(p.default_rate_limit(), RateLimit::DEFAULT);
}
