//! Tests for Bedrock model ARNs as trusty-review model ids (#9200).
//!
//! Routing goes through `build_provider`, the one factory the CLI, config and
//! MCP `reviewer_model` paths share; no test reaches AWS, because
//! `BedrockProvider` builds its client lazily on the first Converse call.

use std::sync::{Arc, Mutex};

use aws_sdk_bedrockruntime::config::interceptors::BeforeSerializationInterceptorContextRef;
use aws_sdk_bedrockruntime::config::{BehaviorVersion, ConfigBag, Intercept, Region};
use aws_sdk_bedrockruntime::error::SdkError;
use aws_sdk_bedrockruntime::operation::converse::{ConverseError, ConverseInput};
use aws_sdk_bedrockruntime::types::error::{AccessDeniedException, ResourceNotFoundException};
use aws_smithy_types::error::ErrorMetadata;

use super::{ArnKind, BedrockArn, mask_account_ids, parse};
use crate::config::{Provider, ReviewConfig};
use crate::llm::bedrock::{
    BedrockProvider, estimate_bedrock_cost_usd, map_converse_error, validate_model_id,
};
use crate::llm::{ChatMessage, LlmError, LlmRequest, build_provider, resolve_model};
use crate::pipeline::post::format_review_footer;

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
        log.contains("WARN") && log.contains("unpriced") && log.contains(MASKED_APP_PROFILE_ARN),
        "an unpriced ARN must say so at warn level: {log:?}"
    );
    assert!(
        !log.contains(ACCOUNT),
        "the warn line leaks the account: {log:?}"
    );
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

/// A Converse service error carrying `code` and `message` in its metadata.
fn service_error(code: &str, message: &str) -> SdkError<ConverseError, ()> {
    let meta = ErrorMetadata::builder().code(code).message(message).build();
    let err = match code {
        "AccessDeniedException" => ConverseError::AccessDeniedException(
            AccessDeniedException::builder()
                .message(message)
                .meta(meta)
                .build(),
        ),
        _ => ConverseError::ResourceNotFoundException(
            ResourceNotFoundException::builder()
                .message(message)
                .meta(meta)
                .build(),
        ),
    };
    SdkError::service_error(err, ())
}

/// ModelNotFound, AccessDenied and the validation error name the model; none
/// may carry the account id, whether it came from the model or from AWS.
#[test]
fn converse_errors_mask_the_arn_account_id() {
    let not_found = service_error(
        "ResourceNotFoundException",
        &format!("{APP_PROFILE_ARN} is not found"),
    );
    let err = map_converse_error(&not_found, APP_PROFILE_ARN, "us-west-2");
    assert!(matches!(err, LlmError::ModelNotFound(_)), "{err:?}");
    assert!(err.to_string().contains(MASKED_APP_PROFILE_ARN), "{err}");
    assert!(!err.to_string().contains(ACCOUNT), "account leaked: {err}");

    let denied = service_error(
        "AccessDeniedException",
        &format!("User: arn:aws:sts::{ACCOUNT}:assumed-role/r/s is not authorized"),
    );
    let err = map_converse_error(&denied, APP_PROFILE_ARN, "us-west-2");
    assert!(matches!(err, LlmError::AccessDenied(_)), "{err:?}");
    assert!(err.to_string().contains(MASKED_APP_PROFILE_ARN), "{err}");
    assert!(!err.to_string().contains(ACCOUNT), "account leaked: {err}");

    // A near-ARN the validator rejects is echoed masked, and the message
    // names the ARN form it would have accepted.
    let near = format!("arn:aws:bedrock:us-west-2:{ACCOUNT}:agent/AGENT12345");
    let err = validate_model_id(&near).expect_err("an agent ARN is not a model");
    let text = err.to_string();
    assert!(
        text.contains("arn:aws:bedrock:us-west-2:****:agent/AGENT12345"),
        "{text}"
    );
    assert!(text.contains("Bedrock model ARN"), "{text}");
    assert!(!text.contains(ACCOUNT), "account leaked: {text}");
}

/// Captures the model id the SDK built into the Converse request, then halts.
#[derive(Debug, Clone, Default)]
struct CaptureModelId(Arc<Mutex<Option<String>>>);

