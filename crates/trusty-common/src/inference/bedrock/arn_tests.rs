//! Tests for Bedrock model ARN parsing and account masking (#9200; moved
//! from trusty-review with the parser, #10892).
//!
//! Routing, pricing and PR-footer masking stay in trusty-review's
//! `llm/bedrock/arn_tests.rs`, which drive those crate-local paths.

use super::{ArnKind, BedrockArn, is_unpriced, mask_account_ids, parse};

/// Shaped like the owner-ruling-59 pilot profile; the account id is AWS's
/// documentation placeholder, not a real account.
const APP_PROFILE_ARN: &str =
    "arn:aws:bedrock:us-west-2:111122223333:application-inference-profile/9iatxd8u1751";
const INFERENCE_PROFILE_ARN: &str =
    "arn:aws:bedrock:us-east-2:111122223333:inference-profile/us.anthropic.claude-sonnet-4-6";
/// The documentation placeholder account id the ARNs above carry.
const ACCOUNT: &str = "111122223333";
const MASKED_APP_PROFILE_ARN: &str =
    "arn:aws:bedrock:us-west-2:****:application-inference-profile/9iatxd8u1751";
const FOUNDATION_MODEL_ARN: &str =
    "arn:aws:bedrock:eu-central-1::foundation-model/anthropic.claude-haiku-4-5-20251001-v1:0";

#[test]
fn parse_reads_region_kind_and_name_from_each_arn_form() {
    assert_eq!(
        parse(APP_PROFILE_ARN),
        Some(BedrockArn {
            region: "us-west-2",
            kind: ArnKind::ApplicationInferenceProfile,
            name: "9iatxd8u1751",
        })
    );
    assert_eq!(
        parse(INFERENCE_PROFILE_ARN),
        Some(BedrockArn {
            region: "us-east-2",
            kind: ArnKind::InferenceProfile,
            name: "us.anthropic.claude-sonnet-4-6",
        })
    );
    assert_eq!(
        parse(FOUNDATION_MODEL_ARN),
        Some(BedrockArn {
            region: "eu-central-1",
            kind: ArnKind::FoundationModel,
            name: "anthropic.claude-haiku-4-5-20251001-v1:0",
        })
    );
}

#[test]
fn parse_rejects_plain_ids_and_malformed_arns() {
    for id in [
        "claude-sonnet-5-5",
        "us.anthropic.claude-sonnet-4-6",
        "anthropic/claude-sonnet-4.6",
        "accounts/fireworks/models/llama-v3p1-70b-instruct",
        "arn:aws:bedrock:us-west-2:111122223333:agent/AGENT12345",
        "arn:aws:bedrock:us-west-2:111122223333:application-inference-profile/",
        "arn:aws:bedrock:us-west-2:12345:application-inference-profile/abc",
        "arn:aws-us-gov:bedrock:us-gov-west-1:111122223333:application-inference-profile/abc",
        "bedrock/arn:aws:bedrock:us-west-2:111122223333:application-inference-profile/abc",
        " arn:aws:bedrock:us-west-2:111122223333:application-inference-profile/abc",
    ] {
        assert_eq!(parse(id), None, "{id:?}");
    }
}

// ── Account-id masking (#9200, architect ruling 2026-10-05) ──────────────

#[test]
fn mask_hides_the_account_of_every_arn_form() {
    assert_eq!(mask_account_ids(APP_PROFILE_ARN), MASKED_APP_PROFILE_ARN);
    assert_eq!(
        mask_account_ids(INFERENCE_PROFILE_ARN),
        "arn:aws:bedrock:us-east-2:****:inference-profile/us.anthropic.claude-sonnet-4-6"
    );
    // A foundation-model ARN has no account id, so nothing changes.
    assert_eq!(mask_account_ids(FOUNDATION_MODEL_ARN), FOUNDATION_MODEL_ARN);
    // Inside longer text, every ARN is masked, including ones AWS quotes back.
    let text =
        format!("User: arn:aws:sts::{ACCOUNT}:assumed-role/r/s cannot invoke {APP_PROFILE_ARN}.");
    assert_eq!(
        mask_account_ids(&text),
        format!("User: arn:aws:sts::****:assumed-role/r/s cannot invoke {MASKED_APP_PROFILE_ARN}.")
    );
}

#[test]
fn mask_leaves_plain_ids_and_non_arn_text_unchanged() {
    for text in [
        "",
        "claude-sonnet-5-5",
        "us.anthropic.claude-sonnet-4-6",
        "anthropic/claude-sonnet-4.6",
        "accounts/fireworks/models/llama-v3p1-70b-instruct",
        "order 111122223333 shipped",
        "the arn: field is empty",
    ] {
        assert_eq!(mask_account_ids(text), text, "{text:?}");
    }
}

#[test]
fn mask_survives_malformed_and_truncated_input() {
    for text in [
        "arn:",
        "arn:aws",
        "arn:aws:bedrock:us-west-2:",
        "arn:aws:bedrock:us-west-2:11112222333",
        "arn:aws:bedrock:us-west-2:1111222233334:x/y",
        "arn:aws:bedrock:us-west-2:11112222333a:x/y",
        "arn:aws:bedrock:US-WEST-2:111122223333:x/y",
        "arnarn:arn:arn:",
        "arn:aws:bedrock:us-west-2:111122223333",
        "arn:\u{e9}:bedrock:us-west-2:111122223333:x/y",
    ] {
        assert_eq!(mask_account_ids(text), text, "{text:?}");
    }
    // A truncated second ARN does not undo the mask on the first.
    let text = format!("{APP_PROFILE_ARN} arn:aws:bedrock:us-west-2:1111");
    assert_eq!(
        mask_account_ids(&text),
        format!("{MASKED_APP_PROFILE_ARN} arn:aws:bedrock:us-west-2:1111")
    );
}

#[test]
fn priced_model_names_the_embedded_model_or_none() {
    let priced = |id| parse(id).expect("a valid ARN").priced_model();
    assert_eq!(priced(APP_PROFILE_ARN), None);
    assert_eq!(
        priced(INFERENCE_PROFILE_ARN),
        Some("us.anthropic.claude-sonnet-4-6")
    );
    assert_eq!(
        priced(FOUNDATION_MODEL_ARN),
        Some("anthropic.claude-haiku-4-5-20251001-v1:0")
    );
}

#[test]
fn is_unpriced_only_for_application_profiles() {
    assert!(is_unpriced(APP_PROFILE_ARN));
    for id in [
        INFERENCE_PROFILE_ARN,
        FOUNDATION_MODEL_ARN,
        "us.anthropic.claude-sonnet-4-6",
    ] {
        assert!(!is_unpriced(id), "{id}");
    }
}
