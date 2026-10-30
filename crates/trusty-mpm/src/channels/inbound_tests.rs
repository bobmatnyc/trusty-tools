//! Tests for the inbound decision (#9873, #8433 P5).
//!
//! Why: `decide` is a Fail-Open Check site. Each test goes red against a
//! decision that proceeds: one that binds a reply to the wrong question,
//! resolves for the wrong sender, reads the table for an unknown sender, or
//! admits past the rate window.
//! What: a real question table and a real channel audit under a hermetic
//! temp home; the policy, the snapshot, the limiter clock and the preflight
//! are a counting fake.
//! Test: this is the test module.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use trusty_channels::policy::{
    Channel, ChannelPolicy, Clock, DropReason, InboundDecision, MessageKind, PolicySpec,
    QuestionId, QuestionState, Route, RouteSpec,
};

use super::{
    AnswerTable, InboundEnv, InboundEvent, InboundLimiter, MAX_ANSWER_BYTES, RateBucket, RateTake,
    Verdict, decide,
};
use crate::channels::audit_log::{ChannelAudit, audit_path};
use crate::channels::question_table::{
    Answer, Ask, Conversation, Question, QuestionTable, ResolveOutcome, UnparsedOutcome,
    questions_path,
};
use crate::channels::snapshot::{Refusal, SnapshotDeny};

pub(crate) const OWNER: &str = "U0OWNER1";
pub(crate) const OTHER: &str = "U0OTHER2";
const STRANGER: &str = "U0STRANGER9";
pub(crate) const ASKER: &str = "6f1c1d0e-0000-4000-8000-0000000000a1";
const SECRET_TEXT: &str = "approve-and-the-text-never-reaches-the-audit";

/// A clock a test moves by hand.
#[derive(Clone, Default)]
pub(crate) struct TestClock(pub(crate) Arc<AtomicU64>);

impl Clock for TestClock {
    fn now(&self) -> Duration {
        Duration::from_millis(self.0.load(Ordering::SeqCst))
    }
}

impl TestClock {
    fn advance(&self, by: Duration) {
        let ms = u64::try_from(by.as_millis()).expect("ms fit");
        self.0.fetch_add(ms, Ordering::SeqCst);
    }
}

/// How the fake's table behaves.
#[derive(Clone)]
pub(crate) enum TableMode {
    /// The real table, counted.
    Real,
    /// `table()` fails.
    OpenFails,
    /// The real table, reporting poisoned.
    Poisoned,
    /// The real table, but `resolve` returns this without changing it.
    Resolves(ResolveOutcome),
    /// The real table, but `state` reports every question unanswered, so
    /// only the CAS can catch a second answer.
    StaleState,
    /// The real `resolve` runs, then the caller's task panics (F16).
    PanicsAfterResolve,
}

/// A counting wrapper over the real table.
pub(crate) struct Counting {
    pub(crate) inner: Arc<QuestionTable>,
    pub(crate) resolves: Arc<AtomicUsize>,
    pub(crate) mode: TableMode,
}

impl AnswerTable for Counting {
    fn find_by_conversation(&self, conversation: &Conversation) -> Option<QuestionId> {
        self.inner.find_by_conversation(conversation)
    }

    fn state(&self, id: QuestionId) -> Option<QuestionState> {
        let state = self.inner.state(id)?;
        match self.mode {
            TableMode::StaleState => Some(QuestionState {
                answered: false,
                ..state
            }),
            _ => Some(state),
        }
    }

    fn get(&self, id: QuestionId) -> Option<Question> {
        self.inner.get(id)
    }

    fn answer(&self, id: QuestionId) -> Option<Answer> {
        self.inner.answer(id)
    }

    fn resolve(
        &self,
        id: QuestionId,
        answerer: &str,
        conversation: &Conversation,
        text: &str,
    ) -> ResolveOutcome {
        self.resolves.fetch_add(1, Ordering::SeqCst);
        if let TableMode::Resolves(outcome) = &self.mode {
            return outcome.clone();
        }
        let outcome = self.inner.resolve(id, answerer, conversation, text);
        if matches!(self.mode, TableMode::PanicsAfterResolve) {
            panic!("the decision task dies after resolve: {outcome:?}");
        }
        outcome
    }

    fn mark_unparsed(
        &self,
        id: QuestionId,
        answerer: &str,
        conversation: &Conversation,
        text: &str,
    ) -> UnparsedOutcome {
        self.inner.mark_unparsed(id, answerer, conversation, text)
    }

