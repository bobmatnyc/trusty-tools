//! `RateLimiter` and `SlidingWindow`: at most 100 admits in any 60 s per
//! binding, no early re-admit, and a separate window for unknown senders.

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
    // No admit until the first 100 leave the 60 s window.
    for ms in [0, 601, 30_000, 59_999] {
        clock.set_ms(ms);
        assert_eq!(limiter.take_route(bob), Exhausted, "at {ms} ms");
    }
    clock.set_ms(60_000);
    assert_eq!(limiter.take_route(bob), Admit, "at 60,000 ms");
}

#[test]
fn backwards_clock_reading_never_readmits_early() {
    let p = policy(three_routes());
    let bob = &p.routes()[1];
    let clock = TestClock::default();
    let mut limiter = RateLimiter::new(p.default_rate_limit(), clock.clone());
    // Drain at 10 s; a reading of 5 s, then of 10.3 s, is refused.
    clock.set_ms(10_000);
    for _ in 0..100 {
        assert_eq!(limiter.take_route(bob), Admit);
    }
    for ms in [5_000, 10_300] {
        clock.set_ms(ms);
        assert_eq!(limiter.take_route(bob), Exhausted, "drained, at {ms} ms");
    }
    // A backwards reading is refused even with room left: it is a clock
    // fault, and it records nothing.
    assert_eq!(limiter.take_unknown_sender(), Admit, "at 10,300 ms");
    clock.set_ms(5_000);
    assert_eq!(limiter.take_unknown_sender(), Exhausted, "backwards to 5 s");
    clock.set_ms(10_300);
    assert_eq!(limiter.take_unknown_sender(), Admit, "time resumed");
    // The drained route re-admits 60 s after its admits, not earlier.
    clock.set_ms(69_999);
    assert_eq!(limiter.take_route(bob), Exhausted, "at 69,999 ms");
    clock.set_ms(70_000);
    assert_eq!(limiter.take_route(bob), Admit, "at 70,000 ms");
}

#[test]
fn rebuilt_lower_route_limit_keeps_the_log_and_refuses() {
    let p = policy(three_routes());
    let clock = TestClock::default();
    let mut limiter = RateLimiter::new(p.default_rate_limit(), clock.clone());
    for _ in 0..40 {
        assert_eq!(limiter.take_route(&p.routes()[1]), Admit);
    }
    // Rebuild with bob-dm lowered to 40 per 60 s: the 40 admits already
    // made count against it, and the rebuild refills nothing.
    let mut routes = three_routes();
    routes[1].rate_limit = Some(limit(40, 60));
    let lowered = policy(routes);
    let bob = &lowered.routes()[1];
    assert_eq!(bob.rate_limit().limit(), 40);
    assert_eq!(
        limiter.take_route(bob),
        Exhausted,
        "right after the rebuild"
    );
    clock.set_ms(1);
    assert_eq!(limiter.take_route(bob), Exhausted, "1 ms later");
}

#[test]
fn unknown_sender_bucket_is_separate() {
    let p = policy(three_routes());
    let (bob_dm, bob_tg) = (&p.routes()[1], &p.routes()[2]);
    let mut limiter = RateLimiter::new(p.default_rate_limit(), TestClock::default());
    // Drain the unknown-sender window.
    for _ in 0..100 {
        assert_eq!(limiter.take_unknown_sender(), Admit);
    }
    assert_eq!(limiter.take_unknown_sender(), Exhausted);
    // A routed binding still has its own empty window.
    for _ in 0..100 {
        assert_eq!(limiter.take_route(bob_dm), Admit);
    }
    assert_eq!(limiter.take_route(bob_dm), Exhausted);
    // Draining one binding leaves another binding empty.
    assert_eq!(limiter.take_route(bob_tg), Admit);
    // And the unknown-sender window stays full.
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
    route.rate_limit = Some(limit(2, 120));
    let p = policy(vec![route]);
    let bob = &p.routes()[0];
    let clock = TestClock::default();
    let mut limiter = RateLimiter::new(p.default_rate_limit(), clock.clone());
    assert_eq!(limiter.take_route(bob), Admit);
    assert_eq!(limiter.take_route(bob), Admit);
    assert_eq!(limiter.take_route(bob), Exhausted);
    // A 120 s window: refused until both admits are 120 s old.
    clock.set_ms(119_999);
    assert_eq!(limiter.take_route(bob), Exhausted);
    clock.set_ms(120_000);
    assert_eq!(limiter.take_route(bob), Admit);
    assert_eq!(p.default_rate_limit(), RateLimit::DEFAULT);
}
