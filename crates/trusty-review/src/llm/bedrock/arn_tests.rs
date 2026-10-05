//! Tests for Bedrock model ARNs as trusty-review model ids (#9200).
//!
//! Routing goes through `build_provider`, the one factory the CLI, config and
//! MCP `reviewer_model` paths share; no test reaches AWS, because
//! `BedrockProvider` builds its client lazily on the first Converse call.

use std::sync::{Arc, Mutex};

use super::{ArnKind, BedrockArn, parse};
use crate::config::{Provider, ReviewConfig};
use crate::llm::bedrock::{BedrockProvider, estimate_bedrock_cost_usd};
use crate::llm::{build_provider, resolve_model};

/// Shaped like the owner-ruling-59 pilot profile; the account id is AWS's
/// documentation placeholder, not a real account.
const APP_PROFILE_ARN: &str =
    "arn:aws:bedrock:us-west-2:111122223333:application-inference-profile/9iatxd8u1751";
const INFERENCE_PROFILE_ARN: &str =
    "arn:aws:bedrock:us-east-2:111122223333:inference-profile/us.anthropic.claude-sonnet-4-6";
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

/// Every route an operator can name an ARN by must build the Bedrock provider,
/// whatever the configured default provider is.
async fn assert_builds_bedrock(arn: &str) {
    let config = ReviewConfig::load(None);
    let prefixed = format!("bedrock/{arn}");
    for requested in [arn, prefixed.as_str()] {
        for default in [Provider::OpenRouter, Provider::Fireworks, Provider::Bedrock] {
            let resolution = resolve_model(requested, &default).expect("an ARN resolves");
            assert_eq!(
                resolution.provider,
                Provider::Bedrock,
                "{requested} / {default}"
            );
            assert_eq!(resolution.model, arn, "the ARN reaches Converse verbatim");
            assert_eq!(resolution.note, None, "an ARN on Bedrock is no adjustment");

            let provider = build_provider(requested, &default, &config)
                .await
                .unwrap_or_else(|e| panic!("{requested} / {default}: {e}"));
            assert_eq!(provider.name(), "bedrock", "{requested} / {default}");
        }
    }
}

/// #9200: before ARN support, `BedrockProvider::new` rejected every ARN for
/// lacking a `us.`-style prefix, so `build_provider` failed on all three forms.
#[tokio::test]
async fn application_inference_profile_arn_routes_to_bedrock() {
    assert_builds_bedrock(APP_PROFILE_ARN).await;
}

#[tokio::test]
async fn inference_profile_arn_routes_to_bedrock() {
    assert_builds_bedrock(INFERENCE_PROFILE_ARN).await;
}

#[tokio::test]
async fn foundation_model_arn_routes_to_bedrock() {
    assert_builds_bedrock(FOUNDATION_MODEL_ARN).await;
}

/// Precedence: explicit region > the ARN's region > the ambient env walk.
/// Every assertion is env-independent: the ARN tier sits above both env vars.
#[test]
fn arn_region_wins_over_ambient_and_explicit_region_wins_over_arn() {
    for (arn, region) in [
        (APP_PROFILE_ARN, "us-west-2"),
        (INFERENCE_PROFILE_ARN, "us-east-2"),
        (FOUNDATION_MODEL_ARN, "eu-central-1"),
    ] {
        let ambient = BedrockProvider::new(arn).expect("an ARN is a valid Bedrock id");
        assert_eq!(ambient.region(), region, "{arn}");

        let blank = BedrockProvider::new_in_region(arn, Some("  ")).expect("valid");
        assert_eq!(blank.region(), region, "a blank explicit region is unset");

        let pinned = BedrockProvider::new_in_region(arn, Some("ap-northeast-1")).expect("valid");
        assert_eq!(pinned.region(), "ap-northeast-1", "explicit beats the ARN");
    }
}

/// Collects formatted log output for one closure.
#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Capture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("capture lock").extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Capture {
    type Writer = Capture;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// The cost of one call to `model`, with the log lines it emitted.
fn cost_and_log(model: &str) -> (f64, String) {
    let capture = Capture::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(capture.clone())
        .with_ansi(false)
        .finish();
    let cost = tracing::subscriber::with_default(subscriber, || {
        estimate_bedrock_cost_usd(model, 1_000_000, 1_000_000)
    });
    let bytes = capture.0.lock().expect("capture lock").clone();
    (cost, String::from_utf8(bytes).expect("utf8 log output"))
}

/// #9200 pricing rule: an ARN that embeds a model id is priced as that model;
/// an application inference profile hides it and is reported as unpriced,
/// never a silent $0.
#[test]
fn arn_pricing_resolves_the_embedded_model_or_reports_unpriced() {
    for (arn, embedded) in [
        (INFERENCE_PROFILE_ARN, "us.anthropic.claude-sonnet-4-6"),
        (
            "arn:aws:bedrock:us-east-1:111122223333:inference-profile/global.anthropic.claude-opus-4-8",
            "global.anthropic.claude-opus-4-8",
        ),
        (
            FOUNDATION_MODEL_ARN,
            "anthropic.claude-haiku-4-5-20251001-v1:0",
        ),
    ] {
        let expected = estimate_bedrock_cost_usd(embedded, 1_000_000, 1_000_000);
        assert!(expected > 0.0, "{embedded} is in the price table");
        let (cost, log) = cost_and_log(arn);
        assert!(
            (cost - expected).abs() < 1e-9,
            "{arn}: priced {cost}, want {expected} (the rate of {embedded})"
        );
        assert!(!log.contains("unpriced"), "{arn} is priced: {log}");
    }

    let parsed = parse(APP_PROFILE_ARN).expect("a valid ARN");
    assert_eq!(parsed.priced_model(), None, "the profile id names no model");
    let (cost, log) = cost_and_log(APP_PROFILE_ARN);
    assert_eq!(cost, 0.0);
    assert!(
        log.contains("WARN") && log.contains("unpriced") && log.contains(APP_PROFILE_ARN),
        "an unpriced ARN must say so at warn level: {log:?}"
    );
}
