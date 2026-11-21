//! #9779 D5t: the owner push as a Slack DM. No test touches the network or
//! reads the real token: senders are fakes, and the curl seam is scripted.

use std::cell::RefCell;
use std::io;
use std::time::Duration;

use serde_json::{Map, Value};
use trusty_architect::classify::{Alert, alert};
use trusty_architect::poller::push::{PUSH_AFTER, PUSH_FAILED, PushRoute, push_step};
use trusty_architect::poller::slack::{CurlRun, PushError, PushSender, SlackDm};
use trusty_architect::poller::steps::scripts::ChildOutput;

const T0: f64 = 1_000_000.0;
const SENTINEL: &str = "xoxb-SENTINEL-0000-never-log-me";

/// Records each send; answers with `fail` when set.
#[derive(Default)]
struct FakeSender {
    sent: RefCell<Vec<(String, String)>>,
    fail: Option<fn() -> PushError>,
}

impl PushSender for FakeSender {
    fn send(&self, recipient: &str, text: &str) -> Result<(), PushError> {
        self.sent
            .borrow_mut()
            .push((recipient.to_owned(), text.to_owned()));
        self.fail.map_or(Ok(()), |f| Err(f()))
    }
}

fn dead(now: f64) -> Vec<Alert> {
    vec![alert(
        "architect_dead",
        "dead",
        "pane is gone",
        "tm-architect",
        "",
        now,
    )]
}

fn route<'a>(channel: Option<&'a str>, sender: &'a dyn PushSender, shadow: bool) -> PushRoute<'a> {
    PushRoute {
        channel,
        recipient: Some("U0OWNER"),
        sender: Some(sender),
        hands_off: shadow,
        supervisor: "tm-architect",
    }
}

/// Runs the step once at `when` with the incident active.
fn at(other: &mut Map<String, Value>, r: &PushRoute<'_>, when: f64) -> (Vec<Alert>, Vec<String>) {
    push_step(other, &dead(when), when, r)
}

#[test]
fn push_off_sends_nothing() {
    let fake = FakeSender::default();
    let r = route(None, &fake, false);
    let mut other = Map::new();
    at(&mut other, &r, T0);
    let (alerts, lines) = at(&mut other, &r, T0 + PUSH_AFTER);
    assert!(alerts.is_empty());
    assert!(lines[0].contains("nothing sent"), "{lines:?}");
    assert!(fake.sent.borrow().is_empty());
}

#[test]
fn push_on_sends_once_per_incident() {
    let fake = FakeSender::default();
    let r = route(Some("slack"), &fake, false);
    let mut other = Map::new();
    assert!(at(&mut other, &r, T0).1.is_empty(), "not due yet");
    assert!(at(&mut other, &r, T0 + PUSH_AFTER - 1.0).1.is_empty());
    let (alerts, lines) = at(&mut other, &r, T0 + PUSH_AFTER);
    assert!(alerts.is_empty(), "{alerts:?}");
    assert_eq!(lines, ["pushed: architect_dead:dead to slack user U0OWNER"]);
    at(&mut other, &r, T0 + 3.0 * PUSH_AFTER);
    let sent = fake.sent.borrow().clone();
    assert_eq!(
        sent,
        [(
            "U0OWNER".to_owned(),
            "Architect is broken: pane is gone (architect_dead:dead) has not been cleared for 30 min."
                .to_owned()
        )],
        "exactly one send per incident"
    );
    // Cleared, then back: a new incident, a new push after 30 min.
    push_step(&mut other, &[], T0 + 4.0 * PUSH_AFTER, &r);
    at(&mut other, &r, T0 + 5.0 * PUSH_AFTER);
    at(&mut other, &r, T0 + 6.0 * PUSH_AFTER);
    assert_eq!(fake.sent.borrow().len(), 2);
}

#[test]
fn push_failure_kinds_are_alerts() {
    type Fail = fn() -> PushError;
    let kinds: [(Fail, &str); 6] = [
        (|| PushError::MissingToken, "token"),
        (|| PushError::BadToken, "token-format"),
        (|| PushError::Timeout, "timeout"),
        (|| PushError::Http(500), "http"),
        (|| PushError::Slack("channel_not_found".to_owned()), "slack"),
        (
            || PushError::Transport("curl exit 7".to_owned()),
            "transport",
        ),
    ];
    for (fail, fp) in kinds {
        let fake = FakeSender {
            fail: Some(fail),
            ..FakeSender::default()
        };
        let r = route(Some("slack"), &fake, false);
        let mut other = Map::new();
        at(&mut other, &r, T0);
        let (alerts, _) = at(&mut other, &r, T0 + PUSH_AFTER);
        assert_eq!(alerts.len(), 1, "{fp}");
        assert_eq!(
            (alerts[0].kind.as_str(), alerts[0].fingerprint.as_str()),
            (PUSH_FAILED, fp)
        );
        // Retried no sooner than the push cadence.
        assert!(
            at(&mut other, &r, T0 + PUSH_AFTER + 60.0).0.is_empty(),
            "{fp}"
        );
        assert_eq!(at(&mut other, &r, T0 + 2.0 * PUSH_AFTER).0.len(), 1, "{fp}");
        assert_eq!(fake.sent.borrow().len(), 2, "{fp}");
    }
    // Config failures: an unknown channel, no recipient, no sender.
    let fake = FakeSender::default();
    let cases = [
        (route(Some("pager"), &fake, false), "channel"),
        (
            PushRoute {
                recipient: None,
                ..route(Some("slack"), &fake, false)
            },
            "recipient",
        ),
        (
            PushRoute {
                sender: None,
                ..route(Some("slack"), &fake, false)
            },
            "sender",
        ),
    ];
    for (r, fp) in cases {
        let mut other = Map::new();
        at(&mut other, &r, T0);
        let (alerts, _) = at(&mut other, &r, T0 + PUSH_AFTER);
        assert_eq!(alerts[0].fingerprint, fp);
    }
    assert!(fake.sent.borrow().is_empty());
}

