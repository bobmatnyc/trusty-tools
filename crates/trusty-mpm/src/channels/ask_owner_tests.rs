//! Tests for `ask_owner` (#9856, #8433 P4) and the Slack bot it posts with.
//!
//! Why: `ask_owner` is a Fail-Open Check site. Each test goes red against a
//! stub that proceeds: a refusal that reaches the transport, an id that is
//! reused, a send reported as done before its question is open.
//! What: a real question table and a real channel audit under a hermetic
//! temp home; the policy, bot ref, token and transport are a counting fake.
//! Test: this is the test module.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier, Mutex};

use trusty_channels::policy::{
    Channel, ChannelPolicy, DenyReason, MessageKind, PolicySpec, Route, RouteSpec,
};
use trusty_common::credentials::Secret;

use super::{
    ASK_KIND, AskEnv, AskError, AskOwnerRequest, QuestionStore, ask_owner, is_slack_user_id,
};
use crate::channels::audit_log::{ChannelAudit, audit_path};
use crate::channels::question_table::{
    Ask, Question, QuestionTable, QuestionTableError, questions_path,
};
use crate::channels::slack_bot::{Posted, SendFailure};
use crate::channels::snapshot::{Refusal, SnapshotDeny};

const OWNER: &str = "U0OWNER1";
const OTHER: &str = "U0OTHER2";
const ASKER: &str = "6f1c1d0e-0000-4000-8000-000000000001";

/// How the fake's question table behaves.
enum TableMode {
    Real,
    OpenFails,
    ReserveFails,
    OpenQuestionFails,
}

/// A counting fake around a real table and a real audit log.
struct Fake {
    home: PathBuf,
    policy: ChannelPolicy,
    snapshot: Option<SnapshotDeny>,
    deny: Option<DenyReason>,
    allow_any: bool,
    bot_ref: Result<String, String>,
    token_fails: bool,
    table_mode: TableMode,
    table: Mutex<Option<Arc<QuestionTable>>>,
    reply: Mutex<Result<Posted, SendFailure>>,
    audit_home: Option<PathBuf>,
    posts: AtomicUsize,
    kinds: Mutex<Vec<MessageKind>>,
    resolved: Mutex<Vec<String>>,
}

impl Fake {
    fn new(home: &Path) -> Self {
        Self {
            home: home.to_path_buf(),
            policy: policy(&[OWNER]),
            snapshot: None,
            deny: None,
            allow_any: false,
            bot_ref: Ok("slack".into()),
            token_fails: false,
            table_mode: TableMode::Real,
            table: Mutex::new(None),
            reply: Mutex::new(Ok(posted("1700000000.000100"))),
            audit_home: Some(home.to_path_buf()),
            posts: AtomicUsize::new(0),
            kinds: Mutex::new(Vec::new()),
            resolved: Mutex::new(Vec::new()),
        }
    }

    fn real_table(&self) -> Arc<QuestionTable> {
        let mut slot = self.table.lock().expect("table slot");
        Arc::clone(slot.get_or_insert_with(|| {
            Arc::new(QuestionTable::open_at(&questions_path(&self.home)).expect("open table"))
        }))
    }

    fn posts(&self) -> usize {
        self.posts.load(Ordering::SeqCst)
    }
}

/// A store whose `reserve` or `open` fails, over a real table.
struct Faulty {
    inner: Arc<QuestionTable>,
    reserve_fails: bool,
}

impl QuestionStore for Faulty {
    fn reserve(&self) -> Result<u64, QuestionTableError> {
        if self.reserve_fails {
            return Err(QuestionTableError::Poisoned);
        }
        self.inner.reserve()
    }

    fn open(&self, _id: u64, _ask: Ask) -> Result<Question, QuestionTableError> {
        Err(QuestionTableError::Poisoned)
    }
}

impl AskEnv for Fake {
    fn check_egress(
        &self,
        recipient: &str,
        kind: MessageKind,
    ) -> Result<Route, Refusal<DenyReason>> {
        self.kinds.lock().expect("kinds").push(kind);
        if let Some(deny) = self.snapshot {
            return Err(Refusal::Snapshot(deny));
        }
        if let Some(deny) = self.deny {
            return Err(Refusal::Policy(deny));
        }
        if self.allow_any {
            return Ok(self.policy.routes()[0].clone());
        }
        match self.policy.check_egress(Channel::Slack, recipient, kind) {
            trusty_channels::policy::EgressDecision::Allow(route) => Ok(route.clone()),
            trusty_channels::policy::EgressDecision::Deny(reason) => Err(Refusal::Policy(reason)),
        }
    }

