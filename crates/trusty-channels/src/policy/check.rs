//! The two policy checks: outbound ([`ChannelPolicy::check_egress`]) and
//! inbound ([`ChannelPolicy::check_inbound`]).
//!
//! Why: deny by default for every channel (#8454). A send needs a route that
//! names the recipient and lists the kind; an inbound message may only
//! answer one open question its sender's route owns.
//! What: pure functions over a built policy and caller-supplied question
//! state. Each returns an enum whose only accepting arm carries the route;
//! every other outcome is a `Deny` or `Drop` with a reason code. No arm
//! accepts by default.
//! Test: `src/policy/tests/egress.rs`, `src/policy/tests/inbound.rs`.

use crate::policy::table::ChannelPolicy;
use crate::policy::types::{Channel, MessageKind, Route};

/// A question id, as the channel's question ledger numbers it.
pub type QuestionId = u64;

/// Why an outbound message was refused.
///
/// Why: callers audit and report the rule a send broke.
/// What: [`DenyReason::as_str`] is the audit reason code.
/// Test: `unknown_route_denies`, `kind_not_listed_denies`,
/// `empty_policy_denies_all`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DenyReason {
    /// The policy holds no routes.
    NoPolicy,
    /// No route names this (channel, recipient).
    NoRoute,
    /// The route exists but does not list the kind.
    KindNotAllowed,
}

impl DenyReason {
    /// The audit reason code.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NoPolicy => "no_policy",
            Self::NoRoute => "no_route",
            Self::KindNotAllowed => "kind_not_allowed",
        }
    }
}

/// The outbound verdict.
///
/// Why: the type has one accepting arm, and it carries the route that
/// granted the send.
/// What: `Allow(route)` or `Deny(reason)`.
/// Test: `exact_route_and_listed_kind_allow`, `unknown_route_denies`.
#[derive(Debug, Clone, Copy, PartialEq)]
#[must_use]
pub enum EgressDecision<'a> {
    /// A route names the recipient and lists the kind.
    Allow(&'a Route),
    /// Refused; nothing may be sent.
    Deny(DenyReason),
}

/// Why an inbound message was dropped.
///
/// Why: each drop is audited with sender, time and length, never content;
/// the reason code says which rule refused it.
/// What: [`DropReason::as_str`] is the audit reason code.
/// Test: `inbound_unknown_sender_drops`, `inbound_no_open_question_drops`,
/// `second_answer_to_one_question_drops_already_resolved`,
/// `inbound_foreign_or_ambiguous_binding_drops`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DropReason {
    /// No route on this channel names the sender.
    UnknownSender,
    /// The sender's route does not carry questions.
    NoQuestionKind,
    /// The message binds to no question.
    NoBinding,
    /// The message binds to more than one question.
    AmbiguousBinding,
    /// The bound question is not in the open-question set.
    NoOpenQuestion,
    /// The bound question belongs to another route.
    NotOwnedByRoute,
    /// The bound question already has its answer; the first answer wins.
    AlreadyResolved,
}

impl DropReason {
    /// The audit reason code.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UnknownSender => "unknown_sender",
            Self::NoQuestionKind => "no_question_kind",
            Self::NoBinding => "no_binding",
            Self::AmbiguousBinding => "ambiguous_binding",
            Self::NoOpenQuestion => "no_open_question",
            Self::NotOwnedByRoute => "not_owned_by_route",
            Self::AlreadyResolved => "already_resolved",
        }
    }
}

/// The inbound verdict.
///
/// Why: an accepted message may only answer a question; there is no arm
/// that starts a session, a prompt or a tool call.
/// What: `Answer { route, question }` or `Drop(reason)`.
/// Test: `inbound_bound_reply_to_open_question_answers`,
/// `inbound_unknown_sender_drops`.
#[derive(Debug, Clone, Copy, PartialEq)]
#[must_use]
pub enum InboundDecision<'a> {
    /// The message answers `question`, which `route` owns.
    Answer {
        /// The sender's route.
        route: &'a Route,
        /// The question it answers.
        question: QuestionId,
    },
    /// Dropped; nothing reaches a model or a session.
    Drop(DropReason),
}

