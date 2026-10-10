//! Inbound rate limit (#8454 S3b): at most 100 messages a minute per route
//! and in the shared unknown-sender window, taken after the route lookup and
//! before any state is read or written. Time comes from [`FakeClock`]; no
//! test sleeps.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::{
    bootstrap, dm, message, mount_create, mount_token, Project, ASKER, DM_JANET, JANET,
    SUBSCRIPTION, THREE_ROUTES,
};
use crate::gchat::api::client::Endpoints;
use crate::gchat::api::events::{ChatEvent, MessageEvent, PulledMessage};
use crate::gchat::channel::GchatChannel;
use crate::gchat::inbound::{InboundOutcome, LimitBucket};
use crate::gchat::poller::Poller;
use crate::policy::Clock;

const STRANGER: &str = "stranger@example.com";
const DM_ASKER: &str = "spaces/DMASKER";

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

fn is_limited(outcome: &InboundOutcome) -> bool {
    matches!(outcome, InboundOutcome::RateLimited { .. })
}

/// `msg` with its MESSAGE event changed by `edit`.
fn edited(mut msg: PulledMessage, edit: impl FnOnce(&mut MessageEvent)) -> PulledMessage {
    if let Ok(ChatEvent::Message(m)) = &mut msg.event {
        edit(m);
    }
    msg
}

