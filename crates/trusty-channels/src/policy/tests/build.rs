//! `ChannelPolicy::build` load rules: every broken rule fails the build.

use super::{limit, policy, spec, three_routes, BOB_SLACK, BOB_TG, JANET};
use crate::policy::{
    Channel, ChannelPolicy, MessageKind, PolicyError, PolicySpec, RateLimit, RouteSpec,
};

use MessageKind::{Question, ReviewNotice};

fn build(routes: Vec<RouteSpec>) -> Result<ChannelPolicy, PolicyError> {
    ChannelPolicy::build(PolicySpec {
        rate_limit: None,
        routes,
    })
}

#[test]
fn overlapping_routes_fail_build_and_name_both() {
    // Same (channel, recipient), different names.
    let err = build(vec![
        spec(Channel::Slack, "a", BOB_SLACK, &[Question]),
        spec(Channel::Slack, "b", BOB_SLACK, &[Question]),
    ])
    .expect_err("shared recipient must fail");
    assert_eq!(
        err,
        PolicyError::Duplicate {
            first: r#"routes[0] slack "a""#.into(),
            second: r#"routes[1] slack "b""#.into(),
            field: "recipient",
            value: BOB_SLACK.into(),
        }
    );
    let msg = err.to_string();
    assert!(msg.contains(r#"routes[0] slack "a""#) && msg.contains(r#"routes[1] slack "b""#));

    // A gchat recipient differing only in case is the same recipient.
    let err = build(vec![
        spec(Channel::Gchat, "j1", JANET, &[Question]),
        spec(Channel::Gchat, "j2", "Janet@Example.COM", &[Question]),
    ])
    .expect_err("case-folded gchat recipient must fail");
    assert!(matches!(
        err,
        PolicyError::Duplicate {
            field: "recipient",
            ..
        }
    ));

    // Same name on two channels.
    let err = build(vec![
        spec(Channel::Slack, "bob", BOB_SLACK, &[Question]),
        spec(Channel::Telegram, "bob", BOB_TG, &[Question]),
    ])
    .expect_err("shared name must fail");
    assert_eq!(
        err,
        PolicyError::Duplicate {
            first: r#"routes[0] slack "bob""#.into(),
            second: r#"routes[1] telegram "bob""#.into(),
            field: "name",
            value: "bob".into(),
        }
    );
}

#[test]
fn empty_kinds_fails_build() {
    let err = build(vec![spec(Channel::Slack, "bob-dm", BOB_SLACK, &[])])
        .expect_err("empty kinds must fail");
    assert_eq!(
        err,
        PolicyError::EmptyKinds {
            entry: r#"routes[0] slack "bob-dm""#.into()
        }
    );
}

#[test]
fn review_notice_on_slack_or_telegram_fails_build() {
    for (channel, recipient) in [(Channel::Slack, BOB_SLACK), (Channel::Telegram, BOB_TG)] {
        let err = build(vec![spec(
            channel,
            "r",
            recipient,
            &[Question, ReviewNotice],
        )])
        .expect_err("review_notice off gchat must fail");
        assert_eq!(
            err,
            PolicyError::KindNotOnChannel {
                entry: format!(r#"routes[0] {channel} "r""#),
                kind: ReviewNotice,
                channel,
            }
        );
    }
    // gchat carries it.
    assert!(build(vec![spec(Channel::Gchat, "r", JANET, &[ReviewNotice])]).is_ok());
}

#[test]
fn invalid_recipient_or_name_fails_build() {
    let bad = [
        spec(Channel::Gchat, "r", "not-an-email", &[Question]),
        spec(Channel::Slack, "r", "C0CHANNEL1", &[Question]),
        spec(Channel::Slack, "r", "D0DMCHAN1", &[Question]),
        spec(Channel::Slack, "r", "u0lower1", &[Question]),
        spec(Channel::Telegram, "r", "@bob", &[Question]),
        spec(Channel::Telegram, "r", "-100123456", &[Question]),
        spec(Channel::Telegram, "r", "0123", &[Question]),
        spec(Channel::Telegram, "r", "", &[Question]),
        spec(Channel::Slack, "bad name", BOB_SLACK, &[Question]),
        spec(Channel::Slack, "", BOB_SLACK, &[Question]),
    ];
    for s in bad {
        let label = format!("{s:?}");
        let err = build(vec![s]).expect_err(&label);
        assert!(
            matches!(err, PolicyError::InvalidRoute { .. }),
            "{label}: {err}"
        );
    }
}

#[test]
fn bucket_zero_or_nan_params_fail_build() {
    let bad = [
        limit(0, 1.0),
        limit(-1, 1.0),
        limit(10_001, 1.0),
        limit(10, 0.0),
        limit(10, -1.0),
        limit(10, f64::NAN),
        limit(10, f64::INFINITY),
        limit(10, 10_000.5),
    ];
    for l in bad {
        // As the policy default.
        let err = ChannelPolicy::build(PolicySpec {
            rate_limit: Some(l),
            routes: Vec::new(),
        })
        .expect_err(&format!("default {l:?} must fail"));
        assert!(
            matches!(&err, PolicyError::InvalidRateLimit { entry, .. } if entry == "rate_limit"),
            "default {l:?}: {err}"
        );
        // As a route's own limit.
        let mut route = spec(Channel::Slack, "bob-dm", BOB_SLACK, &[Question]);
        route.rate_limit = Some(l);
        let err = build(vec![route]).expect_err(&format!("route {l:?} must fail"));
        assert!(
            matches!(err, PolicyError::InvalidRateLimit { .. }),
            "route {l:?}: {err}"
        );
    }
}

#[test]
fn route_cannot_raise_rate_limit() {
    for (raised, field) in [
        (limit(101, 1.0), "capacity"),
        (limit(50, 2.0), "refill_per_sec"),
    ] {
        let mut route = spec(Channel::Slack, "bob-dm", BOB_SLACK, &[Question]);
        route.rate_limit = Some(raised);
        let err = build(vec![route]).expect_err("a raised limit must fail");
        assert!(
            matches!(&err, PolicyError::RateLimitRaised { field: f, .. } if *f == field),
            "{err}"
        );
    }
    // Lower in both parameters is accepted and becomes the route's limit.
    let mut route = spec(Channel::Slack, "bob-dm", BOB_SLACK, &[Question]);
    route.rate_limit = Some(limit(10, 0.5));
    let p = policy(vec![route]);
    assert_eq!(p.routes()[0].rate_limit().capacity(), 10);
    assert_eq!(p.routes()[0].rate_limit().refill_per_sec(), 0.5);
}

#[test]
fn absent_rate_limit_uses_builtin_not_unlimited() {
    let p = policy(three_routes());
    assert_eq!(p.default_rate_limit(), RateLimit::DEFAULT);
    assert_eq!(p.default_rate_limit().capacity(), 100);
    assert!((p.default_rate_limit().refill_per_sec() * 60.0 - 100.0).abs() < 1e-9);
    for r in p.routes() {
        assert_eq!(r.rate_limit(), RateLimit::DEFAULT, "{}", r.name());
    }
}