#[test]
fn push_shadow_sends_nothing() {
    let fake = FakeSender::default();
    let r = route(Some("slack"), &fake, true);
    let mut other = Map::new();
    at(&mut other, &r, T0);
    let (alerts, lines) = at(&mut other, &r, T0 + PUSH_AFTER);
    assert!(alerts.is_empty());
    assert_eq!(
        lines,
        ["would push: architect_dead:dead to slack user U0OWNER"]
    );
    assert!(fake.sent.borrow().is_empty());
}

/// Answers curl with a fixed result; records argv and stdin.
struct FakeCurl {
    answer: fn() -> io::Result<ChildOutput>,
    calls: RefCell<Vec<(Vec<String>, String)>>,
}

impl CurlRun for FakeCurl {
    fn run(&self, argv: &[String], stdin: &[u8], _: Duration) -> io::Result<ChildOutput> {
        let input = String::from_utf8_lossy(stdin).into_owned();
        self.calls.borrow_mut().push((argv.to_vec(), input));
        (self.answer)()
    }
}

fn exits(code: i32, stdout: &str) -> io::Result<ChildOutput> {
    Ok(ChildOutput {
        code: Some(code),
        stdout: stdout.to_owned(),
        // curl echoing the header is the worst case the redaction covers.
        stderr: format!("Authorization: Bearer {SENTINEL}"),
    })
}

fn slack(curl: &FakeCurl, token: &dyn Fn() -> Option<String>) -> Result<(), PushError> {
    let dm = SlackDm {
        curl,
        token,
        url: "https://slack.invalid/api/chat.postMessage".to_owned(),
        timeout: Duration::from_secs(1),
    };
    dm.send("U0OWNER", "Architect is broken")
}

#[test]
fn slack_dm_ok_reply_is_sent() {
    let curl = FakeCurl {
        answer: || exits(0, "{\"ok\":true}\n200"),
        calls: RefCell::default(),
    };
    let token = || Some(SENTINEL.to_owned());
    assert!(slack(&curl, &token).is_ok());
    let (argv, stdin) = curl.calls.borrow()[0].clone();
    assert!(stdin.contains(SENTINEL), "the token goes in on stdin");
    assert!(argv.iter().all(|a| !a.contains(SENTINEL)), "never in argv");
}

#[test]
fn slack_dm_never_leaks_the_token() {
    type Answer = fn() -> io::Result<ChildOutput>;
    let answers: [(Answer, &str); 6] = [
        (|| exits(28, ""), "timeout"),
        (|| exits(7, ""), "transport"),
        (|| exits(0, "{\"ok\":false}\n500"), "http"),
        (
            || {
                exits(
                    0,
                    "{\"ok\":false,\"error\":\"xoxb-SENTINEL-0000-never-log-me\"}\n200",
                )
            },
            "slack",
        ),
        (|| exits(0, "not json\n200"), "transport"),
        (
            || Err(io::Error::other(format!("spawn failed {SENTINEL}"))),
            "transport",
        ),
    ];
    let token = || Some(SENTINEL.to_owned());
    let mut seen = Vec::new();
    for (answer, kind) in answers {
        let curl = FakeCurl {
            answer,
            calls: RefCell::default(),
        };
        let err = slack(&curl, &token).expect_err(kind);
        assert_eq!(err.kind(), kind, "{err}");
        // Through the push step: alert detail and log lines.
        let fail = FakeSlack(SlackDm {
            curl: &curl,
            token: &token,
            url: "https://slack.invalid/".to_owned(),
            timeout: Duration::from_secs(1),
        });
        let r = route(Some("slack"), &fail, false);
        let mut other = Map::new();
        at(&mut other, &r, T0);
        let (alerts, lines) = at(&mut other, &r, T0 + PUSH_AFTER);
        seen.push(err.to_string());
        seen.extend(lines);
        seen.extend(alerts.iter().map(|a| format!("{a:?}")));
        seen.push(format!("{other:?}"));
    }
    let curl = FakeCurl {
        answer: || exits(0, "{\"ok\":true}\n200"),
        calls: RefCell::default(),
    };
    let missing = slack(&curl, &|| None).expect_err("missing");
    assert_eq!(missing.kind(), "token");
    let bad = slack(&curl, &|| Some("xoxb-\"; evil".to_owned())).expect_err("bad");
    assert_eq!(bad.kind(), "token-format");
    assert!(
        curl.calls.borrow().is_empty(),
        "no curl without a usable token"
    );
    for text in &seen {
        assert!(!text.contains(SENTINEL), "token leaked: {text}");
    }
}

/// A [`SlackDm`] behind the sender trait object the route takes.
struct FakeSlack<'a>(SlackDm<'a>);

impl PushSender for FakeSlack<'_> {
    fn send(&self, recipient: &str, text: &str) -> Result<(), PushError> {
        self.0.send(recipient, text)
    }
}
