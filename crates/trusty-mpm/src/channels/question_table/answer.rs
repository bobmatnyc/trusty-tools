//! The answer side of the question table: `resolve` and the
//! `answered_unparsed` annotation (#8433 P2, #10127).
//!
//! Why: #10127: `question_table.rs` sat at the SLOC cap, and the S5 P2
//! annotation is a second write on the same reply path as `resolve`.
//! What: [`Answer`], [`ResolveOutcome`] and [`QuestionTable::resolve`], moved
//! here unchanged, plus [`Unparsed`], [`UnparsedOutcome`],
//! [`QuestionTable::mark_unparsed`] and [`QuestionTable::unparsed`]. Both
//! writes judge the same refusals, under the same lock hold as the append.
//! Test: `question_table_tests.rs`.

use serde::{Deserialize, Serialize};
use trusty_channels::policy::QuestionId;

use super::replay::{Book, Record};
use super::{Conversation, QuestionTable, now, sha256_hex};

/// The first answer recorded for a question. Holds no answer text, only its
/// digest and length; P5 delivers the text from its own buffer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Answer {
    /// Who answered.
    pub answerer: String,
    /// When it was recorded (RFC 3339, milliseconds).
    pub answered_at: String,
    /// Lowercase hex SHA-256 of the answer text.
    pub text_sha256: String,
    /// The answer text's length in bytes.
    pub text_len: usize,
}

impl Answer {
    /// The record of `answerer` answering `text` now.
    fn new(answerer: &str, text: &str) -> Self {
        Self {
            answerer: answerer.to_owned(),
            answered_at: now(),
            text_sha256: sha256_hex(text),
            text_len: text.len(),
        }
    }
}

/// What [`QuestionTable::resolve`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveOutcome {
    /// The question was open and now holds this answer, durably.
    Resolved,
    /// The question already had an answer; nothing changed.
    AlreadyResolved {
        /// The answer that won.
        first: Answer,
    },
    /// The answerer or conversation is not the question's; nothing changed.
    NotRecipient,
    /// No open or answered question has this id (a reserved-only id too).
    Unknown,
    /// The table is poisoned; nothing changed in memory. Reopen it.
    Poisoned,
}

/// One reply to a question that matched none of its options (#10127).
///
/// Why: Architect ruling Q4 (df-Da7): a held reply keeps its SHA-256 and
/// length, never its text.
/// What: when it was recorded, and the reply's digest and length.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unparsed {
    /// When it was recorded (RFC 3339, milliseconds).
    pub ts: String,
    /// Lowercase hex SHA-256 of the reply text.
    pub text_sha256: String,
    /// The reply text's length in bytes.
    pub text_len: usize,
}

/// What [`QuestionTable::mark_unparsed`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnparsedOutcome {
    /// The annotation is durable; the question stays open (#10127 Q1).
    Marked,
    /// Nothing was written, for the reason [`QuestionTable::resolve`] would
    /// give; never `Resolved`.
    Refused(ResolveOutcome),
}

impl QuestionTable {
    /// Resolve question `id` with `answer` from `answerer` in `conversation`,
    /// once.
    ///
    /// Why: the arbiter for P5. `check_inbound` matches route names only,
    /// and its `answered` copy is taken before this call, so the recipient,
    /// the conversation and "still open" are judged here, in the same lock
    /// hold as the append (#8433 r6 item 4).
    /// What: `Unknown` for no open or answered question; `NotRecipient`
    /// unless `answerer` and `conversation` equal the question's;
    /// `AlreadyResolved` with the first answer; else appends `resolve`,
    /// syncs it and returns `Resolved`. An append error poisons the table
    /// and returns `Poisoned`.
    /// Test: `concurrent_answers_resolve_exactly_once_n_rounds`,
    /// `first_answer_wins_second_gets_already_answered_by_x_at_t`,
    /// `answerer_must_be_the_questions_recipient`,
    /// `answer_from_other_conversation_is_refused`,
    /// `resolve_is_durable_before_it_returns`.
    pub fn resolve(
        &self,
        id: QuestionId,
        answerer: &str,
        conversation: &Conversation,
        answer: &str,
    ) -> ResolveOutcome {
        let Ok(mut state) = self.lock() else {
            return ResolveOutcome::Poisoned;
        };
        if let Some(refused) = refusal(&state.book, id, answerer, conversation) {
            return refused;
        }
        let record = Record::Resolve {
            id,
            answer: Answer::new(answerer, answer),
        };
        match self.commit(&mut state, record) {
            Ok(()) => ResolveOutcome::Resolved,
            Err(_) => ResolveOutcome::Poisoned,
        }
    }

    /// Record that `answerer`'s reply `text` in `conversation` matched no
    /// option of question `id` (#10127).
    ///
    /// Why: S5 P2 holds a reply that matches no option instead of
    /// delivering it, and the log keeps that it came. Architect ruling Q1:
    /// the annotation is not terminal, so the owner may reply again. Q4: the
    /// record keeps the digest and length only.
    /// What: the refusals of [`Self::resolve`], judged in the same lock hold
    /// and returned as `Refused` with nothing written; else appends
    /// `unparsed` and returns `Marked`. The question stays unanswered. An
    /// append error poisons the table and returns `Refused(Poisoned)`.
    /// Test: `unparsed_record_replays_and_keeps_the_question_open`,
    /// `unparsed_for_unknown_question_writes_nothing`,
    /// `unparsed_write_failure_poisons_and_keeps_the_question_open`.
    pub fn mark_unparsed(
        &self,
        id: QuestionId,
        answerer: &str,
        conversation: &Conversation,
        text: &str,
    ) -> UnparsedOutcome {
        let Ok(mut state) = self.lock() else {
            return UnparsedOutcome::Refused(ResolveOutcome::Poisoned);
        };
        if let Some(refused) = refusal(&state.book, id, answerer, conversation) {
            return UnparsedOutcome::Refused(refused);
        }
        let record = Record::Unparsed {
            id,
            ts: now(),
            text_sha256: sha256_hex(text),
            text_len: text.len(),
        };
        match self.commit(&mut state, record) {
            Ok(()) => UnparsedOutcome::Marked,
            Err(_) => UnparsedOutcome::Refused(ResolveOutcome::Poisoned),
        }
    }

    /// The unmatched replies recorded for question `id`, oldest first; empty
    /// when none, when unknown and when poisoned (#10127).
    pub fn unparsed(&self, id: QuestionId) -> Vec<Unparsed> {
        self.lock()
            .ok()
            .and_then(|state| state.book.questions.get(&id).map(|e| e.unparsed.clone()))
            .unwrap_or_default()
    }
}

/// Why a reply from `answerer` in `conversation` may not write to question
/// `id`; `None` when the question is open to it (#8433 r6, #10127).
fn refusal(
    book: &Book,
    id: QuestionId,
    answerer: &str,
    conversation: &Conversation,
) -> Option<ResolveOutcome> {
    let Some(entry) = book.questions.get(&id) else {
        return Some(ResolveOutcome::Unknown);
    };
    // #8433 r6: recipient and conversation are checked inside the CAS.
    let ask = &entry.question.ask;
    if ask.recipient != answerer || ask.conversation != *conversation {
        return Some(ResolveOutcome::NotRecipient);
    }
    entry
        .answer
        .as_ref()
        .map(|first| ResolveOutcome::AlreadyResolved {
            first: first.clone(),
        })
}
