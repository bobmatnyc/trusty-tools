//! `dg.verdict.v1`: round trip and the §4.5 record rules.
#![cfg(feature = "types")]

mod common;

use common::*;
use trusty_delegation_guard::{
    Boundary, DgError, DgEvent, DgVerdict, FailureCategory, FundedBy, ModelReceipt, RefRole,
    RefSpan, RuleRef, Status, Tokens, Unavailable, UnavailableReason, Verdict,
};

fn ok_verdict(event: &DgEvent, verdict: Verdict) -> DgVerdict {
    let rule = &event.policy.rules[0];
    DgVerdict {
        event_id: event.event_id,
        dedupe_key: event.dedupe_key.clone(),
        prompt_sha256: trusty_delegation_guard::Sha256Hex::digest(b"prompt-file"),
        model: ModelReceipt {
            alias: "pinned".to_owned(),
            provider: "bedrock".to_owned(),
            id: "model-synthetic".to_owned(),
            account: "000000000000".to_owned(),
            funded_by: FundedBy::Ours,
        },
        status: Status::Ok,
        unavailable: None,
        verdict: Some(verdict),
        rule_refs: vec![RuleRef {
            rule_id: rule.rule_id.clone(),
            span: RefSpan {
                source_ref: rule.source_ref.clone(),
                start: rule.span.start,
                len: rule.span.len,
            },
            role: RefRole::Satisfied,
        }],
        failure_category: None,
        mismatch: String::new(),
        evidence_needed: Vec::new(),
        latency_ms: 6000,
        tokens: Tokens {
            input: 900,
            out: 80,
        },
    }
}

#[test]
fn verdict_round_trips_through_json() {
    let v = ok_verdict(&alpha_event(Boundary::Contract), Verdict::Allow);
    let json = v.to_json().expect("json");
    assert!(json.contains("\"in\":900"), "tokens.in wire name");
    assert!(json.contains("\"funded_by\":\"ours\""));
    assert_eq!(DgVerdict::from_json(&json).expect("parse"), v);
}

#[test]
fn unknown_verdict_key_is_rejected() {
    let json = ok_verdict(&alpha_event(Boundary::Contract), Verdict::Allow)
        .to_json()
        .expect("json");
    for edit in [
        |v: &mut serde_json::Value| v["replacement_policy"] = serde_json::json!("x"),
        |v: &mut serde_json::Value| v["model"]["extra"] = serde_json::json!(1),
        |v: &mut serde_json::Value| v["model"]["funded_by"] = serde_json::json!("recipient"),
    ] {
        let bad = edit_json(&json, edit);
        assert!(matches!(DgVerdict::from_json(&bad), Err(DgError::Parse(_))));
    }
}

#[test]
fn allow_with_failure_category_is_rejected() {
    let mut v = ok_verdict(&alpha_event(Boundary::Contract), Verdict::Allow);
    v.failure_category = Some(FailureCategory::ScopeLost);
    assert_eq!(
        v.validate(),
        Err(DgError::MalformedVerdict("allow with failure_category"))
    );
}

#[test]
fn revise_without_rule_ref_is_rejected() {
    let mut v = ok_verdict(&alpha_event(Boundary::Contract), Verdict::Revise);
    v.failure_category = Some(FailureCategory::NestedCapOmitted);
    assert!(v.validate().is_ok(), "revise with a citation is valid");
    v.rule_refs.clear();
    assert_eq!(
        v.validate(),
        Err(DgError::MalformedVerdict("revise without rule_ref"))
    );
}

#[test]
fn status_and_verdict_must_agree() {
    let event = alpha_event(Boundary::Contract);
    let mut v = ok_verdict(&event, Verdict::Allow);
    v.status = Status::Unavailable;
    assert!(v.validate().is_err(), "unavailable with a verdict");

    v.verdict = None;
    v.unavailable = Some(Unavailable {
        reason: UnavailableReason::Timeout,
    });
    assert!(v.validate().is_err(), "unavailable with citations");
    v.rule_refs.clear();
    assert!(v.validate().is_ok());

    let mut v = ok_verdict(&event, Verdict::Allow);
    v.verdict = None;
    assert!(v.validate().is_err(), "ok without a verdict");

    let mut v = ok_verdict(&event, Verdict::Allow);
    v.mismatch = "m".repeat(301);
    assert!(v.validate().is_err(), "mismatch over 300 chars");
}

#[test]
fn verdict_citing_foreign_rule_is_rejected() {
    let event = alpha_event(Boundary::Contract);
    let mut v = ok_verdict(&event, Verdict::Revise);
    assert!(v.validate_against(&event).is_ok());
    v.rule_refs[0].rule_id = "beta-cap-1".to_owned();
    assert_eq!(
        v.validate_against(&event),
        Err(DgError::MalformedVerdict("citation not in bundle"))
    );
    let mut v = ok_verdict(&event, Verdict::Allow);
    v.rule_refs[0].span.len += 1;
    assert!(v.validate_against(&event).is_err(), "span mismatch");
    let other = alpha_event(Boundary::Contract);
    assert!(
        ok_verdict(&other, Verdict::Allow)
            .validate_against(&event)
            .is_err(),
        "verdict for another event"
    );
}
