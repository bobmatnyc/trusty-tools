//! Inbound answers: judge, bind and resolve one channel reply (#9873, #8433 P5).
//!
//! Why: an owner's reply may answer exactly one open question, once, and only
//! from the recipient and the thread it was asked in. This is a Fail-Open
//! Check site: every step that cannot decide drops the message, writes one
//! audit line (never the text) and changes no state.
//! What: [`decide`] runs over an [`InboundEnv`], in order: the sender alone
//! (`check_inbound` with no binding, so an unknown sender never reads the
//! table), the question table, the binding by conversation (#9873 Q2; `[Q-n]`
//! text is data only), `check_inbound` with that binding, the route's rate
//! window (Q7), the text bound (Q10), the asker preflight (Q4), then
//! [`QuestionTable::resolve`], the arbiter. Only a `Resolved` verdict carries
//! the text on, to `crate::channels::deliver`. This file holds the crate's
//! only `resolve` call (Q8). Blocking: the daemon runs it on the blocking pool.
//! Test: `inbound_tests.rs`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use trusty_channels::policy::{
    BucketDecision, Channel, Clock, DropReason, MonotonicClock, QuestionId, QuestionState,
    RateLimit, RateLimiter, Route,
};

use crate::channels::question_table::{
    Answer, Conversation, Question, QuestionTable, ResolveOutcome,
};
use crate::channels::snapshot::{Refusal, SnapshotDeny};

/// The longest answer text accepted, in bytes; a longer one drops whole
/// (#9873 Q10), never truncated.
pub const MAX_ANSWER_BYTES: usize = 4000;

/// One inbound message, as a source (the sandbox route now, P6 later) builds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboundEvent {
    /// The channel it arrived on.
    pub channel: Channel,
    /// The sender, as the channel names it (a Slack `U…` id).
    pub sender: String,
    /// Where it was posted: the DM channel and the thread.
    pub conversation: Conversation,
    /// The message text; data, never an instruction.
    pub text: String,
}

/// The question table, as [`decide`] uses it.
///
/// Why: each table outcome is a Fail-Open Check arm a test must force.
/// What: [`QuestionTable`]'s read-only lookups and its `resolve`.
pub trait AnswerTable: Send + Sync {
    /// See [`QuestionTable::find_by_conversation`].
    fn find_by_conversation(&self, conversation: &Conversation) -> Option<QuestionId>;
    /// See [`QuestionTable::state`].
    fn state(&self, id: QuestionId) -> Option<QuestionState>;
    /// See [`QuestionTable::get`].
    fn get(&self, id: QuestionId) -> Option<Question>;
    /// See [`QuestionTable::answer`].
    fn answer(&self, id: QuestionId) -> Option<Answer>;
    /// See [`QuestionTable::resolve`].
    fn resolve(
        &self,
        id: QuestionId,
        answerer: &str,
        conversation: &Conversation,
        text: &str,
    ) -> ResolveOutcome;
    /// See [`QuestionTable::is_poisoned`].
    fn is_poisoned(&self) -> bool;
}

impl AnswerTable for QuestionTable {
    fn find_by_conversation(&self, conversation: &Conversation) -> Option<QuestionId> {
        QuestionTable::find_by_conversation(self, conversation)
    }

    fn state(&self, id: QuestionId) -> Option<QuestionState> {
        QuestionTable::state(self, id)
    }

    fn get(&self, id: QuestionId) -> Option<Question> {
        QuestionTable::get(self, id)
    }

    fn answer(&self, id: QuestionId) -> Option<Answer> {
        QuestionTable::answer(self, id)
    }

    fn resolve(
        &self,
        id: QuestionId,
        answerer: &str,
        conversation: &Conversation,
        text: &str,
    ) -> ResolveOutcome {
        // #9873 Q8: the crate's one `resolve` call site.
        QuestionTable::resolve(self, id, answerer, conversation, text)
    }

    fn is_poisoned(&self) -> bool {
        QuestionTable::is_poisoned(self)
    }
}

/// Which rate window a take spends.
#[derive(Debug, Clone, Copy)]
pub enum RateBucket<'a> {
    /// The window of the route that admitted the answer.
    Route(&'a Route),
    /// The one window every unknown sender shares.
    UnknownSender,
}

/// What a rate take decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RateTake {
    /// Admitted; the message goes on.
    Admit,
    /// Over the limit; dropped. `audit` is true for the first drop of this
    /// bucket in a window, the only one that writes a `rate_limited` line.
    Limited {
        /// Whether this drop writes the window's one audit line.
        audit: bool,
    },
    /// The limiter's lock is poisoned; dropped (never admitted by default).
    Poisoned,
}

