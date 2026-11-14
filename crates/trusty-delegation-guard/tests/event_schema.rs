//! `dg.event.v1`: round trip, strict parsing, privacy and derived-field checks.
#![cfg(feature = "types")]

mod common;

use common::*;
use trusty_delegation_guard::{Boundary, DgError, DgEvent, Sha256Hex, hash, redact};

#[test]
fn event_round_trips_through_json() {
    for boundary in [Boundary::Contract, Boundary::Launch] {
        let event = alpha_event(boundary);
        let json = event.to_json().expect("serialize");
        let back = DgEvent::from_json(&json).expect("parse");
        assert_eq!(back, event);
        assert_eq!(back.schema, "dg.event.v1");
        assert_eq!(back.event_id.get_version_num(), 7);
    }
}

#[test]
fn unknown_top_level_key_is_rejected() {
    let json = alpha_event(Boundary::Contract)
        .to_json()
        .expect("serialize");
    let bad = edit_json(&json, |v| {
        v["extra_field"] = serde_json::json!("x");
    });
    let err = DgEvent::from_json(&bad).expect_err("unknown key must fail");
    assert!(
        matches!(&err, DgError::Parse(m) if m.contains("unknown field")),
        "{err:?}"
    );
}

#[test]
fn unknown_nested_key_is_rejected() {
    let json = alpha_event(Boundary::Launch).to_json().expect("serialize");
    for path in [
        "actor",
        "proposed",
        "policy",
        "worker_state",
        "evidence_hashes",
    ] {
        let bad = edit_json(&json, |v| {
            v[path]["sneaky"] = serde_json::json!(1);
        });
        assert!(
            matches!(DgEvent::from_json(&bad), Err(DgError::Parse(_))),
            "{path} accepted an unknown key"
        );
    }
    let bad = edit_json(&json, |v| {
        v["policy"]["rules"][0]["sneaky"] = serde_json::json!(1);
    });
    assert!(matches!(DgEvent::from_json(&bad), Err(DgError::Parse(_))));
}

#[test]
fn stored_event_carries_no_raw_ids() {
    let event = alpha_event(Boundary::Launch);
    let json = event.to_json().expect("serialize");
    assert!(!json.contains(SESSION_ID), "raw session_id stored");
    assert!(!json.contains(TOOL_USE_ID), "raw tool_use_id stored");
    assert!(!json.contains("\"session_id\""));
    assert!(!json.contains("\"tool_use_id\""));
    assert_eq!(event.action_id, hash::action_id(SESSION_ID, TOOL_USE_ID));
    assert_eq!(event.actor.session_hash, hash::session_hash(SESSION_ID));
}

#[test]
fn redaction_hit_sets_redacted_flag() {
    let clean = alpha_event(Boundary::Contract);
    assert!(!clean.proposed.redacted);

    let prompt = "Deploy with key AKIAABCDEFGHIJKLMNOP then report.";
    let event = DgEvent::capture(
        &project("alpha"),
        &alpha_bundle(),
        input(Boundary::Contract, prompt),
    )
    .expect("event");
    assert!(event.proposed.redacted);
    assert!(
        !event
            .proposed
            .prompt_excerpt
            .contains("AKIAABCDEFGHIJKLMNOP")
    );
    assert_eq!(
        event.proposed.prompt_sha256,
        Sha256Hex::digest(prompt.as_bytes())
    );

    let mut inp = input(Boundary::Contract, "plain prompt");
    inp.description = "use token=supersecretvalue123";
    let event = DgEvent::capture(&project("alpha"), &alpha_bundle(), inp).expect("event");
    assert!(event.proposed.redacted, "description hit must set redacted");
    assert!(!event.proposed.description.contains("supersecretvalue123"));
}

#[test]
fn long_prompt_is_capped_and_flagged() {
    let prompt = "x".repeat(redact::EXCERPT_MAX_BYTES + 100);
    let event = DgEvent::capture(
        &project("alpha"),
        &alpha_bundle(),
        input(Boundary::Contract, &prompt),
    )
    .expect("event");
    assert_eq!(
        event.proposed.prompt_excerpt.len(),
        redact::EXCERPT_MAX_BYTES
    );
    assert!(event.proposed.truncated);
    assert_eq!(event.proposed.prompt_bytes, prompt.len() as u64);
}

#[test]
fn oversized_excerpt_is_rejected() {
    let json = alpha_event(Boundary::Contract)
        .to_json()
        .expect("serialize");
    let bad = edit_json(&json, |v| {
        v["proposed"]["prompt_excerpt"] =
            serde_json::json!("y".repeat(redact::EXCERPT_MAX_BYTES + 1));
    });
    assert!(matches!(
        DgEvent::from_json(&bad),
        Err(DgError::LimitExceeded(_))
    ));
}

#[test]
fn unredacted_secret_fails_validation() {
    let event = alpha_event(Boundary::Contract);
    assert!(event.validate().is_ok(), "captured event validates");
    let json = event.to_json().expect("serialize");
    for (field, secret) in [
        ("prompt_excerpt", "deploy with AKIAABCDEFGHIJKLMNOP"),
        ("description", "API_TOKEN=tok0synthetic0value"),
    ] {
        let bad = edit_json(&json, |v| {
            v["proposed"][field] = serde_json::json!(secret);
            v["proposed"]["redacted"] = serde_json::json!(false);
        });
        assert_eq!(
            DgEvent::from_json(&bad),
            Err(DgError::Unredacted(field)),
            "{field}"
        );
    }
}

#[test]
fn tampered_dedupe_key_is_rejected() {
    let json = alpha_event(Boundary::Contract)
        .to_json()
        .expect("serialize");
    let bad = edit_json(&json, |v| {
        v["dedupe_key"] = serde_json::json!(Sha256Hex::digest(b"other").as_str());
    });
    assert_eq!(
        DgEvent::from_json(&bad),
        Err(DgError::DerivedMismatch("dedupe_key"))
    );
}

#[test]
fn worker_state_must_match_boundary() {
    let mut inp = input(Boundary::Contract, "p");
    inp.worker_state = Some(worker_state(1));
    assert!(matches!(
        DgEvent::capture(&project("alpha"), &alpha_bundle(), inp),
        Err(DgError::Inconsistent(_))
    ));
    let mut inp = input(Boundary::Launch, "p");
    inp.worker_state = None;
    assert!(matches!(
        DgEvent::capture(&project("alpha"), &alpha_bundle(), inp),
        Err(DgError::Inconsistent(_))
    ));
}

#[test]
fn inherit_must_name_a_bundle_rule() {
    let mut inp = input(Boundary::Contract, "p");
    inp.inherits = vec![trusty_delegation_guard::Inherit {
        rule_id: "not-in-bundle".to_owned(),
        via: trusty_delegation_guard::InheritVia::AgentDef,
        verified: true,
        evidence_sha256: Sha256Hex::digest(b"e"),
    }];
    assert!(matches!(
        DgEvent::capture(&project("alpha"), &alpha_bundle(), inp),
        Err(DgError::Inconsistent(_))
    ));
}

#[test]
fn wrong_schema_version_is_rejected() {
    let json = alpha_event(Boundary::Contract)
        .to_json()
        .expect("serialize");
    let bad = edit_json(&json, |v| v["schema"] = serde_json::json!("dg.event.v2"));
    assert!(matches!(
        DgEvent::from_json(&bad),
        Err(DgError::SchemaMismatch { .. })
    ));
}