    fn is_poisoned(&self) -> bool {
        matches!(self.mode, TableMode::Poisoned) || self.inner.is_poisoned()
    }
}

/// A counting fake around a real table and a real audit log.
pub(crate) struct Fake {
    pub(crate) home: PathBuf,
    pub(crate) policy: ChannelPolicy,
    pub(crate) snapshot: Option<SnapshotDeny>,
    pub(crate) table_mode: TableMode,
    pub(crate) table: Arc<QuestionTable>,
    pub(crate) limiter: InboundLimiter,
    pub(crate) clock: TestClock,
    pub(crate) preflight: Result<(), String>,
    pub(crate) audit_fails: bool,
    pub(crate) audits: Mutex<Vec<(&'static str, bool)>>,
    pub(crate) resolves: Arc<AtomicUsize>,
    pub(crate) table_reads: AtomicUsize,
    /// Released together at the preflight, so racing answers meet at `resolve`.
    pub(crate) gate: Option<Arc<std::sync::Barrier>>,
    _tmp: Arc<tempfile::TempDir>,
}

impl Fake {
    pub(crate) fn new() -> Self {
        let tmp = crate::test_support::hermetic_temp_dir();
        let home = std::fs::canonicalize(tmp.path()).expect("canonical home");
        let table = QuestionTable::open_at(&questions_path(&home)).expect("open table");
        let clock = TestClock::default();
        Self {
            home,
            policy: policy(&[OWNER, OTHER]),
            snapshot: None,
            table_mode: TableMode::Real,
            table: Arc::new(table),
            limiter: InboundLimiter::new(Box::new(clock.clone())),
            clock,
            preflight: Ok(()),
            audit_fails: false,
            audits: Mutex::new(Vec::new()),
            resolves: Arc::new(AtomicUsize::new(0)),
            table_reads: AtomicUsize::new(0),
            gate: None,
            _tmp: Arc::new(tmp),
        }
    }

    /// Reserve and open one question of `route` to `recipient` in `conv`,
    /// with the options `approve` and `reject` (#10127: a reply to a
    /// question with no options is held, never resolved).
    pub(crate) fn open(&self, route: &str, recipient: &str, conv: &Conversation) -> QuestionId {
        self.open_with(route, recipient, conv, &["approve", "reject"])
    }

    /// [`Self::open`] with the option labels `options`; none is free text.
    pub(crate) fn open_with(
        &self,
        route: &str,
        recipient: &str,
        conv: &Conversation,
        options: &[&str],
    ) -> QuestionId {
        let id = self.table.reserve().expect("reserve");
        let ask = Ask {
            channel: Channel::Slack,
            route: route.into(),
            recipient: recipient.into(),
            asker_managed_session: ASKER.into(),
            conversation: conv.clone(),
            message_ts: conv.thread_ts.clone(),
            options: options.iter().map(|o| (*o).to_owned()).collect(),
        };
        self.table.open(id, ask).expect("open question");
        id
    }

    pub(crate) fn resolves(&self) -> usize {
        self.resolves.load(Ordering::SeqCst)
    }

    pub(crate) fn audits(&self) -> Vec<(&'static str, bool)> {
        self.audits.lock().expect("audits").clone()
    }

    pub(crate) fn log_bytes(&self) -> Vec<u8> {
        std::fs::read(questions_path(&self.home)).unwrap_or_default()
    }
}

impl InboundEnv for Fake {
    fn check_inbound(
        &self,
        channel: Channel,
        sender: &str,
        binding: &[QuestionId],
        questions: &[QuestionState],
    ) -> Result<(Route, QuestionId), Refusal<DropReason>> {
        if let Some(deny) = self.snapshot {
            return Err(Refusal::Snapshot(deny));
        }
        match self
            .policy
            .check_inbound(channel, sender, binding, questions)
        {
            InboundDecision::Answer { route, question } => Ok((route.clone(), question)),
            InboundDecision::Drop(reason) => Err(Refusal::Policy(reason)),
        }
    }

    fn table(&self) -> Result<Arc<dyn AnswerTable>, String> {
        self.table_reads.fetch_add(1, Ordering::SeqCst);
        if matches!(self.table_mode, TableMode::OpenFails) {
            return Err("held by another question table".into());
        }
        Ok(Arc::new(Counting {
            inner: Arc::clone(&self.table),
            resolves: Arc::clone(&self.resolves),
            mode: self.table_mode.clone(),
        }))
    }

