//! Tests for reading a reply as one option, and holding any other (#10127,
//! S5 P2b).
//!
//! Why: a reply resolves a question only as an option it names; every other
//! reply is held and marks the question `unparsed`, and a step that cannot
//! decide writes nothing. Each test goes red against a decision that
//! resolves free text, matches loosely, or annotates where it must drop.
//! What: the matcher alone, then `decide` over the inbound fake (a real
//! table and audit) wrapped by [`OptEnv`], whose table can hide a question
//! or refuse the annotation.
//! Test: this is the test module.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use trusty_channels::policy::{Channel, DropReason, QuestionId, QuestionState, Route};

use super::tests::{Fake, OWNER, conv, dropped, ev, owner_conv};
use super::{AnswerTable, InboundEnv, InboundEvent, RateBucket, RateTake, Verdict, decide};
use crate::channels::option_match::{HoldReason, match_reply};
use crate::channels::question_table::{
    Answer, Conversation, Question, ResolveOutcome, UnparsedOutcome,
};
use crate::channels::snapshot::Refusal;

/// How [`OptTable`] departs from the table it wraps.
#[derive(Clone, Default)]
pub(crate) struct Faults {
    /// `find_by_conversation` binds every thread to this id.
    pub(crate) bind_to: Option<QuestionId>,
    /// `get` finds no question.
    pub(crate) get_none: bool,
    /// `mark_unparsed` returns this and writes nothing.
    pub(crate) mark: Option<UnparsedOutcome>,
}

/// The fake's table, with [`Faults`] and a count of `mark_unparsed` calls.
struct OptTable {
    inner: Arc<dyn AnswerTable>,
    faults: Faults,
    marks: Arc<AtomicUsize>,
}

impl AnswerTable for OptTable {
    fn find_by_conversation(&self, conversation: &Conversation) -> Option<QuestionId> {
        self.faults
            .bind_to
            .or_else(|| self.inner.find_by_conversation(conversation))
    }

    fn state(&self, id: QuestionId) -> Option<QuestionState> {
        self.inner.state(id)
    }

    fn get(&self, id: QuestionId) -> Option<Question> {
        if self.faults.get_none {
            return None;
        }
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
        self.inner.resolve(id, answerer, conversation, text)
    }

    fn mark_unparsed(
        &self,
        id: QuestionId,
        answerer: &str,
        conversation: &Conversation,
        text: &str,
    ) -> UnparsedOutcome {
        self.marks.fetch_add(1, Ordering::SeqCst);
        if let Some(outcome) = &self.faults.mark {
            return outcome.clone();
        }
        self.inner.mark_unparsed(id, answerer, conversation, text)
    }

    fn is_poisoned(&self) -> bool {
        self.inner.is_poisoned()
    }
}

/// The inbound fake, its table wrapped in an [`OptTable`].
pub(crate) struct OptEnv {
    pub(crate) fake: Fake,
    pub(crate) faults: Faults,
    pub(crate) marks: Arc<AtomicUsize>,
}

impl OptEnv {
    pub(crate) fn new(fake: Fake) -> Self {
        Self {
            fake,
            faults: Faults::default(),
            marks: Arc::new(AtomicUsize::new(0)),
        }
    }

    pub(crate) fn marks(&self) -> usize {
        self.marks.load(Ordering::SeqCst)
    }
}

impl InboundEnv for OptEnv {
    fn check_inbound(
        &self,
        channel: Channel,
        sender: &str,
        binding: &[QuestionId],
        questions: &[QuestionState],
    ) -> Result<(Route, QuestionId), Refusal<DropReason>> {
        self.fake.check_inbound(channel, sender, binding, questions)
    }

    fn table(&self) -> Result<Arc<dyn AnswerTable>, String> {
        let inner = self.fake.table()?;
        Ok(Arc::new(OptTable {
            inner,
            faults: self.faults.clone(),
            marks: Arc::clone(&self.marks),
        }))
    }

    fn take_rate(&self, bucket: RateBucket<'_>) -> RateTake {
        self.fake.take_rate(bucket)
    }