/// Fill janet's window with 100 admits at the current time.
fn drain_janet(channel: &GchatChannel) {
    let report = channel
        .process_batch(&dms("fill", JANET, DM_JANET, 100))
        .expect("batch");
    assert_eq!(report.rate_limited, 0, "{:?}", report.outcomes);
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

#[tokio::test]
async fn first_100_messages_pass_the_limiter() {
    let server = MockServer::start().await;
    let project = Project::committed(THREE_ROUTES);
    let clock = FakeClock::default();
    let channel = open(&project, &server, &clock);
    // 100 messages spread over 59.4 s: every one is admitted and acked.
    for i in 0..100u64 {
        clock.set_ms(i * 600);
        let ack = format!("m{i}");
        let report = channel
            .process_batch(&[dm(&ack, JANET, DM_JANET, "hello")])
            .expect("batch");
        assert!(!is_limited(&report.outcomes[0]), "message {}", i + 1);
        assert_eq!(report.ack_ids, [ack]);
    }
}

#[tokio::test]
async fn window_slides_after_60s_boundary() {
    let server = MockServer::start().await;
    let project = Project::committed(THREE_ROUTES);
    let clock = FakeClock::default();
    let channel = open(&project, &server, &clock);
    // One admit per millisecond from 0 to 99 ms.
    for i in 0..100u64 {
        clock.set_ms(i);
        let report = channel
            .process_batch(&[dm(&format!("m{i}"), JANET, DM_JANET, "hello")])
            .expect("batch");
        assert_eq!(report.rate_limited, 0);
    }
    let one = |ack: &str| {
        channel
            .process_batch(&[dm(ack, JANET, DM_JANET, "hello")])
            .expect("batch")
            .outcomes
    };
    clock.set_ms(59_999);
    assert_eq!(one("x1"), [route_limited("janet")], "at 59,999 ms");
    // The admit at 0 ms leaves the window at 60,000 ms; the one at 1 ms
    // is still inside it.
    clock.set_ms(60_000);
    assert_eq!(one("x2"), [no_open_question()], "at 60,000 ms");
    assert_eq!(one("x3"), [route_limited("janet")], "again at 60,000 ms");
}

#[tokio::test]
async fn rate_limited_message_does_not_learn_a_dm_space() {
    let server = MockServer::start().await;
    let project = Project::committed(THREE_ROUTES);
    let clock = FakeClock::default();
    let channel = open(&project, &server, &clock);
    // 100 messages from janet outside a DM: admitted, and none learns.
    let named: Vec<PulledMessage> = (0..100)
        .map(|i| message(&format!("n{i}"), JANET, "spaces/ROOM", "SPACE", "hi", None))
        .collect();
    let report = channel.process_batch(&named).expect("batch");
    assert_eq!(report.rate_limited, 0);

    let report = channel
        .process_batch(&[dm("boot", JANET, DM_JANET, "hello")])
        .expect("batch");
    assert_eq!(report.outcomes, [route_limited("janet")]);
    let health = channel.health();
    let janet = health.routes.iter().find(|r| r.name == "janet");
    assert_eq!(
        janet.expect("janet route").space,
        None,
        "a limited DM taught a space"
    );
}

#[tokio::test]
async fn buckets_are_keyed_by_route_not_sender_id() {
    let server = MockServer::start().await;
    let project = Project::committed(THREE_ROUTES);
    let clock = FakeClock::default();
    let channel = open(&project, &server, &clock);
    drain_janet(&channel);

    // Another route has its own window.
    let report = channel
        .process_batch(&[dm("a1", ASKER, DM_ASKER, "hello")])
        .expect("batch");
    assert!(!is_limited(&report.outcomes[0]), "{:?}", report.outcomes);
    // A new user resource name for janet's email is still janet's window.
    let renamed = edited(dm("j1", JANET, DM_JANET, "hello"), |m| {
        m.sender.name = "users/another-id".into();
    });
    let report = channel.process_batch(&[renamed]).expect("batch");
    assert_eq!(report.outcomes, [route_limited("janet")]);
}

#[tokio::test]
async fn unknown_senders_share_one_bucket() {
    let server = MockServer::start().await;
    let project = Project::committed(THREE_ROUTES);
    let clock = FakeClock::default();
    let channel = open(&project, &server, &clock);
    // 100 distinct unrouted senders drain the one shared window.
    let strangers: Vec<PulledMessage> = (0..100)
        .map(|i| {
            let sender = format!("s{i}@example.com");
            dm(&format!("s{i}"), &sender, "spaces/DMS", "hello")
        })
        .collect();
    let report = channel.process_batch(&strangers).expect("batch");
    assert_eq!(report.rate_limited, 0, "{:?}", report.outcomes);

    let no_email = edited(dm("e1", STRANGER, "spaces/DMS", "hello"), |m| {
        m.sender.email = None;
    });
    let report = channel
        .process_batch(&[no_email, dm("e2", STRANGER, "spaces/DMS", "hello")])
        .expect("batch");
    assert_eq!(report.outcomes, [unknown_limited(), unknown_limited()]);
    assert_eq!(report.ack_ids, ["e1", "e2"]);
    // A routed sender is not in that window.
    let report = channel
        .process_batch(&[dm("j1", JANET, DM_JANET, "hello")])
        .expect("batch");
    assert!(!is_limited(&report.outcomes[0]), "{:?}", report.outcomes);
}

#[tokio::test]
async fn bot_sender_counts_against_unknown_bucket() {
    let server = MockServer::start().await;
    let project = Project::committed(THREE_ROUTES);
    let clock = FakeClock::default();
    let channel = open(&project, &server, &clock);
    // Bots using janet's email still spend the unknown-sender window.
    let bots: Vec<PulledMessage> = (0..100)
        .map(|i| {
            edited(dm(&format!("b{i}"), JANET, DM_JANET, "hello"), |m| {
                m.sender.user_type = Some("BOT".into());
            })
        })
        .collect();
    let report = channel.process_batch(&bots).expect("batch");
    assert_eq!(report.rate_limited, 0, "{:?}", report.outcomes);

    let report = channel
        .process_batch(&[dm("s1", STRANGER, "spaces/DMS", "hello")])
        .expect("batch");
    assert_eq!(report.outcomes, [unknown_limited()]);
    // Janet's own window is untouched.
    let report = channel
        .process_batch(&[dm("j1", JANET, DM_JANET, "hello")])
        .expect("batch");
    assert!(!is_limited(&report.outcomes[0]), "{:?}", report.outcomes);
}

#[tokio::test]
async fn rate_limited_audit_line_has_expected_fields_and_no_text() {
    let server = MockServer::start().await;
    let project = Project::committed(THREE_ROUTES);
    let clock = FakeClock::default();
    let channel = open(&project, &server, &clock);
    drain_janet(&channel);
    let text = "SENTINEL-8454-limited-text";
    channel
        .process_batch(&[dm("over", JANET, DM_JANET, text)])
        .expect("batch");

    let lines = limited_lines(&project);
    assert_eq!(lines.len(), 1, "{lines:?}");
    let line = &lines[0];
    let mut keys: Vec<&str> = line
        .as_object()
        .expect("object")
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        ["event", "length", "message_id", "reason", "route", "sender", "space", "ts"]
    );
    assert_eq!(line["reason"], "route_rate_limit");
    assert_eq!(line["route"], "janet");
    assert_eq!(line["sender"], JANET);
    assert_eq!(line["space"], DM_JANET);
    assert_eq!(line["message_id"], "id-over");
    assert_eq!(line["length"], text.len());
    assert!(!project.audit_text().contains(text), "message text audited");
}