/// One known question, as the channel's ledger reports it.
///
/// Why: the policy owns no ledger; the caller passes the state it holds.
/// What: the id, the owning route (channel and name) and whether it has its
/// answer.
/// Test: `inbound_no_open_question_drops`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuestionState {
    /// The question id.
    pub id: QuestionId,
    /// The channel it was sent on.
    pub channel: Channel,
    /// The name of the route it was sent on.
    pub route: String,
    /// True once an answer has been recorded.
    pub answered: bool,
}

impl ChannelPolicy {
    /// The outbound check: may a `kind` message go to `recipient`?
    ///
    /// Why: the one rule every send passes before any network call (#8454).
    /// What: `Deny(NoPolicy)` on an empty policy; `Deny(NoRoute)` unless a
    /// route names exactly this (channel, recipient), compared in the
    /// channel's normalized form; `Deny(KindNotAllowed)` unless that route
    /// lists `kind`. Otherwise `Allow(route)`.
    /// Test: `exact_route_and_listed_kind_allow`, `unknown_route_denies`,
    /// `kind_not_listed_denies`, `empty_policy_denies_all`.
    pub fn check_egress(
        &self,
        channel: Channel,
        recipient: &str,
        kind: MessageKind,
    ) -> EgressDecision<'_> {
        if self.is_empty() {
            return EgressDecision::Deny(DenyReason::NoPolicy);
        }
        let Some(route) = self.route_for(channel, recipient) else {
            return EgressDecision::Deny(DenyReason::NoRoute);
        };
        if !route.allows(kind) {
            return EgressDecision::Deny(DenyReason::KindNotAllowed);
        }
        EgressDecision::Allow(route)
    }

    /// The inbound check: may this message answer a question?
    ///
    /// Why: an inbound message may only answer one open question its sender
    /// was asked; first answer wins (#8454 plan §3).
    /// What: `binding` holds the question ids the caller found in the message
    /// as quoted data (a thread match, `[Q-n]` tokens); repeats of one id
    /// count once. Accepts only when a route on `channel` names `sender`,
    /// that route lists `question`, `binding` names exactly one id,
    /// `questions` holds exactly one entry with that id, the entry belongs
    /// to the sender's route, and it is unanswered. Every other case drops.
    /// Test: `inbound_bound_reply_to_open_question_answers`,
    /// `inbound_unknown_sender_drops`, `inbound_no_open_question_drops`,
    /// `second_answer_to_one_question_drops_already_resolved`,
    /// `inbound_foreign_or_ambiguous_binding_drops`.
    pub fn check_inbound(
        &self,
        channel: Channel,
        sender: &str,
        binding: &[QuestionId],
        questions: &[QuestionState],
    ) -> InboundDecision<'_> {
        let Some(route) = self.route_for(channel, sender) else {
            return InboundDecision::Drop(DropReason::UnknownSender);
        };
        if !route.allows(MessageKind::Question) {
            return InboundDecision::Drop(DropReason::NoQuestionKind);
        }
        let Some(&id) = binding.first() else {
            return InboundDecision::Drop(DropReason::NoBinding);
        };
        if binding.iter().any(|&b| b != id) {
            return InboundDecision::Drop(DropReason::AmbiguousBinding);
        }
        let mut matches = questions.iter().filter(|q| q.id == id);
        let Some(question) = matches.next() else {
            return InboundDecision::Drop(DropReason::NoOpenQuestion);
        };
        if matches.next().is_some() {
            return InboundDecision::Drop(DropReason::AmbiguousBinding);
        }
        if question.channel != channel || question.route != route.name {
            return InboundDecision::Drop(DropReason::NotOwnedByRoute);
        }
        if question.answered {
            return InboundDecision::Drop(DropReason::AlreadyResolved);
        }
        InboundDecision::Answer {
            route,
            question: id,
        }
    }
}