    fn bot_ref(&self) -> Result<String, String> {
        self.bot_ref.clone()
    }

    fn bot_token(&self, bot_ref: &str) -> Result<Secret<String>, String> {
        self.resolved
            .lock()
            .expect("resolved")
            .push(bot_ref.to_owned());
        if self.token_fails {
            return Err("no value in any tier".into());
        }
        Ok(Secret::new(
            "xoxb-000000000000-SYNTHETIC-NOTREAL".to_owned(),
        ))
    }

    fn table(&self) -> Result<Arc<dyn QuestionStore>, String> {
        match self.table_mode {
            TableMode::Real => Ok(self.real_table()),
            TableMode::OpenFails => Err("held by another question table".into()),
            TableMode::ReserveFails => Ok(Arc::new(Faulty {
                inner: self.real_table(),
                reserve_fails: true,
            })),
            TableMode::OpenQuestionFails => Ok(Arc::new(Faulty {
                inner: self.real_table(),
                reserve_fails: false,
            })),
        }
    }

    fn post(
        &self,
        _token: &Secret<String>,
        _recipient: &str,
        _text: &str,
    ) -> Result<Posted, SendFailure> {
        self.posts.fetch_add(1, Ordering::SeqCst);
        self.reply.lock().expect("reply").clone()
    }

    fn audit(&self, to: &str, reason: &'static str) -> Result<(), String> {
        ChannelAudit::for_home(self.audit_home.as_deref())
            .send_refused(Channel::Slack, to, ASK_KIND, reason)
            .map_err(|e| e.to_string())
    }
}

fn policy(recipients: &[&str]) -> ChannelPolicy {
    let routes = recipients
        .iter()
        .enumerate()
        .map(|(i, r)| RouteSpec {
            channel: Channel::Slack,
            name: format!("owner-{i}"),
            recipient: (*r).to_owned(),
            kinds: vec![MessageKind::Question],
            rate_limit: None,
        })
        .collect();
    ChannelPolicy::build(PolicySpec {
        rate_limit: None,
        routes,
    })
    .expect("policy builds")
}

fn posted(ts: &str) -> Posted {
    Posted {
        channel: "D0OWNERDM".into(),
        ts: ts.into(),
    }
}

fn temp_home() -> (tempfile::TempDir, PathBuf) {
    let tmp = crate::test_support::hermetic_temp_dir();
    let home = std::fs::canonicalize(tmp.path()).expect("canonical");
    (tmp, home)
}

fn req(recipient: &str) -> AskOwnerRequest {
    AskOwnerRequest {
        recipient: recipient.into(),
        question: "Merge #9856 now?".into(),
    }
}

fn asker() -> Result<String, String> {
    Ok(ASKER.into())
}

/// Every audit line under `home`, decoded.
fn audit_lines(home: &Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(audit_path(home))
        .unwrap_or_default()
        .lines()
        .map(|l| serde_json::from_str(l).expect("audit line is JSON"))
        .collect()
}

/// Every `op` in the question log under `home`.
fn table_ops(home: &Path) -> Vec<String> {
    std::fs::read_to_string(questions_path(home))
        .unwrap_or_default()
        .lines()
        .map(|l| {
            let v: serde_json::Value = serde_json::from_str(l).expect("log line");
            v["op"].as_str().unwrap_or("").to_owned()
        })
        .collect()
}

/// A refusal: the code, no post, nothing reserved, one audit line naming it.
fn assert_refused(fake: &Fake, got: Result<super::Asked, AskError>, code: &str) {
    let err = got.expect_err("the ask must be refused");
    assert_eq!(err.code(), code, "{err}");
    assert!(
        err.to_string().starts_with(code),
        "error names the rule: {err}"
    );
    assert_eq!(fake.posts(), 0, "a refusal reached the transport");
    assert!(
        table_ops(&fake.home).is_empty(),
        "a refusal touched the table"
    );
    let lines = audit_lines(&fake.home);
    assert_eq!(lines.len(), 1, "one audit line: {lines:?}");
    assert_eq!(lines[0]["event"], "send_refused");
    assert_eq!(lines[0]["reason"], code);
}