#[tokio::test]
async fn audit_write_failure_on_rate_limited_withholds_ack() {
    let server = MockServer::start().await;
    let project = Project::committed(THREE_ROUTES);
    let clock = FakeClock::default();
    let channel = open(&project, &server, &clock);
    drain_janet(&channel);
    let audit = project.dir().join(".trusty-channels/state/audit.jsonl");
    std::fs::remove_file(&audit).expect("rm audit");
    std::fs::create_dir(&audit).expect("make audit unwritable");

    let report = channel
        .process_batch(&[dm("over", JANET, DM_JANET, "hello")])
        .expect("batch");
    assert_eq!(report.outcomes, [route_limited("janet")]);
    assert!(report.ack_ids.is_empty(), "acked with no audit line");
    assert_eq!(report.withheld, ["over"]);

    // The failed line is not remembered: the redelivery writes it.
    std::fs::remove_dir(&audit).expect("restore audit");
    let report = channel
        .process_batch(&[dm("over", JANET, DM_JANET, "hello")])
        .expect("batch");
    assert_eq!(report.outcomes, [route_limited("janet")]);
    assert_eq!(report.ack_ids, ["over"]);
    assert_eq!(limited_lines(&project).len(), 1);
}

#[tokio::test]
async fn second_limited_drop_in_a_window_writes_no_audit_line() {
    let server = MockServer::start().await;
    let project = Project::committed(THREE_ROUTES);
    let clock = FakeClock::default();
    let channel = open(&project, &server, &clock);
    drain_janet(&channel);

    clock.set_ms(1_000);
    let report = channel
        .process_batch(&[dm("over1", JANET, DM_JANET, "hello")])
        .expect("batch");
    assert_eq!(report.rate_limited, 1);
    assert_eq!(limited_lines(&project).len(), 1);

    // #8454 Q2: a second drop in the window is counted and acked, with no
    // second line.
    clock.set_ms(2_000);
    let report = channel
        .process_batch(&[dm("over2", JANET, DM_JANET, "hello")])
        .expect("batch");
    assert_eq!(report.outcomes, [route_limited("janet")]);
    assert_eq!(report.ack_ids, ["over2"]);
    assert_eq!(report.rate_limited, 1);
    assert_eq!(limited_lines(&project).len(), 1, "a second line was written");

    // Another bucket writes its own first line.
    let strangers: Vec<PulledMessage> = (0..101)
        .map(|i| dm(&format!("s{i}"), STRANGER, "spaces/DMS", "hello"))
        .collect();
    let report = channel.process_batch(&strangers).expect("batch");
    assert_eq!(report.rate_limited, 1);
    let lines = limited_lines(&project);
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert_eq!(lines[1]["reason"], "unknown_sender_rate_limit");
}

/// A base64 Pub/Sub payload: a DM from janet saying "hello".
fn janet_event(n: usize) -> String {
    let event = json!({
        "type": "MESSAGE",
        "space": {"name": DM_JANET, "spaceType": "DIRECT_MESSAGE"},
        "message": {
            "name": format!("{DM_JANET}/messages/P{n}"),
            "text": "hello",
            "sender": {"name": "users/1", "email": JANET, "type": "HUMAN"}
        }
    });
    base64::engine::general_purpose::STANDARD.encode(event.to_string())
}

#[tokio::test]
async fn poll_status_counts_rate_limited_and_stays_healthy() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    let received: Vec<serde_json::Value> = (0..2)
        .map(|i| json!({"ackId": format!("ack-{i}"), "message": {"data": janet_event(i), "messageId": format!("m{i}")}}))
        .collect();
    Mock::given(method("POST"))
        .and(path(format!("/v1/{SUBSCRIPTION}:pull")))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"receivedMessages": received})),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(format!("/v1/{SUBSCRIPTION}:acknowledge")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(1)
        .mount(&server)
        .await;
    let project = Project::committed(THREE_ROUTES);
    let clock = FakeClock::default();
    let channel = Arc::new(open(&project, &server, &clock));
    drain_janet(&channel);

    let poller = Poller::new(Arc::clone(&channel), 10);
    let report = poller.tick().await.expect("tick");
    assert_eq!(report.ack_ids, ["ack-0", "ack-1"]);
    let status = poller.status();
    let status = status.lock().expect("status lock");
    assert_eq!(status.rate_limited, 2);
    assert!(status.is_healthy(), "{status:?}");
    assert_eq!(status.consecutive_failures, 0);
    assert!(status.last_ok_at.is_some());
}
