//! `ChannelPolicy::check_inbound`: accept only a routed sender's answer to
//! one open, unanswered question its route owns.

use super::{policy, spec, three_routes, BOB_SLACK, BOB_TG, JANET};
use crate::policy::{
    Channel, ChannelPolicy, DropReason, InboundDecision, MessageKind, QuestionState,
};

fn open(id: u64, channel: Channel, route: &str) -> QuestionState {
    QuestionState {
        id,
        channel,
        route: route.to_string(),
        answered: false,
    }
}

fn dropped(reason: DropReason) -> InboundDecision<'static> {
    InboundDecision::Drop(reason)
}

fn answered_by(d: InboundDecision<'_>) -> Option<(&str, u64)> {
    match d {
        InboundDecision::Answer { route, question } => Some((route.name(), question)),
        InboundDecision::Drop(_) => None,
    }
}

#[test]
fn inbound_bound_reply_to_open_question_answers() {
    let p = policy(three_routes());
    let qs = [
        open(1, Channel::Gchat, "janet"),
        open(2, Channel::Slack, "bob-dm"),
        open(3, Channel::Telegram, "bob-tg"),
    ];
    assert_eq!(
        answered_by(p.check_inbound(Channel::Gchat, "JANET@example.com", &[1], &qs)),
        Some(("janet", 1))
    );
    // One id quoted twice still binds to one question.
    assert_eq!(
        answered_by(p.check_inbound(Channel::Slack, BOB_SLACK, &[2, 2], &qs)),
        Some(("bob-dm", 2))
    );
    assert_eq!(
        answered_by(p.check_inbound(Channel::Telegram, BOB_TG, &[3], &qs)),
        Some(("bob-tg", 3))
    );
}

#[test]
fn inbound_unknown_sender_drops() {
    let p = policy(three_routes());
    // Each open question is owned by the route of a known sender, so only
    // the sender rule stands between these messages and an answer.
    let qs = [
        open(1, Channel::Gchat, "janet"),
        open(2, Channel::Slack, "bob-dm"),
        open(3, Channel::Telegram, "bob-tg"),
    ];
    let cases = [
        (Channel::Gchat, "mallory@example.com", 1),
        (Channel::Slack, "U0EVIL0001", 2),
        (Channel::Telegram, "555", 3),
        // A routed sender on the wrong channel.
        (Channel::Telegram, BOB_SLACK, 3),
        (Channel::Slack, JANET, 2),
    ];
    for (channel, sender, id) in cases {
        assert_eq!(
            p.check_inbound(channel, sender, &[id], &qs),
            dropped(DropReason::UnknownSender),
            "{channel} {sender}"
        );
    }
    // An empty policy knows no sender.
    assert_eq!(
        ChannelPolicy::default().check_inbound(Channel::Slack, BOB_SLACK, &[2], &qs),
        dropped(DropReason::UnknownSender)
    );
}

#[test]
fn inbound_no_open_question_drops() {
    let p = policy(three_routes());
    // No questions at all.
    assert_eq!(
        p.check_inbound(Channel::Slack, BOB_SLACK, &[7], &[]),
        dropped(DropReason::NoOpenQuestion)
    );
    // Questions exist, but not the bound one.
    let qs = [open(8, Channel::Slack, "bob-dm")];
    assert_eq!(
        p.check_inbound(Channel::Slack, BOB_SLACK, &[7], &qs),
        dropped(DropReason::NoOpenQuestion)
    );
    // The message binds to nothing.
    assert_eq!(
        p.check_inbound(Channel::Slack, BOB_SLACK, &[], &qs),
        dropped(DropReason::NoBinding)
    );
}

#[test]
fn second_answer_to_one_question_drops_already_resolved() {
    let p = policy(three_routes());
    let mut qs = [open(4, Channel::Slack, "bob-dm")];
    assert_eq!(
        answered_by(p.check_inbound(Channel::Slack, BOB_SLACK, &[4], &qs)),
        Some(("bob-dm", 4))
    );
    // The caller records the first answer; the second is refused.
    qs[0].answered = true;
    assert_eq!(
        p.check_inbound(Channel::Slack, BOB_SLACK, &[4], &qs),
        dropped(DropReason::AlreadyResolved)
    );
}

#[test]
fn inbound_foreign_or_ambiguous_binding_drops() {
    let p = policy(vec![
        spec(
            Channel::Slack,
            "bob-dm",
            BOB_SLACK,
            &[MessageKind::Question],
        ),
        spec(
            Channel::Slack,
            "amy-dm",
            "U0AMY0001",
            &[MessageKind::Question],
        ),
        spec(
            Channel::Gchat,
            "notices",
            JANET,
            &[MessageKind::ReviewNotice],
        ),
    ]);
    let qs = [
        open(1, Channel::Slack, "bob-dm"),
        open(2, Channel::Slack, "amy-dm"),
        open(3, Channel::Gchat, "bob-dm"),
        open(5, Channel::Slack, "bob-dm"),
        open(5, Channel::Slack, "bob-dm"),
    ];
    let cases = [
        // Bob answers Amy's question.
        (
            Channel::Slack,
            BOB_SLACK,
            vec![2],
            DropReason::NotOwnedByRoute,
        ),
        // A question with Bob's route name but on another channel.
        (
            Channel::Slack,
            BOB_SLACK,
            vec![3],
            DropReason::NotOwnedByRoute,
        ),
        // Two different questions in one message.
        (
            Channel::Slack,
            BOB_SLACK,
            vec![1, 2],
            DropReason::AmbiguousBinding,
        ),
        // The question set holds two entries for one id.
        (
            Channel::Slack,
            BOB_SLACK,
            vec![5],
            DropReason::AmbiguousBinding,
        ),
        // A route that never asks questions.
        (Channel::Gchat, JANET, vec![1], DropReason::NoQuestionKind),
    ];
    for (channel, sender, binding, reason) in cases {
        assert_eq!(
            p.check_inbound(channel, sender, &binding, &qs),
            dropped(reason),
            "{sender} {binding:?}"
        );
    }
}