#[test]
fn ask_owner_to_unnamed_recipient_refused_before_any_slack_call() {
    let (_tmp, home) = temp_home();
    let mut fake = Fake::new(&home);
    fake.policy = ChannelPolicy::default();
    let got = ask_owner(&fake, asker(), &req(OWNER));
    assert_refused(&fake, got, "no_policy");
}

#[test]
fn ask_owner_non_owner_recipient_refused() {
    let (_tmp, home) = temp_home();
    let fake = Fake::new(&home);
    let got = ask_owner(&fake, asker(), &req(OTHER));
    assert_refused(&fake, got, "no_route");
}

#[test]
fn ask_owner_kind_not_allowed_refused() {
    let (_tmp, home) = temp_home();
    let mut fake = Fake::new(&home);
    fake.deny = Some(DenyReason::KindNotAllowed);
    let got = ask_owner(&fake, asker(), &req(OWNER));
    assert_refused(&fake, got, "kind_not_allowed");
    assert_eq!(*fake.kinds.lock().expect("kinds"), [MessageKind::Question]);
}

#[test]
fn ask_owner_with_stale_snapshot_refused() {
    let cases = [
        (
            SnapshotDeny::Stale {
                age: std::time::Duration::from_secs(61),
                max: std::time::Duration::from_secs(60),
            },
            "snapshot_stale",
        ),
        (SnapshotDeny::NeverLoaded, "snapshot_never_loaded"),
        (SnapshotDeny::Denied, "snapshot_denied"),
        (SnapshotDeny::Poisoned, "snapshot_poisoned"),
    ];
    for (deny, code) in cases {
        let (_tmp, home) = temp_home();
        let mut fake = Fake::new(&home);
        fake.snapshot = Some(deny);
        let got = ask_owner(&fake, asker(), &req(OWNER));
        assert_refused(&fake, got, code);
    }
}

#[test]
fn ask_owner_resolves_no_user_token() {
    let (_tmp, home) = temp_home();
    let mut fake = Fake::new(&home);
    fake.bot_ref = Ok("slack-user".into());
    let got = ask_owner(&fake, asker(), &req(OWNER));
    assert_refused(&fake, got, "user_token_refused");
    assert!(
        fake.resolved.lock().expect("resolved").is_empty(),
        "a slack-user ref reached the resolver"
    );
    // The allowed path resolves the bot key and nothing else.
    let (_tmp2, home2) = temp_home();
    let fake = Fake::new(&home2);
    ask_owner(&fake, asker(), &req(OWNER)).expect("ask");
    assert_eq!(*fake.resolved.lock().expect("resolved"), ["slack"]);
}

#[test]
fn ask_owner_group_dm_or_channel_target_refused() {
    for target in [
        "C0CHANNEL1",
        "G0GROUPDM1",
        "D0DIRECT1",
        "W0ENTERPRISE",
        "u0lower",
        "",
    ] {
        let (_tmp, home) = temp_home();
        let mut fake = Fake::new(&home);
        // #9856 Q6: refused even when a route would allow it.
        fake.allow_any = true;
        let got = ask_owner(&fake, asker(), &req(target));
        assert_refused(&fake, got, "recipient_not_user_id");
        assert!(fake.kinds.lock().expect("kinds").is_empty(), "{target}");
    }
}

#[test]
fn ask_owner_opens_question_before_send_returns() {
    let (_tmp, home) = temp_home();
    let fake = Fake::new(&home);
    let asked = ask_owner(&fake, asker(), &req(OWNER)).expect("ask");
    assert_eq!(fake.posts(), 1);
    let question = fake
        .real_table()
        .get(asked.question_id)
        .expect("the question is open when ask_owner returns");
    assert_eq!(question.ask.asker_managed_session, ASKER);
    assert_eq!(question.ask.recipient, OWNER);
    assert_eq!(question.ask.route, "owner-0");
    assert_eq!(question.ask.conversation.dm_channel_id, "D0OWNERDM");
    assert_eq!(question.ask.conversation.thread_ts, asked.thread_ts);
    assert_eq!(
        table_ops(&home),
        ["reserve", "open"],
        "one reserve, one open"
    );
    assert!(audit_lines(&home).is_empty(), "a success writes no refusal");
}

#[test]
fn ask_owner_refuses_an_unproven_asker() {
    let (_tmp, home) = temp_home();
    let fake = Fake::new(&home);
    let got = ask_owner(&fake, Err("no kernel peer".into()), &req(OWNER));
    assert_refused(&fake, got, "asker_unproven");
    assert!(fake.kinds.lock().expect("kinds").is_empty());
}

