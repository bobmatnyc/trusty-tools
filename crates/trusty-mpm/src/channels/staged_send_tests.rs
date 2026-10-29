//! Tests for `staged_send.rs` (#10417).

use std::cell::RefCell;
use std::sync::Mutex;
use std::time::Duration;

use trusty_common::credentials::Secret;

use super::orgs::{OrgRoute, OrgsConfig};
use super::orgs_tests::INITIAL;
use super::slack_bot::{Posted, SendFailure, SlackPost};
use super::staged_send::*;

pub(crate) const TEXT: &str = "hello from the architect";

/// Records lines; `fail` refuses every write (#10423).
#[derive(Default)]
pub(crate) struct Audit(pub(crate) RefCell<Vec<AuditLine>>, pub(crate) bool);
impl SendAudit for Audit {
    fn record(&self, line: AuditLine) -> Result<(), WriteFailed> {
        if self.1 {
            return Err(WriteFailed("disk full".into()));
        }
        self.0.borrow_mut().push(line);
        Ok(())
    }
}

/// Counts saves; `fail_after` refuses every save past that many (#10423).
#[derive(Default)]
pub(crate) struct Store(pub(crate) RefCell<usize>, pub(crate) Option<usize>);
impl SendStore for Store {
    fn save(&self, _t: &StagedSends) -> Result<(), WriteFailed> {
        let mut n = self.0.borrow_mut();
        if self.1.is_some_and(|max| *n >= max) {
            return Err(WriteFailed("read-only".into()));
        }
        *n += 1;
        Ok(())
    }
}

pub(crate) fn io<'a>(audit: &'a Audit, store: &'a dyn SendStore) -> SendIo<'a> {
    SendIo { audit, store }
}

pub(crate) fn owner(cfg: &OrgsConfig) -> OwnerApproval {
    OwnerApproval::authenticate(cfg, "duetto", "U0OWNER01").expect("owner")
}