    fn take_rate(&self, bucket: RateBucket<'_>) -> RateTake {
        self.limiter.take(bucket)
    }

    fn preflight(&self, _question: &Question) -> Result<(), String> {
        if let Some(gate) = &self.gate {
            gate.wait();
        }
        self.preflight.clone()
    }

    fn audit(
        &self,
        ev: &InboundEvent,
        code: &'static str,
        rate_limited: bool,
    ) -> Result<(), String> {
        self.audits
            .lock()
            .expect("audits")
            .push((code, rate_limited));
        if self.audit_fails {
            return Err("audit log is read-only".into());
        }
        if rate_limited {
            return Ok(());
        }
        ChannelAudit::for_home(Some(&self.home))
            .inbound_dropped(ev.channel, &ev.sender, ev.text.len(), code)
            .map_err(|e| e.to_string())
    }
}

/// Routes `owner-0`, `owner-1`, … on Slack, one per recipient.
pub(crate) fn policy(recipients: &[&str]) -> ChannelPolicy {
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

pub(crate) fn conv(dm: &str, ts: &str) -> Conversation {
    Conversation {
        dm_channel_id: dm.into(),
        thread_ts: ts.into(),
    }
}

pub(crate) fn owner_conv() -> Conversation {
    conv("D0OWNERDM", "1700000000.000100")
}

pub(crate) fn ev(sender: &str, conversation: &Conversation, text: &str) -> InboundEvent {
    InboundEvent {
        channel: Channel::Slack,
        sender: sender.into(),
        conversation: conversation.clone(),
        text: text.into(),
    }
}

/// The drop code, or a panic naming the resolved question.
#[track_caller]
pub(crate) fn dropped(verdict: &Verdict) -> &'static str {
    match verdict {
        Verdict::Dropped(d) => d.code,
        Verdict::Resolved(r) => panic!("expected a drop, resolved {}", r.question().id),
        Verdict::Held(h) => panic!("expected a drop, held {}", h.question().id),
    }
}

/// Every decoded audit line under `home`.
fn audit_lines(home: &Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(audit_path(home))
        .unwrap_or_default()
        .lines()
        .map(|l| serde_json::from_str(l).expect("audit line is JSON"))
        .collect()
}

fn answered(fake: &Fake, id: QuestionId) -> bool {
    fake.table.state(id).expect("known question").answered
}

/// 🔴 #9873: an unrouted sender drops before the table is read, leaves the
/// table unchanged, and writes one audit line with no text in it.
#[test]
fn unknown_sender_changes_no_state_and_is_logged() {
    let fake = Fake::new();
    let id = fake.open("owner-0", OWNER, &owner_conv());
    let before = fake.log_bytes();
    let verdict = decide(&fake, &ev(STRANGER, &owner_conv(), SECRET_TEXT));
    assert_eq!(dropped(&verdict), "unknown_sender");
    assert_eq!(fake.table_reads.load(Ordering::SeqCst), 0, "no table read");
    assert_eq!(fake.resolves(), 0);
    assert_eq!(fake.log_bytes(), before, "the table is unchanged");
    assert!(!answered(&fake, id));
    let lines = audit_lines(&fake.home);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0]["reason"], "unknown_sender");
    assert_eq!(lines[0]["sender"], STRANGER);
    assert_eq!(lines[0]["length"], SECRET_TEXT.len());
    let raw = std::fs::read_to_string(audit_path(&fake.home)).expect("audit");
    assert!(
        !raw.contains(SECRET_TEXT),
        "the text reached the audit: {raw}"
    );
}

/// 🔴 #9873: a routed sender with no open question in the thread resolves
/// nothing, so nothing is tainted or delivered.
#[test]
fn matched_route_without_open_question_never_starts_a_prompt() {
    let fake = Fake::new();
    let verdict = decide(&fake, &ev(OWNER, &owner_conv(), "approve"));
    assert_eq!(dropped(&verdict), "no_open_question");
    assert_eq!(fake.resolves(), 0);
    assert_eq!(fake.audits(), vec![("no_open_question", false)]);
    let taint = crate::core::channels_taint::taint_dir(&fake.home);
    assert!(!taint.exists(), "no taint was written");
}

