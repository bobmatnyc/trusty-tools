//! Inbound rate limit (#8454 S3b): at most 100 messages a minute per route
//! and in the shared unknown-sender window, taken after the route lookup and
//! before any state is read or written. Time comes from [`FakeClock`]; no
//! test sleeps.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use wiremock::MockServer;

use super::{bootstrap, dm, mount_create, mount_token, Project, DM_JANET, JANET, THREE_ROUTES};
use crate::gchat::api::client::Endpoints;
use crate::gchat::api::events::PulledMessage;
use crate::gchat::channel::GchatChannel;
use crate::gchat::inbound::{InboundOutcome, LimitBucket};
use crate::policy::Clock;

const STRANGER: &str = "stranger@example.com";

/// A settable clock, shared between a test and the channel it opens.
#[derive(Debug, Clone, Default)]
struct FakeClock(Arc<AtomicU64>);

impl FakeClock {
    fn set_ms(&self, ms: u64) {
        self.0.store(ms, Ordering::SeqCst);
    }
}

impl Clock for FakeClock {
    fn now(&self) -> Duration {
        Duration::from_millis(self.0.load(Ordering::SeqCst))
    }
}

fn open(project: &Project, server: &MockServer, clock: &FakeClock) -> GchatChannel {
    GchatChannel::open_with_clock(
        project.dir(),
        Endpoints::single_host(&server.uri()),
        Box::new(clock.clone()),
    )
    .expect("open channel")
}

/// DMs from `sender` in `space` saying "hello", ack ids `<prefix><i>`.
fn dms(prefix: &str, sender: &str, space: &str, n: usize) -> Vec<PulledMessage> {
    (0..n)
        .map(|i| dm(&format!("{prefix}{i}"), sender, space, "hello"))
        .collect()
}

fn route_limited(route: &str) -> InboundOutcome {
    InboundOutcome::RateLimited {
        bucket: LimitBucket::Route(route.to_string()),
    }
}

fn unknown_limited() -> InboundOutcome {
    InboundOutcome::RateLimited {
        bucket: LimitBucket::UnknownSender,
    }
}

fn no_open_question() -> InboundOutcome {
    InboundOutcome::Dropped {
        reason: "no_open_question",
    }
}

/// The parsed `rate_limited` audit lines.
fn limited_lines(project: &Project) -> Vec<serde_json::Value> {
    project
        .audit_lines()
        .into_iter()
        .filter(|l| l["event"] == "rate_limited")
        .collect()
}

#[tokio::test]
async fn route_101st_message_in_a_minute_is_dropped_and_acked() {
    let server = MockServer::start().await;
    let project = Project::committed(THREE_ROUTES);
    let clock = FakeClock::default();
    let channel = open(&project, &server, &clock);
    let report = channel
        .process_batch(&dms("a", JANET, DM_JANET, 100))
        .expect("batch");
    assert_eq!(report.rate_limited, 0, "{:?}", report.outcomes);

    clock.set_ms(59_999);
    let report = channel
        .process_batch(&[dm("over", JANET, DM_JANET, "hello")])
        .expect("batch");
    assert_eq!(report.outcomes, [route_limited("janet")]);
    assert_eq!(report.ack_ids, ["over"], "the drop is acked");
    assert!(report.withheld.is_empty());
    assert_eq!(report.rate_limited, 1);
    let lines = limited_lines(&project);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0]["reason"], "route_rate_limit");
    assert_eq!(lines[0]["route"], "janet");
}

#[tokio::test]
async fn rate_limited_reply_does_not_resolve_the_question() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    mount_create(&server).await;
    let project = Project::committed(THREE_ROUTES);
    let clock = FakeClock::default();
    let channel = open(&project, &server, &clock);
    // One admit for the bootstrap DM, 99 more: the window is full.
    bootstrap(&channel, JANET, DM_JANET);
    channel
        .send_question("janet", "Ship it?")
        .await
        .expect("q1");
    let report = channel
        .process_batch(&dms("a", JANET, DM_JANET, 99))
        .expect("batch");
    assert_eq!(report.rate_limited, 0, "{:?}", report.outcomes);

    let report = channel
        .process_batch(&[dm("r1", JANET, DM_JANET, "[Q-1] yes")])
        .expect("batch");
    assert_eq!(report.outcomes, [route_limited("janet")]);
    assert!(
        channel.question(1).expect("q1").is_open(),
        "a rate-limited reply resolved the question"
    );

    // Once the window slides, the redelivered reply binds.
    clock.set_ms(60_000);
    let report = channel
        .process_batch(&[dm("r1", JANET, DM_JANET, "[Q-1] yes")])
        .expect("batch");
    assert_eq!(
        report.outcomes,
        [InboundOutcome::Answered {
            question_id: 1,
            route: "janet".into()
        }]
    );
}

#[tokio::test]
async fn backwards_clock_drops_and_leaves_state_unchanged() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    mount_create(&server).await;
    let project = Project::committed(THREE_ROUTES);
    let clock = FakeClock::default();
    clock.set_ms(10_000);
    let channel = open(&project, &server, &clock);
    bootstrap(&channel, JANET, DM_JANET);
    channel
        .send_question("janet", "Ship it?")
        .await
        .expect("q1");
    let before = channel.health();

    // The clock reads earlier than the windows' latest reading: both the
    // route window and the unknown-sender window fail closed.
    clock.set_ms(5_000);
    let report = channel
        .process_batch(&[
            dm("r1", JANET, DM_JANET, "[Q-1] yes"),
            dm("s1", STRANGER, "spaces/DMSTRANGER", "hello"),
        ])
        .expect("batch");
    assert_eq!(report.outcomes, [route_limited("janet"), unknown_limited()]);
    assert_eq!(report.ack_ids, ["r1", "s1"]);
    assert!(channel.question(1).expect("q1").is_open());
    assert_eq!(channel.health(), before, "a limited drop changed state");

    // Time resumes: the reply binds.
    clock.set_ms(10_001);
    let report = channel
        .process_batch(&[dm("r1", JANET, DM_JANET, "[Q-1] yes")])
        .expect("batch");
    assert_eq!(
        report.outcomes,
        [InboundOutcome::Answered {
            question_id: 1,
            route: "janet".into()
        }]
    );
}

#[tokio::test]
async fn restart_resets_the_windows() {
    let server = MockServer::start().await;
    let project = Project::committed(THREE_ROUTES);
    let clock = FakeClock::default();
    let channel = open(&project, &server, &clock);
    channel
        .process_batch(&dms("a", JANET, DM_JANET, 100))
        .expect("batch");
    let report = channel
        .process_batch(&[dm("over", JANET, DM_JANET, "hello")])
        .expect("batch");
    assert_eq!(report.outcomes, [route_limited("janet")]);

    // The windows live in memory only: a reopen at the same instant admits.
    drop(channel);
    let channel = open(&project, &server, &clock);
    let report = channel
        .process_batch(&[dm("after", JANET, DM_JANET, "hello")])
        .expect("batch");
    assert_eq!(report.outcomes, [no_open_question()]);
    assert_eq!(report.rate_limited, 0);
}
