//! `ask_owner`: one question DM to the owner a route names (#9856, #8433 P4).
//!
//! Why: the Architect must be able to ask its owner a question over Slack,
//! and an answer must later bind to exactly that question (P5). Nothing but
//! a `question` to a user a route names may leave, and only with the
//! daemon-held bot token. This is a Fail-Open Check site: every refusal
//! sends nothing, and a question id whose send outcome is unknown is never
//! reissued.
//! What: [`ask_owner`] runs the whole decision over an [`AskEnv`], in
//! order: the proven asker, the recipient's shape (a Slack user id, Q6),
//! the question text, its option labels (#10127), the policy snapshot's `check_egress`, the bot ref and
//! token, the question table, `reserve`, the post, then `open`. Each refusal
//! writes one audit line and returns an [`AskError`] naming the rule. A
//! failure after `reserve` keeps the id reserved; the table never reuses it.
//! Blocking: the daemon runs it on the blocking pool.
//! Test: `ask_owner_tests.rs`.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use trusty_channels::policy::{Channel, DenyReason, MessageKind, QuestionId, Route};
use trusty_common::credentials::Secret;

use crate::channels::question_table::{
    Ask, Conversation, Question, QuestionTable, QuestionTableError, validate_options,
};
use crate::channels::slack_bot::{Posted, SendFailure};
use crate::channels::snapshot::{Refusal, SnapshotDeny};

/// The daemon route serving [`ask_owner`].
pub const ASK_OWNER_ROUTE: &str = "/api/v1/channels/ask-owner";

/// The longest question text accepted, in bytes.
pub const MAX_QUESTION_BYTES: usize = 4000;

/// One ask: whom to ask and what. The asker is never a field (#9856 Q1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AskOwnerRequest {
    /// The Slack user id (`U…`) a route names.
    pub recipient: String,
    /// The question text.
    pub question: String,
    /// The answer options, numbered from 1 in the posted text; empty for a
    /// free-text question.
    // #10127: optional, so a body without it is today's ask.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<String>,
}

/// A question that was posted and opened.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Asked {
    /// The question id; an answer binds to it.
    pub question_id: QuestionId,
    /// The route that allowed the send.
    pub route: String,
    /// The DM channel the question was posted in.
    pub dm_channel_id: String,
    /// The question's thread.
    pub thread_ts: String,
    /// The options the question was posted with (#10127).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<String>,
}

/// Why an ask was refused or failed. [`AskError::code`] is the rule.
///
/// Why: the asker and the audit both need the rule that stopped the ask,
/// and the arms after `reserve` must say whether the owner may have seen it.
/// What: the refusals before `reserve`, then `SendRejected` (not posted),
/// `SendOutcomeUnknown` (may have been posted) and `SentUnrecorded`
/// (posted, but no open question; an answer to it drops in P5).
/// Test: every `ask_owner_*` test asserts the code it expects.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AskError {
    /// The asker is not proven to be the live managed Architect session.
    #[error("asker_unproven: {0}")]
    AskerUnproven(String),
    /// The recipient is not a Slack user id (`U…`): a channel, a group DM or
    /// an empty name.
    #[error("recipient_not_user_id: {0:?} is not a Slack user id (U…)")]
    RecipientNotUserId(String),
    /// The question is empty or over [`MAX_QUESTION_BYTES`].
    #[error("question_invalid: the question must be 1..={MAX_QUESTION_BYTES} bytes")]
    QuestionInvalid,
    /// The option labels break a rule (#10127, Architect ruling Q5).
    #[error("options_invalid: {0}")]
    OptionsInvalid(&'static str),
    /// The policy snapshot or a route rule refused the send.
    #[error("{0}: the channel policy refused the send")]
    Policy(&'static str),
    /// No Slack bot ref is configured.
    #[error("no_bot_ref: {0}")]
    NoBotRef(String),
    /// The bot ref names a token that may not post.
    #[error("user_token_refused: {0}")]
    UserTokenRefused(String),
    /// The bot token could not be resolved.
    #[error("token_unavailable: {0}")]
    TokenUnavailable(String),
    /// The question table could not be opened.
    #[error("table_unavailable: {0}")]
    TableUnavailable(String),
    /// No id could be reserved.
    #[error("reserve_failed: {0}")]
    ReserveFailed(String),
    /// Nothing was posted; question `id` stays reserved and unused.
    #[error("send_rejected: question {id} was not posted ({code})")]
    SendRejected {
        /// The reserved id.
        id: QuestionId,
        /// The transport's content-free code.
        code: String,
    },
    /// It may have been posted; question `id` stays reserved, never reused.
    #[error(
        "send_outcome_unknown: question {id} may have been posted ({why}); its id is never reused"
    )]
    SendOutcomeUnknown {
        /// The reserved id.
        id: QuestionId,
        /// Why the outcome is unknown.
        why: String,
    },
    /// Posted, but not opened: an answer to it will be refused.
    #[error(
        "sent_unrecorded: question {id} was posted but not recorded ({why}); an answer to it is refused"
    )]
    SentUnrecorded {
        /// The reserved id.
        id: QuestionId,
        /// Why it is unrecorded.
        why: String,
    },
    /// The ask's task did not finish; a posted question keeps its id
    /// reserved, never reused.
    #[error("ask_aborted: {0}")]
    Aborted(String),
}