#[test]
fn ask_owner_refuses_when_table_cannot_open() {
    let (_tmp, home) = temp_home();
    let mut fake = Fake::new(&home);
    fake.table_mode = TableMode::OpenFails;
    let got = ask_owner(&fake, asker(), &req(OWNER));
    assert_refused(&fake, got, "table_unavailable");
}

#[test]
fn ask_owner_refuses_when_reserve_fails() {
    let (_tmp, home) = temp_home();
    let mut fake = Fake::new(&home);
    fake.table_mode = TableMode::ReserveFails;
    let got = ask_owner(&fake, asker(), &req(OWNER));
    assert_refused(&fake, got, "reserve_failed");
}

#[test]
fn ask_owner_refuses_when_token_cannot_resolve() {
    let (_tmp, home) = temp_home();
    let mut fake = Fake::new(&home);
    fake.token_fails = true;
    let got = ask_owner(&fake, asker(), &req(OWNER));
    assert_refused(&fake, got, "token_unavailable");
    let (_tmp2, home2) = temp_home();
    let mut fake = Fake::new(&home2);
    fake.bot_ref = Err("slack is not enabled in the host ceiling".into());
    let got = ask_owner(&fake, asker(), &req(OWNER));
    assert_refused(&fake, got, "no_bot_ref");
}

#[test]
fn ask_owner_unknown_send_outcome_keeps_id_reserved() {
    let (_tmp, home) = temp_home();
    let fake = Fake::new(&home);
    *fake.reply.lock().expect("reply") = Err(SendFailure::Unknown("timeout".into()));
    let err = ask_owner(&fake, asker(), &req(OWNER)).expect_err("unknown outcome");
    let AskError::SendOutcomeUnknown { id: first, .. } = err else {
        panic!("expected send_outcome_unknown, got {err}");
    };
    assert_eq!(fake.posts(), 1);
    assert!(
        fake.real_table().get(first).is_none(),
        "an unknown send is not open"
    );
    assert_eq!(table_ops(&home), ["reserve"]);
    assert_eq!(audit_lines(&home)[0]["reason"], "send_outcome_unknown");
    // The next ask takes a new id; the unknown one is never reissued.
    *fake.reply.lock().expect("reply") = Ok(posted("1700000000.000200"));
    let next = ask_owner(&fake, asker(), &req(OWNER)).expect("ask");
    assert_ne!(
        next.question_id, first,
        "an id with an unknown outcome was reused"
    );
    // A rejected send also keeps its id.
    *fake.reply.lock().expect("reply") = Err(SendFailure::Rejected("channel_not_found".into()));
    let err = ask_owner(&fake, asker(), &req(OWNER)).expect_err("rejected");
    assert_eq!(err.code(), "send_rejected");
    assert_eq!(table_ops(&home), ["reserve", "reserve", "open", "reserve"]);
}

#[test]
fn ask_owner_open_failure_after_send_reports_sent_unrecorded() {
    let (_tmp, home) = temp_home();
    let mut fake = Fake::new(&home);
    fake.table_mode = TableMode::OpenQuestionFails;
    let err = ask_owner(&fake, asker(), &req(OWNER)).expect_err("unrecorded");
    assert_eq!(err.code(), "sent_unrecorded", "{err}");
    assert!(err.to_string().contains("was posted"), "{err}");
    assert_eq!(fake.posts(), 1);
    let lines = audit_lines(&home);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0]["reason"], "sent_unrecorded");
    assert_eq!(table_ops(&home), ["reserve"]);
}

#[test]
fn ask_owner_refusal_stands_when_audit_write_fails() {
    let (_tmp, home) = temp_home();
    let mut fake = Fake::new(&home);
    fake.audit_home = None;
    let err = ask_owner(&fake, asker(), &req(OTHER)).expect_err("refused");
    assert_eq!(err.code(), "no_route");
    assert_eq!(fake.posts(), 0);
    assert!(table_ops(&home).is_empty());
}

