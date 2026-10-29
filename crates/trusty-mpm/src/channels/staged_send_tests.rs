//! Tests for `staged_send.rs` (#10417).

use std::cell::RefCell;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use trusty_common::credentials::Secret;

use super::orgs::{OrgRoute, OrgsConfig};
use super::orgs_tests::INITIAL;
use super::slack_bot::{Posted, SendFailure, SlackPost};
use super::staged_send::*;

const TEXT: &str = "hello from the architect";

#[derive(Default)]
struct Audit(RefCell<Vec<AuditLine>>);
impl SendAudit for Audit {
    fn record(&self, line: AuditLine) {
        self.0.borrow_mut().push(line);
    }
}

struct Sender {
    sent: RefCell<Vec<(String, String)>>,
    reply: Result<(), SendFailure>,
}
impl Sender {
    fn new(reply: Result<(), SendFailure>) -> Self {
        Self {
            sent: RefCell::new(Vec::new()),
            reply,
        }
    }
}
impl ChannelSender for Sender {
    fn send(&self, route: &OrgRoute, text: &str) -> Result<(), SendFailure> {
        self.sent
            .borrow_mut()
            .push((route.address.clone(), text.to_owned()));
        self.reply.clone()
    }
}

fn staged() -> (OrgsConfig, StagedSends, u64, Instant) {
    let cfg = OrgsConfig::parse(INITIAL).expect("parses");
    let mut table = StagedSends::new(Duration::from_secs(60));
    let now = Instant::now();
    let id = table
        .stage(&cfg, "bob-duetto", None, TEXT, now, &Audit::default())
        .expect("staged");
    (cfg, table, id, now)
}

fn approve() -> Decision {
    Decision::Approve {
        digest: digest_of(TEXT),
    }
}

#[test]
fn approve_posts_exact_text_once() {
    let (_, mut table, id, now) = staged();
    let sender = Sender::new(Ok(()));
    let audit = Audit::default();
    let state = table.decide(id, &approve(), now, &sender, &audit);
    assert_eq!(state, Ok(SendState::Sent));
    assert_eq!(
        table.decide(id, &approve(), now, &sender, &audit),
        Err(SendError::AlreadyDecided)
    );
    assert_eq!(
        *sender.sent.borrow(),
        vec![("U0OWNER01".to_owned(), TEXT.to_owned())]
    );
    for l in audit.0.borrow().iter() {
        assert!(!l.code.contains(TEXT), "audit carries no text: {l:?}");
    }
}

#[test]
fn deny_timeout_and_free_text_post_nothing() {
    let sender = Sender::new(Ok(()));
    let audit = Audit::default();
    let (_, mut t, id, now) = staged();
    assert_eq!(
        t.decide(id, &Decision::FreeText, now, &sender, &audit),
        Ok(SendState::Staged)
    );
    assert_eq!(
        t.decide(id, &Decision::Deny, now, &sender, &audit),
        Ok(SendState::Denied)
    );
    let (_, mut t, id, now) = staged();
    let late = now + Duration::from_secs(61);
    assert_eq!(
        t.decide(id, &approve(), late, &sender, &audit),
        Ok(SendState::Expired)
    );
    assert!(sender.sent.borrow().is_empty());
}

#[test]
fn approve_with_wrong_digest_posts_nothing() {
    let (_, mut t, id, now) = staged();
    let sender = Sender::new(Ok(()));
    let bad = Decision::Approve {
        digest: digest_of("some other text"),
    };
    assert_eq!(
        t.decide(id, &bad, now, &sender, &Audit::default()),
        Err(SendError::DigestMismatch)
    );
    assert!(sender.sent.borrow().is_empty());
    assert_eq!(t.get(id).map(|s| &s.state), Some(&SendState::Staged));
}

#[test]
fn unknown_outcome_is_never_reissued() {
    let (_, mut t, id, now) = staged();
    let sender = Sender::new(Err(SendFailure::Unknown("timeout".into())));
    let audit = Audit::default();
    assert_eq!(
        t.decide(id, &approve(), now, &sender, &audit),
        Ok(SendState::Unknown("timeout".into()))
    );
    assert_eq!(
        t.decide(id, &approve(), now, &sender, &audit),
        Err(SendError::AlreadyDecided)
    );
    assert_eq!(sender.sent.borrow().len(), 1);
}

#[test]
fn cross_org_stage_is_refused_and_audited_without_text() {
    let cfg = OrgsConfig::parse(INITIAL).expect("parses");
    let mut t = StagedSends::default();
    let audit = Audit::default();
    let err = t
        .stage(
            &cfg,
            "masa@travelbta.com",
            Some("duetto"),
            TEXT,
            Instant::now(),
            &audit,
        )
        .expect_err("refused");
    assert!(!err.to_string().contains(TEXT));
    let lines = audit.0.borrow();
    assert_eq!(lines.len(), 1);
    assert_eq!(
        (lines[0].event, lines[0].code.as_str()),
        ("refused", "cross_org")
    );
}

#[test]
fn no_route_stage_is_denied() {
    let cfg = OrgsConfig::parse(INITIAL).expect("parses");
    let mut t = StagedSends::default();
    let err = t
        .stage(
            &cfg,
            "stranger",
            None,
            TEXT,
            Instant::now(),
            &Audit::default(),
        )
        .expect_err("refused");
    assert!(matches!(err, SendError::Route(_)));
    assert!(t.get(1).is_none());
}

struct Poster(Mutex<Vec<String>>);
impl SlackPost for Poster {
    fn post(&self, _t: &Secret<String>, channel: &str, _x: &str) -> Result<Posted, SendFailure> {
        self.0.lock().expect("lock").push(channel.to_owned());
        Ok(Posted {
            channel: "D1".into(),
            ts: "1.0".into(),
        })
    }
}

struct Resolver(Result<(), String>);
impl SecretResolver for Resolver {
    fn resolve(&self, r: &str) -> Result<Secret<String>, String> {
        assert!(r.starts_with("secret://"));
        self.0.clone().map(|()| Secret::new("tok".to_owned()))
    }
}

#[test]
fn slack_dm_posts_to_the_member_id() {
    let cfg = OrgsConfig::parse(INITIAL).expect("parses");
    let poster = Poster(Mutex::new(Vec::new()));
    let resolver = Resolver(Ok(()));
    let sender = SlackDmSender {
        poster: &poster,
        resolver: &resolver,
    };
    let route = cfg.route("bob-duetto", None).expect("route");
    assert_eq!(sender.send(&route, TEXT), Ok(()));
    assert_eq!(
        *poster.0.lock().expect("lock"),
        vec!["U0OWNER01".to_owned()]
    );
    let gchat = cfg.route("masa@travelbta.com", None).expect("route");
    assert!(matches!(
        sender.send(&gchat, TEXT),
        Err(SendFailure::Rejected(_))
    ));
}

#[test]
fn credential_error_posts_nothing() {
    // Fail-Open regression: a resolver error must not post or read as sent.
    let (_, mut t, id, now) = staged();
    let poster = Poster(Mutex::new(Vec::new()));
    let resolver = Resolver(Err("SECRET-VALUE-LEAK".into()));
    let sender = SlackDmSender {
        poster: &poster,
        resolver: &resolver,
    };
    let audit = Audit::default();
    let state = t.decide(id, &approve(), now, &sender, &audit);
    assert_eq!(
        state,
        Ok(SendState::NotSent("credential_unavailable".into()))
    );
    assert!(poster.0.lock().expect("lock").is_empty());
    assert!(audit.0.borrow().iter().all(|l| !l.code.contains("SECRET")));
}
