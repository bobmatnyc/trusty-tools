//! Policy bundles and per-project isolation (design §3 test 5, type-level part).
#![cfg(feature = "types")]

mod common;

use common::*;
use trusty_delegation_guard::{
    Authority, Boundary, BundleBuilder, DgError, DgEvent, PolicyBundle, ProjectId,
    RULE_TEXT_MAX_BYTES, RuleScope, Sha256Hex,
};

const SHARED_PHRASE: &str = "nested workers";

/// Two projects with conflicting caps and one shared synthetic phrase.
fn conflicting_bundles() -> (PolicyBundle, PolicyBundle) {
    let mut a = BundleBuilder::new(project("alpha"));
    a.add_rule(source(
        "alpha",
        "alpha-cap-1",
        "ledger/alpha/0001",
        "Start at most 2 nested workers.",
    ))
    .expect("alpha rule");
    let mut b = BundleBuilder::new(project("beta"));
    b.add_rule(source(
        "beta",
        "beta-cap-1",
        "ledger/beta/0001",
        "Start at most 5 nested workers.",
    ))
    .expect("beta rule");
    (a.build().expect("alpha"), b.build().expect("beta"))
}

#[test]
fn bundle_for_a_from_b_input_is_refused() {
    let mut a = BundleBuilder::new(project("alpha"));
    let err = a
        .add_rule(source(
            "beta",
            "beta-cap-1",
            "ledger/beta/0001",
            "Start at most 5 nested workers.",
        ))
        .expect_err("foreign rule must be refused");
    assert_eq!(
        err,
        DgError::ForeignProject {
            expected: "alpha".to_owned(),
            found: "beta".to_owned()
        }
    );
    let bundle = a.build().expect("empty bundle still builds");
    assert!(
        bundle.rules.is_empty(),
        "refused rule leaked into the bundle"
    );
}

#[test]
fn event_for_a_with_b_bundle_is_refused() {
    let (_, beta) = conflicting_bundles();
    let err = DgEvent::capture(&project("alpha"), &beta, input(Boundary::Contract, "p"))
        .expect_err("foreign bundle must be refused");
    assert!(matches!(err, DgError::ForeignProject { .. }));
}

#[test]
fn conflicting_projects_keep_their_own_spans() {
    let (alpha, beta) = conflicting_bundles();
    let prompt = format!("Plan the work; use {SHARED_PHRASE} as needed.");
    let ea = DgEvent::capture(
        &project("alpha"),
        &alpha,
        input(Boundary::Contract, &prompt),
    )
    .expect("alpha event");
    let eb = DgEvent::capture(&project("beta"), &beta, input(Boundary::Contract, &prompt))
        .expect("beta event");
    let ja = ea.to_json().expect("a json");
    let jb = eb.to_json().expect("b json");
    assert!(ja.contains("at most 2") && !ja.contains("at most 5"));
    assert!(jb.contains("at most 5") && !jb.contains("at most 2"));
    assert!(!ja.contains("beta-cap-1") && !jb.contains("alpha-cap-1"));
    assert_ne!(ea.dedupe_key, eb.dedupe_key);
}

#[test]
fn fleet_rule_needs_owner_authority() {
    let mut b = BundleBuilder::new(project("alpha"));
    let mut fleet = source("alpha", "fleet-1", "doc87/fleet/1", "No pushes on Fridays.");
    fleet.scope = RuleScope::Fleet;
    fleet.authority = Authority::ArchitectNote;
    assert!(matches!(
        b.add_rule(fleet.clone()),
        Err(DgError::Inconsistent(_))
    ));
    fleet.authority = Authority::OwnerVerified;
    b.add_rule(fleet).expect("owner fleet rule enters");
    assert_eq!(b.build().expect("bundle").rules.len(), 1);
}

#[test]
fn bundle_round_trips_through_json() {
    let (alpha, _) = conflicting_bundles();
    let json = serde_json::to_string(&alpha).expect("json");
    assert_eq!(PolicyBundle::from_json(&json).expect("parse"), alpha);
    let bad = edit_json(&json, |v| v["extra"] = serde_json::json!(true));
    assert!(matches!(
        PolicyBundle::from_json(&bad),
        Err(DgError::Parse(_))
    ));
}

#[test]
fn policy_rev_mismatch_is_rejected() {
    let (alpha, _) = conflicting_bundles();
    let json = serde_json::to_string(&alpha).expect("json");
    let bad = edit_json(&json, |v| {
        v["rules"][0]["text"] = serde_json::json!("Start at most 9 nested workers.");
    });
    assert_eq!(
        PolicyBundle::from_json(&bad),
        Err(DgError::DerivedMismatch("policy_rev"))
    );
}

#[test]
fn rule_limits_are_enforced() {
    let long = "z".repeat(RULE_TEXT_MAX_BYTES * 2);
    let mut b = BundleBuilder::new(project("alpha"));
    b.add_rule(source("alpha", "long-1", "ledger/alpha/9", &long))
        .expect("long rule");
    let bundle = b.build().expect("bundle");
    let rule = &bundle.rules[0];
    assert_eq!(rule.text.len(), RULE_TEXT_MAX_BYTES);
    assert_eq!(rule.span.len, long.len() as u64);
    assert_eq!(rule.span_sha256, Sha256Hex::digest(long.as_bytes()));

    let ids: Vec<String> = (0..41).map(|i| format!("r{i}")).collect();
    let mut b = BundleBuilder::new(project("alpha"));
    for id in &ids[..40] {
        b.add_rule(source("alpha", id, "ledger/alpha/1", "rule"))
            .expect("within limit");
    }
    assert!(matches!(
        b.add_rule(source("alpha", &ids[40], "ledger/alpha/1", "rule")),
        Err(DgError::LimitExceeded(_))
    ));

    let mut b = BundleBuilder::new(project("alpha"));
    b.add_rule(source("alpha", "dup", "ledger/alpha/1", "one"))
        .expect("first");
    b.add_rule(source("alpha", "dup", "ledger/alpha/2", "two"))
        .expect("second accepted by add_rule");
    assert!(matches!(b.build(), Err(DgError::Inconsistent(_))));
}

#[test]
fn project_id_rejects_unsafe_values() {
    for bad in ["", ".", "..", "Alpha", "a/b", "a b", &"a".repeat(129)] {
        assert_eq!(
            ProjectId::new(bad),
            Err(DgError::InvalidProjectId),
            "{bad:?}"
        );
    }
    for good in ["alpha", "beta-2", "org.repo_x"] {
        assert!(ProjectId::new(good).is_ok(), "{good:?}");
    }
}