/// 🔴 #9873 (AC3): the second answer drops `already_resolved` with the first
/// answerer and time, whether the policy pre-filter or the CAS catches it;
/// `resolve` runs once on the pre-filter path.
#[test]
fn second_answer_gets_already_answered() {
    for (mode, resolves) in [(TableMode::Real, 1), (TableMode::StaleState, 2)] {
        let mut fake = Fake::new();
        fake.table_mode = mode;
        let id = fake.open("owner-0", OWNER, &owner_conv());
        let first = decide(&fake, &ev(OWNER, &owner_conv(), "approve"));
        assert!(matches!(first, Verdict::Resolved(_)), "{first:?}");
        let second = decide(&fake, &ev(OWNER, &owner_conv(), "reject"));
        let Verdict::Dropped(d) = second else {
            panic!("a second answer resolved");
        };
        assert_eq!(d.code, "already_resolved");
        let won = d.first.expect("the first answer");
        let recorded = fake.table.answer(id).expect("recorded answer");
        assert_eq!(won.answerer, OWNER);
        assert_eq!(won.answered_at, recorded.answered_at);
        assert_eq!(fake.resolves(), resolves);
    }
}

/// 🔴 #9873 (AC4, F6): a routed sender answering another's question, by the
/// policy's route check or the table's recipient check, changes nothing.
#[test]
fn non_owner_dm_never_resolves_a_decision() {
    let fake = Fake::new();
    let theirs = conv("D0OTHERDM", "1700000000.000200");
    let id = fake.open("owner-1", OTHER, &theirs);
    let before = fake.log_bytes();
    let verdict = decide(&fake, &ev(OWNER, &theirs, "approve"));
    assert_eq!(dropped(&verdict), "not_owned_by_route");
    // A row naming OWNER's route but OTHER as recipient: the recipient check.
    let forged = conv("D0FORGED", "1700000000.000300");
    let forged_id = fake.open("owner-0", OTHER, &forged);
    let before_forged = fake.log_bytes();
    let verdict = decide(&fake, &ev(OWNER, &forged, "approve"));
    assert_eq!(dropped(&verdict), "not_recipient");
    assert_eq!(fake.log_bytes(), before_forged);
    assert!(before_forged.starts_with(&before));
    assert!(!answered(&fake, id) && !answered(&fake, forged_id));
    // The CAS refusal itself is a drop too.
    let mut fake = Fake::new();
    fake.table_mode = TableMode::Resolves(ResolveOutcome::NotRecipient);
    let id = fake.open("owner-0", OWNER, &owner_conv());
    let verdict = decide(&fake, &ev(OWNER, &owner_conv(), "approve"));
    assert_eq!(dropped(&verdict), "not_recipient");
    assert!(!answered(&fake, id));
}

/// 🔴 #9873 (epic): a DM reply outside the question's thread never resolves
/// it, even from its recipient.
#[test]
fn answer_from_another_thread_never_resolves() {
    let fake = Fake::new();
    let id = fake.open("owner-0", OWNER, &conv("D0OWNERDM", "1700000000.000100"));
    let verdict = decide(
        &fake,
        &ev(OWNER, &conv("D0OWNERDM", "1700000000.000999"), "yes"),
    );
    assert_eq!(dropped(&verdict), "no_open_question");
    let verdict = decide(
        &fake,
        &ev(OWNER, &conv("D0ELSEWHERE", "1700000000.000100"), "yes"),
    );
    assert_eq!(dropped(&verdict), "no_open_question");
    assert_eq!(fake.resolves(), 0);
    assert!(!answered(&fake, id));
}

/// 🔴 #9873 (F4): a reply in a thread with no question never falls back to
/// the latest open question, even when the text names one as `[Q-n]`.
#[test]
fn reply_with_no_matching_thread_never_binds_to_the_latest_question() {
    let fake = Fake::new();
    let older = fake.open("owner-0", OWNER, &conv("D0OWNERDM", "1700000000.000100"));
    let latest = fake.open("owner-0", OWNER, &conv("D0OWNERDM", "1700000000.000200"));
    let text = format!("[Q-{latest}] yes");
    let verdict = decide(
        &fake,
        &ev(OWNER, &conv("D0OWNERDM", "1700000000.000300"), &text),
    );
    assert_eq!(dropped(&verdict), "no_open_question");
    assert_eq!(fake.resolves(), 0);
    assert!(!answered(&fake, older) && !answered(&fake, latest));
}