impl Intercept for CaptureModelId {
    fn name(&self) -> &'static str {
        "CaptureModelId"
    }

    fn read_before_execution(
        &self,
        context: &BeforeSerializationInterceptorContextRef<'_>,
        _cfg: &mut ConfigBag,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let input = context
            .input()
            .downcast_ref::<ConverseInput>()
            .ok_or("operation input is not a ConverseInput")?;
        *self.0.lock().expect("capture lock") = input.model_id().map(str::to_string);
        Err("request captured; halting before any network I/O".into())
    }
}

/// The mask is display-only: Converse receives the ARN with its account id.
#[tokio::test]
async fn converse_receives_the_unmasked_arn() {
    let capture = CaptureModelId::default();
    let conf = aws_sdk_bedrockruntime::Config::builder()
        .behavior_version(BehaviorVersion::latest())
        .region(Region::new("us-west-2"))
        .interceptor(capture.clone())
        .build();
    let client = aws_sdk_bedrockruntime::Client::from_conf(conf);
    let provider = BedrockProvider::from_client(client, APP_PROFILE_ARN, "us-west-2");
    let req = LlmRequest {
        model: String::new(),
        system: String::new(),
        messages: vec![ChatMessage {
            role: "user".to_string(),
            content: "hello".to_string(),
        }],
        temperature: 0.0,
        max_tokens: 16,
        response_schema: None,
    };

    let err = provider
        .call_once(&req)
        .await
        .expect_err("the interceptor halts the call");
    let sent = capture.0.lock().expect("capture lock").take();
    assert_eq!(sent.as_deref(), Some(APP_PROFILE_ARN));
    assert!(!err.to_string().contains(ACCOUNT), "account leaked: {err}");
}

/// The posted footer masks the account and prints `est. unpriced` for an
/// application inference profile; other models keep their dollar figure.
#[test]
fn footer_masks_the_account_and_says_unpriced_for_an_application_profile() {
    for model in [
        APP_PROFILE_ARN.to_string(),
        format!("bedrock/{APP_PROFILE_ARN}"),
    ] {
        let footer = format_review_footer(None, &model, 1_000, 200, 0.0);
        assert!(footer.contains(MASKED_APP_PROFILE_ARN), "{footer}");
        assert!(footer.contains("est. unpriced"), "{footer}");
        assert!(!footer.contains(ACCOUNT), "{footer}");
        assert!(
            !footer.contains('$'),
            "an unpriced model has no $ figure: {footer}"
        );
    }
    for model in [
        INFERENCE_PROFILE_ARN,
        FOUNDATION_MODEL_ARN,
        "us.anthropic.claude-sonnet-4-6",
    ] {
        let footer = format_review_footer(None, model, 1_000, 200, 0.066);
        assert!(footer.contains("est. $0.066"), "{footer}");
        assert!(!footer.contains("unpriced"), "{footer}");
    }
}

/// Run `finalize_review` on the log path for a result whose model is `model`,
/// and render the comment body that would be posted.
async fn finalized_comment(model: &str) -> (crate::models::ReviewResult, String) {
    use crate::integrations::github::RunMode;
    use crate::integrations::github::posting::build_review_comment_body;
    use crate::pipeline::post::{PostContext, finalize_review};
    use crate::pipeline::trigger::TriggerDecision;

    let mut result = crate::models::ReviewResult::new("local", "repo", 1, "t", "u");
    result.model = model.to_string();
    let out = finalize_review(
        result,
        &ReviewConfig::load(None),
        TriggerDecision::None,
        false,
        false,
        false,
        PostContext {
            owner: "local",
            repo: "repo",
            pr: 1,
            head_sha: "",
            run_mode: RunMode::Cli,
            dedup: None,
        },
    )
    .await;
    let body = build_review_comment_body(&out);
    (out, body)
}

/// The posted comment, VerdictBlock JSON included, carries no account id for
/// an ARN model; the footer still reads the raw id, so it still says unpriced.
#[tokio::test]
async fn posted_comment_masks_the_arn_account_everywhere() {
    let (out, body) = finalized_comment(APP_PROFILE_ARN).await;
    assert_eq!(out.model, MASKED_APP_PROFILE_ARN);
    assert!(!body.contains(ACCOUNT), "account id in the comment: {body}");
    assert!(body.contains(MASKED_APP_PROFILE_ARN), "{body}");
    assert!(body.contains("est. unpriced"), "{body}");

    let plain = "us.anthropic.claude-sonnet-4-6";
    let (out, body) = finalized_comment(plain).await;
    assert_eq!(out.model, plain, "a plain id is never rewritten");
    assert!(body.contains(&format!("`{plain}`")), "{body}");
}