    fn preflight(&self, question: &Question) -> Result<(), String> {
        self.fake.preflight(question)
    }

    fn audit(
        &self,
        ev: &InboundEvent,
        code: &'static str,
        rate_limited: bool,
    ) -> Result<(), String> {
        self.fake.audit(ev, code, rate_limited)
    }
}

/// `labels` as stored option labels.
fn labels(labels: &[&str]) -> Vec<String> {
    labels.iter().map(|l| (*l).to_owned()).collect()
}

/// The option number `reply` names in `options`, if any.
fn number(options: &[&str], reply: &str) -> Option<usize> {
    match_reply(&labels(options), reply).map(|c| c.number())
}

/// The fake with one question to [`OWNER`] offering `Merge` and `Hold`.
fn merge_or_hold() -> (Fake, QuestionId) {
    let fake = Fake::new();
    let id = fake.open_with("owner-0", OWNER, &owner_conv(), &["Merge", "Hold"]);
    (fake, id)
}

/// The option `verdict` resolved its question as: (number, stored label).
#[track_caller]
fn chosen(verdict: &Verdict) -> (usize, String) {
    let Verdict::Resolved(r) = verdict else {
        panic!("expected a resolve: {verdict:?}");
    };
    let choice = r.choice().expect("decide resolves only a named option");
    (choice.number(), choice.label().to_owned())
}

/// 🔴 #10127: a reply equal to a label resolves the question as that option,
/// carrying the stored label.
#[test]
fn reply_equal_to_a_label_matches_that_option() {
    assert_eq!(number(&["Merge", "Hold"], "Hold"), Some(2));
    assert_eq!(number(&["Merge", "Hold"], "Merge"), Some(1));
    let (fake, id) = merge_or_hold();
    let verdict = decide(&fake, &ev(OWNER, &owner_conv(), "Hold"));
    assert_eq!(chosen(&verdict), (2, "Hold".to_owned()));
    assert!(fake.table.state(id).expect("known").answered);
    assert!(fake.table.unparsed(id).is_empty());
}

/// 🔴 #10127: a reply equal to an option's 1-based number resolves the
/// question as that option.
#[test]
fn reply_equal_to_a_number_matches_that_option() {
    assert_eq!(number(&["Merge", "Hold"], "1"), Some(1));
    assert_eq!(number(&["Merge", "Hold"], "02"), Some(2));
    let (fake, _) = merge_or_hold();
    let verdict = decide(&fake, &ev(OWNER, &owner_conv(), "1"));
    assert_eq!(chosen(&verdict), (1, "Merge".to_owned()));
}

/// 🔴 #10127: the match folds case and trims outer whitespace, on the reply
/// and on the label alike.
#[test]
fn match_folds_case_and_trims_whitespace() {
    assert_eq!(number(&["Merge", "Hold"], "  hOLD \n"), Some(2));
    assert_eq!(number(&["Merge", " Hold "], "hold"), Some(2));
    assert_eq!(number(&["Merge", "Hold"], "\t2 "), Some(2));
    let (fake, _) = merge_or_hold();
    let verdict = decide(&fake, &ev(OWNER, &owner_conv(), " MERGE "));
    assert_eq!(chosen(&verdict), (1, "Merge".to_owned()));
}

/// 🔴 #10127: a number out of range, a label with extra text, or any other
/// text matches nothing, and `decide` holds it instead of resolving.
#[test]
fn number_out_of_range_or_label_with_extra_text_does_not_match() {
    for reply in ["0", "3", "-1", "2.", "Hold now", "Merge Hold", "Hol", "yes"] {
        assert_eq!(number(&["Merge", "Hold"], reply), None, "{reply:?}");
    }
    assert_eq!(number(&[], "1"), None, "no options: no number names one");
    for reply in ["3", "Hold now"] {
        let (fake, id) = merge_or_hold();
        let verdict = decide(&fake, &ev(OWNER, &owner_conv(), reply));
        let Verdict::Held(held) = &verdict else {
            panic!("{reply:?} must be held: {verdict:?}");
        };
        assert_eq!(held.reason(), HoldReason::Unmatched);
        assert_eq!(fake.resolves(), 0, "{reply:?}");
        assert!(!fake.table.state(id).expect("known").answered);
    }
}