/// 🔴 #9873 (F1): every snapshot refusal drops before the table is read.
#[test]
fn inbound_with_stale_snapshot_drops() {
    let stale = SnapshotDeny::Stale {
        age: Duration::from_secs(61),
        max: Duration::from_secs(60),
    };
    for (deny, code) in [
        (SnapshotDeny::NeverLoaded, "snapshot_never_loaded"),
        (stale, "snapshot_stale"),
        (SnapshotDeny::Denied, "snapshot_denied"),
        (SnapshotDeny::Poisoned, "snapshot_poisoned"),
    ] {
        let mut fake = Fake::new();
        let id = fake.open("owner-0", OWNER, &owner_conv());
        fake.snapshot = Some(deny);
        let verdict = decide(&fake, &ev(OWNER, &owner_conv(), "approve"));
        assert_eq!(dropped(&verdict), code);
        assert_eq!(fake.table_reads.load(Ordering::SeqCst), 0, "{code}");
        assert_eq!(fake.audits(), vec![(code, false)]);
        assert!(!answered(&fake, id));
    }
}

/// 🔴 #9873 (F3, AC9): a table that cannot open drops; nothing falls back.
#[test]
fn inbound_drops_when_table_cannot_open() {
    let mut fake = Fake::new();
    fake.table_mode = TableMode::OpenFails;
    let verdict = decide(&fake, &ev(OWNER, &owner_conv(), "approve"));
    assert_eq!(dropped(&verdict), "table_unavailable");
    assert_eq!(fake.resolves(), 0);
}

/// 🔴 #9873 (F3, AC9): a poisoned table drops before any lookup.
#[test]
fn inbound_drops_on_a_poisoned_table() {
    let mut fake = Fake::new();
    let id = fake.open("owner-0", OWNER, &owner_conv());
    fake.table_mode = TableMode::Poisoned;
    let verdict = decide(&fake, &ev(OWNER, &owner_conv(), "approve"));
    assert_eq!(dropped(&verdict), "table_poisoned");
    assert_eq!(fake.resolves(), 0);
    assert!(!answered(&fake, id));
}

/// 🔴 #9873 (F5, F6): a `resolve` that does not resolve delivers nothing.
#[test]
fn resolve_poisoned_delivers_nothing() {
    for (outcome, code) in [
        (ResolveOutcome::Poisoned, "table_poisoned"),
        (ResolveOutcome::Unknown, "unknown_question"),
        (ResolveOutcome::NotRecipient, "not_recipient"),
    ] {
        let mut fake = Fake::new();
        fake.table_mode = TableMode::Resolves(outcome);
        fake.open("owner-0", OWNER, &owner_conv());
        let verdict = decide(&fake, &ev(OWNER, &owner_conv(), "approve"));
        assert_eq!(dropped(&verdict), code);
        assert_eq!(fake.resolves(), 1, "{code}");
    }
}

/// 🔴 #9873 (F14): an audit write that fails is logged; the drop stands.
#[test]
fn inbound_drop_stands_when_audit_write_fails() {
    let mut fake = Fake::new();
    fake.audit_fails = true;
    let id = fake.open("owner-0", OWNER, &owner_conv());
    let verdict = decide(&fake, &ev(STRANGER, &owner_conv(), "approve"));
    assert_eq!(dropped(&verdict), "unknown_sender");
    let verdict = decide(&fake, &ev(OWNER, &conv("D0X", "1"), "approve"));
    assert_eq!(dropped(&verdict), "no_open_question");
    assert_eq!(fake.resolves(), 0);
    assert!(!answered(&fake, id));
}

/// 🔴 #9873 (F17, AC7): an answer over the bound drops whole before
/// `resolve`; one at the bound passes it (#10127: and, naming no option, is
/// held).
#[test]
fn over_long_answer_drops_without_resolving() {
    let fake = Fake::new();
    let id = fake.open("owner-0", OWNER, &owner_conv());
    let long = "y".repeat(MAX_ANSWER_BYTES + 1);
    let verdict = decide(&fake, &ev(OWNER, &owner_conv(), &long));
    assert_eq!(dropped(&verdict), "answer_too_long");
    assert_eq!(fake.resolves(), 0);
    assert!(!answered(&fake, id));
    let verdict = decide(&fake, &ev(OWNER, &owner_conv(), "   "));
    assert_eq!(dropped(&verdict), "answer_empty");
    let fits = "y".repeat(MAX_ANSWER_BYTES);
    let verdict = decide(&fake, &ev(OWNER, &owner_conv(), &fits));
    let Verdict::Held(h) = verdict else {
        panic!("an answer at the bound must pass it: {verdict:?}");
    };
    assert_eq!(h.question().id, id);
    assert_eq!(fake.table.unparsed(id)[0].text_len, MAX_ANSWER_BYTES);
}