/// Everything [`decide`] reads or writes outside itself.
///
/// Why: each step is a Fail-Open Check arm that a test must make fail.
/// What: the daemon's is `DaemonInboundEnv` (`managed_routes::inbound`);
/// every method may block.
pub trait InboundEnv {
    /// The policy snapshot's verdict on a message from `sender` bound to
    /// `binding`, with `questions` the table's view of those ids.
    fn check_inbound(
        &self,
        channel: Channel,
        sender: &str,
        binding: &[QuestionId],
        questions: &[QuestionState],
    ) -> Result<(Route, QuestionId), Refusal<DropReason>>;
    /// The daemon's question table.
    fn table(&self) -> Result<Arc<dyn AnswerTable>, String>;
    /// Spend one admit from `bucket`'s window.
    fn take_rate(&self, bucket: RateBucket<'_>) -> RateTake;
    /// Whether `question`'s asker can take a delivery now (#9873 Q4).
    fn preflight(&self, question: &Question) -> Result<(), String>;
    /// Write one audit line for `ev`, dropped for `code`; `rate_limited`
    /// marks a rate-window drop.
    fn audit(&self, ev: &InboundEvent, code: &'static str, rate_limited: bool)
    -> Result<(), String>;
}

/// A dropped message: the rule, and the first answer for `already_resolved`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dropped {
    /// The rule that dropped it, as the audit reason code.
    pub code: &'static str,
    /// The answer that won, when the question was already answered.
    pub first: Option<Answer>,
}

/// A question this message resolved. Built only by [`decide`], after
/// `resolve` returned `Resolved`, so holding one proves the CAS won.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    question: Question,
    answerer: String,
    text: String,
}

impl Resolved {
    /// The question as opened: its id, asker, channel and route.
    pub fn question(&self) -> &Question {
        &self.question
    }

    /// Who answered, in the route's compared form.
    pub fn answerer(&self) -> &str {
        &self.answerer
    }

    /// The answer text, still untrusted data.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// A `Resolved` built outside [`decide`], for the deliverer's tests only.
    #[cfg(test)]
    pub(crate) fn for_test(question: Question, answerer: &str, text: &str) -> Self {
        Self {
            question,
            answerer: answerer.to_owned(),
            text: text.to_owned(),
        }
    }
}

/// The outcome of one inbound message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// The question is resolved; deliver the answer.
    Resolved(Resolved),
    /// Dropped; nothing changed.
    Dropped(Dropped),
}

/// Judge, bind and resolve `ev`.
///
/// RED STUB (#9873 S2): binds and resolves with no check at all; the next
/// commit replaces it.
pub fn decide(env: &dyn InboundEnv, ev: &InboundEvent) -> Verdict {
    let stub = || {
        Verdict::Dropped(Dropped {
            code: "stub",
            first: None,
        })
    };
    let Ok(table) = env.table() else {
        return stub();
    };
    let Some(id) = table.find_by_conversation(&ev.conversation) else {
        return stub();
    };
    let Some(question) = table.get(id) else {
        return stub();
    };
    let answerer = question.ask.recipient.clone();
    match table.resolve(id, &answerer, &ev.conversation, &ev.text) {
        ResolveOutcome::Resolved => Verdict::Resolved(Resolved {
            question,
            answerer,
            text: ev.text.clone(),
        }),
        _ => stub(),
    }
}

/// The audit code of a snapshot refusal.
pub fn snapshot_code(deny: SnapshotDeny) -> &'static str {
    match deny {
        SnapshotDeny::NeverLoaded => "snapshot_never_loaded",
        SnapshotDeny::Stale { .. } => "snapshot_stale",
        SnapshotDeny::Denied => "snapshot_denied",
        SnapshotDeny::Poisoned => "snapshot_poisoned",
    }
}

/// The boxed [`Clock`] the limiter reads: monotonic in the daemon, a test
/// clock in tests.
struct DynClock(Box<dyn Clock + Send>);

impl Clock for DynClock {
    fn now(&self) -> Duration {
        self.0.now()
    }
}

/// The limiter's windows and when each bucket last wrote its audit line.
struct LimiterState {
    limiter: RateLimiter<DynClock>,
    audited: HashMap<String, Duration>,
}

/// The daemon's inbound rate windows (#9873 Q7, #8454).
///
/// Why: a routed sender may flood an open question, and unknown senders may
/// flood the audit log; at most one `rate_limited` line per bucket per window.
/// What: one trusty-channels `RateLimiter` (per-route windows plus the
/// unknown-sender window at [`RateLimit::DEFAULT`]) under one mutex. State is
/// process-local; a restart resets every window. A poisoned mutex answers
/// [`RateTake::Poisoned`].
/// Test: `per_binding_rate_limit_drops_101st`, `poisoned_rate_limiter_drops`.
pub struct InboundLimiter {
    inner: Mutex<LimiterState>,
}

impl std::fmt::Debug for InboundLimiter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InboundLimiter").finish_non_exhaustive()
    }
}

impl Default for InboundLimiter {
    fn default() -> Self {
        Self::new(Box::new(MonotonicClock::new()))
    }
}

impl InboundLimiter {
    /// A limiter reading `clock`.
    pub fn new(clock: Box<dyn Clock + Send>) -> Self {
        let state = LimiterState {
            limiter: RateLimiter::new(RateLimit::DEFAULT, DynClock(clock)),
            audited: HashMap::new(),
        };
        Self {
            inner: Mutex::new(state),
        }
    }

    /// RED STUB (#9873 S2): admits everything; the next commit replaces it.
    pub fn take(&self, _bucket: RateBucket<'_>) -> RateTake {
        let _ = (&self.inner, BucketDecision::Admit);
        RateTake::Admit
    }
}

#[cfg(test)]
#[path = "inbound_tests.rs"]
mod tests;