pub(crate) struct Sender {
    pub(crate) sent: RefCell<Vec<(String, String)>>,
    reply: Result<(), SendFailure>,
}
impl Sender {
    pub(crate) fn new(reply: Result<(), SendFailure>) -> Self {
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

pub(crate) fn staged() -> (OrgsConfig, StagedSends, u64, u64) {
    let cfg = OrgsConfig::parse(INITIAL).expect("parses");
    let mut table = StagedSends::new(Duration::from_secs(60));
    let now = 1_000;
    let (a, s) = (Audit::default(), Store::default());
    let id = table
        .stage(&cfg, "bob-duetto", None, TEXT, now, &io(&a, &s))
        .expect("staged");
    (cfg, table, id, now)
}

pub(crate) fn approve() -> Decision {
    Decision::Approve {
        digest: digest_of(TEXT),
    }
}

#[test]
fn approve_posts_exact_text_once() {
    let (cfg, mut table, id, now) = staged();
    let sender = Sender::new(Ok(()));
    let audit = Audit::default();
    let state = table.decide(
        id,
        &owner(&cfg),
        &approve(),
        now,
        &sender,
        &io(&audit, &Store::default()),
    );
    assert_eq!(state, Ok(SendState::Sent));
    assert_eq!(
        table.decide(
            id,
            &owner(&cfg),
            &approve(),
            now,
            &sender,
            &io(&audit, &Store::default())
        ),
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
    let (cfg, mut t, id, now) = staged();
    assert_eq!(
        t.decide(
            id,
            &owner(&cfg),
            &Decision::FreeText,
            now,
            &sender,
            &io(&audit, &Store::default())
        ),
        Ok(SendState::Staged)
    );
    assert_eq!(
        t.decide(
            id,
            &owner(&cfg),
            &Decision::Deny,
            now,
            &sender,
            &io(&audit, &Store::default())
        ),
        Ok(SendState::Denied)
    );
    let (cfg, mut t, id, now) = staged();
    let late = now + 61;
    assert_eq!(
        t.decide(
            id,
            &owner(&cfg),
            &approve(),
            late,
            &sender,
            &io(&audit, &Store::default())
        ),
        Ok(SendState::Expired)
    );
    assert!(sender.sent.borrow().is_empty());
}

#[test]
fn approve_with_wrong_digest_posts_nothing() {
    let (cfg, mut t, id, now) = staged();
    let sender = Sender::new(Ok(()));
    let bad = Decision::Approve {
        digest: digest_of("some other text"),
    };
    assert_eq!(
        t.decide(
            id,
            &owner(&cfg),
            &bad,
            now,
            &sender,
            &io(&Audit::default(), &Store::default())
        ),
        Err(SendError::DigestMismatch)
    );
    assert!(sender.sent.borrow().is_empty());
    assert_eq!(t.get(id).map(|s| &s.state), Some(&SendState::Staged));
}

#[test]
fn unknown_outcome_is_never_reissued() {
    let (cfg, mut t, id, now) = staged();
    let sender = Sender::new(Err(SendFailure::Unknown("timeout".into())));
    let audit = Audit::default();
    assert_eq!(
        t.decide(
            id,
            &owner(&cfg),
            &approve(),
            now,
            &sender,
            &io(&audit, &Store::default())
        ),
        Ok(SendState::Unknown("timeout".into()))
    );
    assert_eq!(
        t.decide(
            id,
            &owner(&cfg),
            &approve(),
            now,
            &sender,
            &io(&audit, &Store::default())
        ),
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
            "owner@example.com",
            Some("duetto"),
            TEXT,
            1_000,
            &io(&audit, &Store::default()),
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
            1_000,
            &io(&Audit::default(), &Store::default()),
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
    fn resolve(&self, _s: Option<&str>, r: &str) -> Result<Secret<String>, String> {
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
    let gchat = cfg.route("owner@example.com", None).expect("route");
    assert!(matches!(
        sender.send(&gchat, TEXT),
        Err(SendFailure::Rejected(_))
    ));
}

#[test]
fn credential_error_posts_nothing() {
    // Fail-Open regression: a resolver error must not post or read as sent.
    let (cfg, mut t, id, now) = staged();
    let poster = Poster(Mutex::new(Vec::new()));
    let resolver = Resolver(Err("SECRET-VALUE-LEAK".into()));
    let sender = SlackDmSender {
        poster: &poster,
        resolver: &resolver,
    };
    let audit = Audit::default();
    let state = t.decide(
        id,
        &owner(&cfg),
        &approve(),
        now,
        &sender,
        &io(&audit, &Store::default()),
    );
    assert_eq!(
        state,
        Ok(SendState::NotSent("credential_unavailable".into()))
    );
    assert!(poster.0.lock().expect("lock").is_empty());
    assert!(audit.0.borrow().iter().all(|l| !l.code.contains("SECRET")));
}

#[test]
fn debug_output_redacts_the_staged_text() {
    let (_cfg, table, id, _now) = staged();
    let one = format!("{:?}", table.get(id).expect("staged"));
    let all = format!("{table:?}");
    for out in [&one, &all] {
        assert!(!out.contains(TEXT), "text leaked: {out}");
        assert!(out.contains("<redacted>"), "no marker: {out}");
        assert!(
            out.contains(&digest_of(TEXT)[..12]),
            "no digest prefix: {out}"
        );
    }
}

/// #10423 Fail-Open regression: an audit write failure stages nothing.
#[test]
fn audit_failure_refuses_the_stage() {
    let cfg = OrgsConfig::parse(INITIAL).expect("parses");
    let mut t = StagedSends::default();
    let audit = Audit(RefCell::default(), true);
    let err = t
        .stage(
            &cfg,
            "bob-duetto",
            None,
            TEXT,
            1_000,
            &io(&audit, &Store::default()),
        )
        .expect_err("refused");
    assert_eq!(err.code(), "audit_unavailable");
    assert!(t.get(1).is_none());
}

/// #10423 Fail-Open regression: the gating `sending` line cannot be written,
/// so nothing posts and the send stays `Staged`.
#[test]
fn audit_failure_before_send_posts_nothing() {
    let (cfg, mut t, id, now) = staged();
    let sender = Sender::new(Ok(()));
    let audit = Audit(RefCell::default(), true);
    let err = t
        .decide(
            id,
            &owner(&cfg),
            &approve(),
            now,
            &sender,
            &io(&audit, &Store::default()),
        )
        .expect_err("refused");
    assert_eq!(err.code(), "audit_unavailable");
    assert!(sender.sent.borrow().is_empty());
    assert_eq!(t.get(id).map(|s| &s.state), Some(&SendState::Staged));
}

/// #10423 Fail-Open regression: `Sending` cannot be persisted, so nothing
/// posts; a stage that cannot be persisted is withdrawn.
#[test]
fn persist_failure_before_send_posts_nothing() {
    let (cfg, mut t, id, now) = staged();
    let sender = Sender::new(Ok(()));
    let audit = Audit::default();
    let store = Store(RefCell::default(), Some(0));
    let err = t
        .decide(
            id,
            &owner(&cfg),
            &approve(),
            now,
            &sender,
            &io(&audit, &store),
        )
        .expect_err("refused");
    assert_eq!(err.code(), "persist_failed");
    assert!(sender.sent.borrow().is_empty());
    assert_eq!(t.get(id).map(|s| &s.state), Some(&SendState::Staged));
    let err = t
        .stage(&cfg, "bob-duetto", None, TEXT, now, &io(&audit, &store))
        .expect_err("refused");
    assert_eq!(err.code(), "persist_failed");
    assert!(t.get(id + 1).is_none());
}

/// #10423 Fail-Open regression: only the configured owner may decide.
#[test]
fn unauthenticated_approve_posts_nothing() {
    let cfg = OrgsConfig::parse(INITIAL).expect("parses");
    for (org, user) in [("duetto", "U0OTHER01"), ("bta", "U0OWNER01"), ("", "")] {
        assert_eq!(
            OwnerApproval::authenticate(&cfg, org, user),
            Err(SendError::ApproverUnauthenticated)
        );
    }
    let mut no_owner = cfg.clone();
    no_owner.owner = None;
    assert_eq!(
        OwnerApproval::authenticate(&no_owner, "duetto", "U0OWNER01"),
        Err(SendError::ApproverUnauthenticated)
    );
    assert_eq!(owner(&cfg).identity(), ("duetto", "U0OWNER01"));
}