/// 🔴 #9873 (Q4, AC6): a preflight failure drops before `resolve`, so the
/// question stays open and its recipient may answer again.
#[test]
fn failed_preflight_leaves_the_question_open_and_answerable() {
    let mut fake = Fake::new();
    fake.preflight = Err("the asker has no claude session id".into());
    let id = fake.open("owner-0", OWNER, &owner_conv());
    let verdict = decide(&fake, &ev(OWNER, &owner_conv(), "approve"));
    assert_eq!(dropped(&verdict), "asker_unavailable");
    assert_eq!(fake.resolves(), 0);
    assert!(!answered(&fake, id));
    fake.preflight = Ok(());
    let verdict = decide(&fake, &ev(OWNER, &owner_conv(), "approve"));
    let Verdict::Resolved(r) = verdict else {
        panic!("the open question must stay answerable: {verdict:?}");
    };
    assert_eq!((r.question().id, r.answerer()), (id, OWNER));
}

/// 🔴 #9873 (AC8, Q7): the 101st answer in a window on one binding drops,
/// with one `rate_limited` line per window; another binding and the next
/// window are unaffected. Unknown senders share their own window.
#[test]
fn per_binding_rate_limit_drops_101st() {
    let mut fake = Fake::new();
    // A failing preflight keeps the question open, so every event reaches
    // the route's window.
    fake.preflight = Err("asker busy".into());
    fake.open("owner-0", OWNER, &owner_conv());
    let theirs = conv("D0OTHERDM", "1700000000.000200");
    fake.open("owner-1", OTHER, &theirs);
    for n in 0..100 {
        let verdict = decide(&fake, &ev(OWNER, &owner_conv(), "approve"));
        assert_eq!(dropped(&verdict), "asker_unavailable", "admit {n}");
    }
    for _ in 0..2 {
        let verdict = decide(&fake, &ev(OWNER, &owner_conv(), "approve"));
        assert_eq!(dropped(&verdict), "rate_limited");
    }
    let limited: Vec<_> = fake.audits().into_iter().filter(|(_, rl)| *rl).collect();
    assert_eq!(limited, vec![("rate_limited", true)], "one line per window");
    let verdict = decide(&fake, &ev(OTHER, &theirs, "approve"));
    assert_eq!(dropped(&verdict), "asker_unavailable", "another binding");
    fake.clock.advance(Duration::from_secs(60));
    let verdict = decide(&fake, &ev(OWNER, &owner_conv(), "approve"));
    assert_eq!(dropped(&verdict), "asker_unavailable", "the next window");
    for _ in 0..100 {
        assert_eq!(
            dropped(&decide(&fake, &ev(STRANGER, &owner_conv(), "x"))),
            "unknown_sender"
        );
    }
    assert_eq!(
        dropped(&decide(&fake, &ev(STRANGER, &owner_conv(), "x"))),
        "rate_limited"
    );
    assert_eq!(fake.resolves(), 0);
}

/// 🔴 #9873 (F15): a poisoned limiter drops; it never admits by default.
#[test]
fn poisoned_rate_limiter_drops() {
    let fake = Arc::new(Fake::new());
    let id = fake.open("owner-0", OWNER, &owner_conv());
    let holder = Arc::clone(&fake);
    let poisoned = std::thread::spawn(move || {
        let _guard = holder.limiter.inner.lock().expect("limiter lock");
        panic!("poison the limiter");
    })
    .join();
    assert!(poisoned.is_err(), "the holder panicked");
    let verdict = decide(&*fake, &ev(OWNER, &owner_conv(), "approve"));
    assert_eq!(dropped(&verdict), "rate_limiter_poisoned");
    assert_eq!(fake.resolves(), 0);
    assert!(!answered(&fake, id));
    assert_eq!(
        fake.limiter.take(RateBucket::UnknownSender),
        RateTake::Poisoned
    );
}