/// #9856: two askers racing on one table never share a question id, and
/// every success leaves exactly one `reserve` and one `open`.
#[test]
fn concurrent_asks_take_distinct_ids_n_rounds() {
    const ROUNDS: usize = 200;
    let (_tmp, home) = temp_home();
    let fake = Arc::new(Fake::new(&home));
    let mut ids = Vec::new();
    for _ in 0..ROUNDS {
        let barrier = Arc::new(Barrier::new(2));
        let handles: Vec<_> = (0..2)
            .map(|_| {
                let (fake, barrier) = (Arc::clone(&fake), Arc::clone(&barrier));
                std::thread::spawn(move || {
                    barrier.wait();
                    ask_owner(fake.as_ref(), asker(), &req(OWNER)).expect("ask")
                })
            })
            .collect();
        for h in handles {
            ids.push(h.join().expect("thread").question_id);
        }
    }
    let mut unique = ids.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), ids.len(), "a question id was issued twice");
    let ops = table_ops(&home);
    assert_eq!(ops.iter().filter(|o| *o == "reserve").count(), 2 * ROUNDS);
    assert_eq!(ops.iter().filter(|o| *o == "open").count(), 2 * ROUNDS);
}

#[test]
fn slack_user_id_shape() {
    assert!(is_slack_user_id(OWNER));
    for not in [
        "", "U", "C0ABC", "G0ABC", "D0ABC", "W0ABC", "U0abc", "U0 AB",
    ] {
        assert!(!is_slack_user_id(not), "{not:?}");
    }
}

mod slack_bot_tests {
    use crate::channels::slack_bot::{
        Posted, SendFailure, bot_ref, check_bot_ref, classify, resolve_bot_token,
    };

    #[test]
    fn classify_maps_status_and_body() {
        let ok = r#"{"ok":true,"channel":"D0OWNERDM","ts":"1700000000.000100"}"#;
        assert_eq!(
            classify(200, ok),
            Ok(Posted {
                channel: "D0OWNERDM".into(),
                ts: "1700000000.000100".into()
            })
        );
        assert!(matches!(classify(503, ""), Err(SendFailure::Unknown(_))));
        assert!(matches!(classify(500, ok), Err(SendFailure::Unknown(_))));
        assert!(matches!(
            classify(200, "<html>"),
            Err(SendFailure::Unknown(_))
        ));
        assert_eq!(
            classify(429, ""),
            Err(SendFailure::Rejected("http_429".into()))
        );
        assert_eq!(
            classify(200, r#"{"ok":false,"error":"channel_not_found"}"#),
            Err(SendFailure::Rejected("channel_not_found".into()))
        );
        // Slack's server-side error states: the post may have partly landed.
        for code in [
            "fatal_error",
            "internal_error",
            "service_unavailable",
            "request_timeout",
        ] {
            let body = format!(r#"{{"ok":false,"error":"{code}"}}"#);
            assert_eq!(
                classify(200, &body),
                Err(SendFailure::Unknown(code.into())),
                "{code}"
            );
        }
        for code in ["not_authed", "invalid_auth", "user_not_found"] {
            let body = format!(r#"{{"ok":false,"error":"{code}"}}"#);
            assert_eq!(
                classify(200, &body),
                Err(SendFailure::Rejected(code.into())),
                "{code}"
            );
        }
        let channel = r#"{"ok":true,"channel":"C0PUBLIC","ts":"1.2"}"#;
        assert!(matches!(
            classify(200, channel),
            Err(SendFailure::Unrecorded(_))
        ));
        let no_ts = r#"{"ok":true,"channel":"D0OWNERDM"}"#;
        assert!(matches!(
            classify(200, no_ts),
            Err(SendFailure::Unrecorded(_))
        ));
    }

    #[test]
    fn resolve_bot_token_refuses_the_user_token() {
        assert!(check_bot_ref("slack").is_ok());
        let err = resolve_bot_token("slack-user").expect_err("user token refused");
        assert!(err.contains("user token"), "{err}");
        assert!(resolve_bot_token("slack-app").is_err());
    }

    #[test]
    fn bot_ref_is_read_from_the_host_ceiling() {
        let tmp = crate::test_support::hermetic_temp_dir();
        let home = std::fs::canonicalize(tmp.path()).expect("canonical");
        assert!(bot_ref(None).is_err());
        assert!(bot_ref(Some(&home)).is_err(), "no config.yaml");
        let path = trusty_common::crate_config::crate_config_path_at(&home, "trusty-mpm");
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(
            &path,
            "channels:\n  version: 1\n  slack:\n    enabled: true\n    connection: { bot_ref: slack }\n    projects: [/abs]\n",
        )
        .expect("write host");
        assert_eq!(bot_ref(Some(&home)), Ok("slack".to_owned()));
    }
}
