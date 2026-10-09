//! Tests for the channel policy (#8454 S1).
//!
//! Why: each S1 rule needs a test that a fail-open implementation fails.
//! What: shared builders for route specs and a settable test clock. Pure
//! data; no file, network or sleep.
//! Test: this module is the test.

mod bucket;
mod build;
mod egress;
mod inbound;

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use crate::policy::{
    Channel, ChannelPolicy, Clock, MessageKind, PolicySpec, RateLimitSpec, RouteSpec,
};

pub(super) const JANET: &str = "janet@example.com";
pub(super) const BOB_SLACK: &str = "U0ABCDEF1";
pub(super) const BOB_TG: &str = "123456789";

/// A route spec with no rate limit.
pub(super) fn spec(
    channel: Channel,
    name: &str,
    recipient: &str,
    kinds: &[MessageKind],
) -> RouteSpec {
    RouteSpec {
        channel,
        name: name.to_string(),
        recipient: recipient.to_string(),
        kinds: kinds.to_vec(),
        rate_limit: None,
    }
}

/// One route per channel: gchat `janet` (question, review_notice), Slack
/// `bob-dm` and Telegram `bob-tg` (question).
pub(super) fn three_routes() -> Vec<RouteSpec> {
    use MessageKind::{Question, ReviewNotice};
    vec![
        spec(Channel::Gchat, "janet", JANET, &[Question, ReviewNotice]),
        spec(Channel::Slack, "bob-dm", BOB_SLACK, &[Question]),
        spec(Channel::Telegram, "bob-tg", BOB_TG, &[Question]),
    ]
}

/// Build a policy from routes that must be valid.
pub(super) fn policy(routes: Vec<RouteSpec>) -> ChannelPolicy {
    ChannelPolicy::build(PolicySpec {
        rate_limit: None,
        routes,
    })
    .expect("test routes are valid")
}

/// A rate-limit spec.
pub(super) fn limit(limit: i64, window_secs: i64) -> RateLimitSpec {
    RateLimitSpec { limit, window_secs }
}

/// A clock the test sets by hand.
#[derive(Debug, Clone, Default)]
pub(super) struct TestClock(Rc<Cell<Duration>>);

impl TestClock {
    pub(super) fn set_ms(&self, ms: u64) {
        self.0.set(Duration::from_millis(ms));
    }
}

impl Clock for TestClock {
    fn now(&self) -> Duration {
        self.0.get()
    }
}