/// 🔴 #10127 (df-Da7 Q2 c): a reply to a question with no options is held
/// like a no-match: audited as `free_text_held`, annotated, never resolved,
/// and the question stays answerable by a second reply.
#[test]
fn free_text_question_reply_is_held_like_a_no_match() {
    let fake = Fake::new();
    let id = fake.open_with("owner-0", OWNER, &owner_conv(), &[]);
    for n in 1..=2 {
        let verdict = decide(&fake, &ev(OWNER, &owner_conv(), "approve"));
        let Verdict::Held(held) = &verdict else {
            panic!("free text must be held: {verdict:?}");
        };
        assert_eq!(held.reason(), HoldReason::FreeText);
        assert_eq!(held.question().id, id);
        assert_eq!(fake.table.unparsed(id).len(), n, "one record per reply");
    }
    assert_eq!(fake.resolves(), 0);
    assert!(!fake.table.state(id).expect("known").answered);
    assert_eq!(
        fake.audits(),
        vec![("free_text_held", false), ("free_text_held", false)]
    );
    let taint = crate::core::channels_taint::taint_dir(&fake.home);
    assert!(!taint.exists(), "nothing was tainted");
}

/// 🔴 #10127 (plan §6): a failed preflight drops before the annotation, so
/// a foreseeable failure marks nothing.
#[test]
fn failed_preflight_marks_nothing() {
    let (mut fake, id) = merge_or_hold();
    fake.preflight = Err("the asker has no pane".into());
    let before = fake.log_bytes();
    let env = OptEnv::new(fake);
    let verdict = decide(&env, &ev(OWNER, &owner_conv(), "maybe"));
    assert_eq!(dropped(&verdict), "asker_unavailable");
    assert_eq!(env.marks(), 0, "no mark_unparsed call");
    assert_eq!(env.fake.log_bytes(), before, "nothing was written");
    assert!(env.fake.table.unparsed(id).is_empty());
}

/// 🔴 #10127 (plan §6): an unknown, reserved-only or wrong-thread question
/// keeps its existing drop and writes no `unparsed` record; a mark the
/// table refuses drops with `resolve`'s code and writes nothing either.
#[test]
fn reply_to_unknown_or_reserved_question_writes_no_unparsed() {
    let (fake, id) = merge_or_hold();
    let reserved = fake.table.reserve().expect("reserve");
    let mut env = OptEnv::new(fake);
    let before = env.fake.log_bytes();
    let elsewhere = conv("D0OWNERDM", "1700000000.000999");
    assert_eq!(
        dropped(&decide(&env, &ev(OWNER, &elsewhere, "maybe"))),
        "no_open_question",
        "wrong thread"
    );
    // A thread bound to a reserved-only id: the policy sees no question.
    env.faults.bind_to = Some(reserved);
    assert_eq!(
        dropped(&decide(&env, &ev(OWNER, &owner_conv(), "maybe"))),
        "no_open_question",
        "reserved-only"
    );
    env.faults.bind_to = None;
    env.faults.get_none = true;
    assert_eq!(
        dropped(&decide(&env, &ev(OWNER, &owner_conv(), "maybe"))),
        "unknown_question"
    );
    env.faults.get_none = false;
    assert_eq!(env.marks(), 0, "no drop above reached the annotation");
    for (refused, code) in [
        (ResolveOutcome::Unknown, "unknown_question"),
        (ResolveOutcome::NotRecipient, "not_recipient"),
        (ResolveOutcome::Poisoned, "table_poisoned"),
    ] {
        env.faults.mark = Some(UnparsedOutcome::Refused(refused));
        let verdict = decide(&env, &ev(OWNER, &owner_conv(), "maybe"));
        assert_eq!(dropped(&verdict), code);
    }
    assert_eq!(
        env.fake.log_bytes(),
        before,
        "no unparsed record was written"
    );
    assert!(env.fake.table.unparsed(id).is_empty());
    assert!(!env.fake.table.state(id).expect("known").answered);
}