impl AskError {
    /// The rule, as the audit reason code.
    pub fn code(&self) -> &'static str {
        match self {
            Self::AskerUnproven(_) => "asker_unproven",
            Self::RecipientNotUserId(_) => "recipient_not_user_id",
            Self::QuestionInvalid => "question_invalid",
            Self::OptionsInvalid(_) => "options_invalid",
            Self::Policy(code) => code,
            Self::NoBotRef(_) => "no_bot_ref",
            Self::UserTokenRefused(_) => "user_token_refused",
            Self::TokenUnavailable(_) => "token_unavailable",
            Self::TableUnavailable(_) => "table_unavailable",
            Self::ReserveFailed(_) => "reserve_failed",
            Self::SendRejected { .. } => "send_rejected",
            Self::SendOutcomeUnknown { .. } => "send_outcome_unknown",
            Self::SentUnrecorded { .. } => "sent_unrecorded",
            Self::Aborted(_) => "ask_aborted",
        }
    }
}

/// The question table, as [`ask_owner`] uses it.
///
/// Why: a test must make `reserve` or `open` fail on a table that opened.
/// What: [`QuestionTable`]'s two operations.
pub trait QuestionStore: Send + Sync {
    /// See [`QuestionTable::reserve`].
    fn reserve(&self) -> Result<QuestionId, QuestionTableError>;
    /// See [`QuestionTable::open`].
    fn open(&self, id: QuestionId, ask: Ask) -> Result<Question, QuestionTableError>;
}

impl QuestionStore for QuestionTable {
    fn reserve(&self) -> Result<QuestionId, QuestionTableError> {
        QuestionTable::reserve(self)
    }

    fn open(&self, id: QuestionId, ask: Ask) -> Result<Question, QuestionTableError> {
        QuestionTable::open(self, id, ask)
    }
}

