//! `ChannelPolicy::check_egress`: deny unless a route names the exact
//! (channel, recipient) and lists the kind.

use super::{policy, spec, three_routes, BOB_SLACK, BOB_TG, JANET};
use crate::policy::{Channel, ChannelPolicy, DenyReason, EgressDecision, MessageKind, PolicySpec};

use MessageKind::{Question, ReviewNotice};

fn allowed_route(d: EgressDecision<'_>) -> Option<&str> {
    match d {
        EgressDecision::Allow(r) => Some(r.name()),
        EgressDecision::Deny(_) => None,
    }
}

#[test]
fn exact_route_and_listed_kind_allow() {
    let p = policy(three_routes());
    let cases = [
        (Channel::Gchat, JANET, Question, "janet"),
        (Channel::Gchat, " Janet@EXAMPLE.com ", ReviewNotice, "janet"),
        (Channel::Slack, BOB_SLACK, Question, "bob-dm"),
        (Channel::Telegram, BOB_TG, Question, "bob-tg"),
    ];
    for (channel, to, kind, name) in cases {
        assert_eq!(
            allowed_route(p.check_egress(channel, to, kind)),
            Some(name),
            "{channel} {to} {kind}"
        );
    }
}

#[test]
fn unknown_route_denies() {
    let p = policy(three_routes());
    let cases = [
        // Not named at all.
        (Channel::Slack, "U0OTHER99"),
        (Channel::Telegram, "987654321"),
        (Channel::Gchat, "mallory@example.com"),
        // A prefix or an extension of a routed recipient.
        (Channel::Slack, "U0ABCDEF"),
        (Channel::Slack, "U0ABCDEF12"),
        (Channel::Telegram, "12345678"),
        (Channel::Gchat, "janet@example.co"),
        // Slack and Telegram ids are exact: no case folding, no trimming.
        (Channel::Slack, "u0abcdef1"),
        (Channel::Telegram, " 123456789"),
        // A routed recipient on another channel.
        (Channel::Telegram, BOB_SLACK),
        (Channel::Slack, BOB_TG),
        (Channel::Slack, JANET),
        // A wildcard is a literal string, not a pattern.
        (Channel::Slack, "*"),
        (Channel::Gchat, "*@example.com"),
    ];
    for (channel, to) in cases {
        assert_eq!(
            p.check_egress(channel, to, Question),
            EgressDecision::Deny(DenyReason::NoRoute),
            "{channel} {to:?}"
        );
    }
}

#[test]
fn kind_not_listed_denies() {
    let p = policy(vec![
        spec(Channel::Gchat, "q-only", JANET, &[Question]),
        spec(
            Channel::Gchat,
            "notice-only",
            "bob@example.com",
            &[ReviewNotice],
        ),
        spec(Channel::Slack, "bob-dm", BOB_SLACK, &[Question]),
    ]);
    let cases = [
        (Channel::Gchat, JANET, ReviewNotice),
        (Channel::Gchat, "bob@example.com", Question),
        (Channel::Slack, BOB_SLACK, ReviewNotice),
    ];
    for (channel, to, kind) in cases {
        assert_eq!(
            p.check_egress(channel, to, kind),
            EgressDecision::Deny(DenyReason::KindNotAllowed),
            "{channel} {to} {kind}"
        );
    }
}

#[test]
fn empty_policy_denies_all() {
    let built = ChannelPolicy::build(PolicySpec::default()).expect("empty spec builds");
    for p in [ChannelPolicy::default(), built] {
        assert!(p.is_empty());
        for channel in Channel::ALL {
            for kind in MessageKind::ALL {
                for to in [JANET, BOB_SLACK, BOB_TG, "", "*"] {
                    assert_eq!(
                        p.check_egress(channel, to, kind),
                        EgressDecision::Deny(DenyReason::NoPolicy),
                        "{channel} {to:?} {kind}"
                    );
                }
            }
        }
    }
}
