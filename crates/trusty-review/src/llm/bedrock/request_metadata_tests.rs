//! Tests for the Bedrock `requestMetadata` tags.
//!
//! The first two tests read the metadata off the `ConverseInput` the SDK
//! actually built, through an interceptor that captures it and halts the call
//! before identity resolution or any network I/O.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use aws_sdk_bedrockruntime::config::interceptors::BeforeSerializationInterceptorContextRef;
use aws_sdk_bedrockruntime::config::{BehaviorVersion, ConfigBag, Intercept, Region};
use aws_sdk_bedrockruntime::operation::converse::ConverseInput;

use super::{CALLER, role_for_schema};
use crate::llm::bedrock::BedrockProvider;
use crate::llm::{ChatMessage, LlmRequest, ResponseSchema};

type Captured = Arc<Mutex<Option<Option<HashMap<String, String>>>>>;

/// Captures the built request's `requestMetadata`, then halts the call.
#[derive(Debug, Clone, Default)]
struct CaptureMetadata {
    seen: Captured,
}

impl Intercept for CaptureMetadata {
    fn name(&self) -> &'static str {
        "CaptureMetadata"
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
        *self.seen.lock().expect("capture lock") = Some(input.request_metadata().cloned());
        Err("request captured; halting before any network I/O".into())
    }
}

/// Send `req` through `BedrockProvider::call_once` and return the metadata the
/// SDK built into the Converse request.
async fn metadata_sent_for(req: &LlmRequest) -> HashMap<String, String> {
    let capture = CaptureMetadata::default();
    let conf = aws_sdk_bedrockruntime::Config::builder()
        .behavior_version(BehaviorVersion::latest())
        .region(Region::new("us-east-1"))
        .interceptor(capture.clone())
        .build();
    let client = aws_sdk_bedrockruntime::Client::from_conf(conf);
    let provider =
        BedrockProvider::from_client(client, "us.anthropic.claude-sonnet-4-6", "us-east-1");

    let result = provider.call_once(req).await;
    assert!(result.is_err(), "the interceptor halts every call");
    let captured = capture.seen.lock().expect("capture lock").take();
    captured
        .expect("the interceptor saw the Converse input")
        .expect("the Converse request carries requestMetadata")
}

fn request(schema: Option<ResponseSchema>) -> LlmRequest {
    LlmRequest {
        model: "us.anthropic.claude-sonnet-4-6".to_string(),
        system: "system".to_string(),
        messages: vec![ChatMessage {
            role: "user".to_string(),
            content: "hello".to_string(),
        }],
        temperature: 0.0,
        max_tokens: 16,
        response_schema: schema,
    }
}

/// A reviewer request carries `caller`, `crate_version` and `role=reviewer`.
#[tokio::test]
async fn converse_request_carries_caller_and_role_metadata() {
    let schema = crate::pipeline::prompt::review_response_schema();
    let sent = metadata_sent_for(&request(Some(schema))).await;

    let expected: HashMap<String, String> = [
        ("caller", CALLER),
        ("crate_version", env!("CARGO_PKG_VERSION")),
        ("role", "reviewer"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect();
    assert_eq!(sent, expected);
}

/// #9200: a call whose model is a Bedrock ARN carries the same #9216 tags.
/// The ARN must first pass the constructor's model-id check, which rejected
/// every ARN before ARN support.
#[tokio::test]
async fn arn_request_carries_caller_and_role_metadata() {
    let arn = "arn:aws:bedrock:us-west-2:111122223333:application-inference-profile/9iatxd8u1751";
    let provider = BedrockProvider::new(arn).expect("an ARN is a valid Bedrock model id");
    assert_eq!(provider.region(), "us-west-2");

    let mut req = request(Some(crate::pipeline::prompt::review_response_schema()));
    req.model = arn.to_string();
    let sent = metadata_sent_for(&req).await;

    assert_eq!(
        sent.get("caller").map(String::as_str),
        Some("trusty-review")
    );
    assert_eq!(
        sent.get("crate_version").map(String::as_str),
        Some(env!("CARGO_PKG_VERSION"))
    );
    assert_eq!(sent.get("role").map(String::as_str), Some("reviewer"));
}

/// A request with no schema still names the caller, and carries no role.
#[tokio::test]
async fn schemaless_request_carries_caller_without_role() {
    let sent = metadata_sent_for(&request(None)).await;

    assert_eq!(
        sent.get("caller").map(String::as_str),
        Some("trusty-review")
    );
    assert_eq!(
        sent.get("crate_version").map(String::as_str),
        Some(env!("CARGO_PKG_VERSION"))
    );
    assert!(!sent.contains_key("role"), "no schema, no role: {sent:?}");
}

/// Every schema builder's name maps to the role that sends it, so a renamed
/// schema fails here instead of silently dropping its role tag.
#[test]
fn every_schema_builder_maps_to_its_role() {
    let cases = [
        (
            crate::pipeline::prompt::review_response_schema(),
            "reviewer",
        ),
        (
            crate::pipeline::verify_prompt::verify_response_schema(),
            "verifier",
        ),
        (
            crate::pipeline::verify_batch::batch_response_schema(),
            "verifier",
        ),
        (
            crate::report::synthesize_prompt::synthesis_schema(5, 5),
            "synthesis",
        ),
        (
            crate::report::investigate::verdict::verdict_schema(),
            "investigate",
        ),
        (
            crate::report::investigate::analyze::investigation_schema(5),
            "investigate",
        ),
    ];
    for (schema, role) in cases {
        assert_eq!(role_for_schema(&schema.name), Some(role), "{}", schema.name);
    }
    assert_eq!(role_for_schema("unknown_schema"), None);
}