/// Everything [`ask_owner`] reads or writes outside itself.
///
/// Why: each step is a Fail-Open Check arm that a test must make fail.
/// What: the daemon's is `DaemonAskEnv` (`managed_routes::ask_owner`); every
/// method may block.
pub trait AskEnv {
    /// The policy snapshot's verdict on a Slack `kind` message to `recipient`.
    fn check_egress(
        &self,
        recipient: &str,
        kind: MessageKind,
    ) -> Result<Route, Refusal<DenyReason>>;
    /// The host ceiling's Slack `bot_ref`.
    fn bot_ref(&self) -> Result<String, String>;
    /// The token `bot_ref` names.
    fn bot_token(&self, bot_ref: &str) -> Result<Secret<String>, String>;
    /// The daemon's question table.
    fn table(&self) -> Result<Arc<dyn QuestionStore>, String>;
    /// Post `text` to `recipient`.
    fn post(
        &self,
        token: &Secret<String>,
        recipient: &str,
        text: &str,
    ) -> Result<Posted, SendFailure>;
    /// Write one audit line: a send to `to` stopped by `reason`.
    fn audit(&self, to: &str, reason: &'static str) -> Result<(), String>;
}

/// Ask `req.recipient` `req.question` for the asker `asker` proved.
///
/// Why: see the module doc.
/// What: `asker` is the proven asker's managed session id, or why none was
/// proven. Runs the steps in the module doc's order; the first that refuses
/// or fails is audited and returned. On success, the question is open in
/// the table before this returns.
/// Test: `ask_owner_to_unnamed_recipient_refused_before_any_slack_call`,
/// `ask_owner_non_owner_recipient_refused`, `ask_owner_kind_not_allowed_refused`,
/// `ask_owner_with_stale_snapshot_refused`, `ask_owner_resolves_no_user_token`,
/// `ask_owner_group_dm_or_channel_target_refused`,
/// `ask_owner_opens_question_before_send_returns`,
/// `ask_owner_refuses_an_unproven_asker`,
/// `ask_owner_refuses_when_table_cannot_open`,
/// `ask_owner_refuses_when_reserve_fails`,
/// `ask_owner_refuses_when_token_cannot_resolve`,
/// `ask_owner_unknown_send_outcome_keeps_id_reserved`,
/// `ask_owner_open_failure_after_send_reports_sent_unrecorded`,
/// `ask_owner_refusal_stands_when_audit_write_fails`,
/// `ask_with_options_lists_them_numbered_in_the_slack_text`,
/// `ask_with_one_or_seven_options_is_refused`,
/// `ask_with_duplicate_after_case_fold_or_numeric_label_is_refused`,
/// `ask_with_overlong_or_control_char_label_is_refused`,
/// `ask_without_options_posts_todays_text`,
/// `ask_with_options_posts_nothing_when_reserve_fails`.
pub fn ask_owner(
    env: &dyn AskEnv,
    asker: Result<String, String>,
    req: &AskOwnerRequest,
) -> Result<Asked, AskError> {
    let fail = |error: AskError| {
        // #9856: an audit error is logged; the refusal stands either way.
        if let Err(why) = env.audit(&req.recipient, error.code()) {
            tracing::error!(%why, code = error.code(), "ask_owner: audit line not written");
        }
        Err(error)
    };
    let asker = match asker {
        Ok(asker) => asker,
        Err(why) => return fail(AskError::AskerUnproven(why)),
    };
    if !is_slack_user_id(&req.recipient) {
        return fail(AskError::RecipientNotUserId(req.recipient.clone()));
    }
    if req.question.trim().is_empty() || req.question.len() > MAX_QUESTION_BYTES {
        return fail(AskError::QuestionInvalid);
    }
    // #10127: a bad label set is refused before anything is reserved or sent.
    if !req.options.is_empty()
        && let Err(why) = validate_options(&req.options)
    {
        return fail(AskError::OptionsInvalid(why));
    }
    let route = match env.check_egress(&req.recipient, ASK_KIND) {
        Ok(route) => route,
        Err(refusal) => return fail(AskError::Policy(refusal_code(refusal))),
    };
    let bot_ref = match env.bot_ref() {
        Ok(bot_ref) => bot_ref,
        Err(why) => return fail(AskError::NoBotRef(why)),
    };
    if let Err(why) = crate::channels::slack_bot::check_bot_ref(&bot_ref) {
        return fail(AskError::UserTokenRefused(why));
    }
    let token = match env.bot_token(&bot_ref) {
        Ok(token) => token,
        Err(why) => return fail(AskError::TokenUnavailable(why)),
    };
    let table = match env.table() {
        Ok(table) => table,
        Err(why) => return fail(AskError::TableUnavailable(why)),
    };
    let id = match table.reserve() {
        Ok(id) => id,
        Err(e) => return fail(AskError::ReserveFailed(e.to_string())),
    };
    let text = question_text(id, &req.question, &req.options);
    let posted = match env.post(&token, &req.recipient, &text) {
        Ok(posted) => posted,
        Err(SendFailure::Rejected(code)) => return fail(AskError::SendRejected { id, code }),
        Err(SendFailure::Unknown(why)) => return fail(AskError::SendOutcomeUnknown { id, why }),
        Err(SendFailure::Unrecorded(why)) => return fail(AskError::SentUnrecorded { id, why }),
    };
    let ask = Ask {
        channel: Channel::Slack,
        route: route.name().to_owned(),
        recipient: route.recipient().to_owned(),
        asker_managed_session: asker,
        conversation: Conversation {
            dm_channel_id: posted.channel.clone(),
            thread_ts: posted.ts.clone(),
        },
        message_ts: posted.ts.clone(),
        options: req.options.clone(),
    };
    match table.open(id, ask) {
        Ok(_) => Ok(Asked {
            question_id: id,
            route: route.name().to_owned(),
            dm_channel_id: posted.channel,
            thread_ts: posted.ts,
            options: req.options.clone(),
        }),
        Err(e) => fail(AskError::SentUnrecorded {
            id,
            why: e.to_string(),
        }),
    }
}

/// The posted text: `[Q-<id>] <question>`, then one `<n>. <label>` line per
/// option.
fn question_text(id: QuestionId, question: &str, options: &[String]) -> String {
    // #9856: the id goes in the text, so a reply can name it as data (P5).
    let head = format!("[Q-{id}] {question}");
    // #10127: options are numbered from 1, the number a reply may give.
    let lines = options
        .iter()
        .enumerate()
        .map(|(n, label)| format!("\n{}. {label}", n + 1));
    std::iter::once(head).chain(lines).collect()
}

/// True for a Slack user id: `U` then upper-case ASCII letters and digits
/// (#9856 Q6). A channel (`C…`), a group DM (`G…`) or anything else is not.
pub fn is_slack_user_id(s: &str) -> bool {
    s.len() >= 2
        && s.starts_with('U')
        && s.chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
}

/// The audit code of a policy refusal.
fn refusal_code(refusal: Refusal<DenyReason>) -> &'static str {
    match refusal {
        Refusal::Policy(reason) => reason.as_str(),
        Refusal::Snapshot(SnapshotDeny::NeverLoaded) => "snapshot_never_loaded",
        Refusal::Snapshot(SnapshotDeny::Stale { .. }) => "snapshot_stale",
        Refusal::Snapshot(SnapshotDeny::Denied) => "snapshot_denied",
        Refusal::Snapshot(SnapshotDeny::Poisoned) => "snapshot_poisoned",
    }
}

/// The kind every ask sends; no other kind leaves through `ask_owner`.
pub const ASK_KIND: MessageKind = MessageKind::Question;

#[cfg(test)]
#[path = "ask_owner_tests.rs"]
mod tests;
