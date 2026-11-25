//! Error-arm tests for the typed Bedrock send error (#10892).
//!
//! Each test builds the failing `SdkError` the AWS SDK produces for one
//! failure class and asserts the [`BedrockError`] variant, never the text.
//! `chat_without_credentials_returns_missing_credential` drives the real SDK
//! orchestrator through the adapter with a credential provider that fails, so
//! it proves the classification against the SDK's own error shape. No test
//! reaches AWS.

use aws_credential_types::provider::error::CredentialsError;
use aws_credential_types::provider::{ProvideCredentials, future};
use aws_sdk_bedrockruntime::Client as BedrockRuntimeClient;
use aws_sdk_bedrockruntime::config::http::HttpResponse;
use aws_sdk_bedrockruntime::config::retry::RetryConfig;
use aws_sdk_bedrockruntime::config::{BehaviorVersion, Region};
use aws_sdk_bedrockruntime::error::{ConnectorError, ErrorMetadata, SdkError};
use aws_sdk_bedrockruntime::operation::converse::ConverseError;
use aws_smithy_types::body::SdkBody;

use super::BedrockAdapter;
use super::send_error::classify_send_error;
use crate::inference::adapter::InferenceAdapter;
use crate::inference::error::{BedrockError, InferenceError};
use crate::inference::types::{ChatMessage, ChatRequest};

const MODEL: &str = "bedrock/us.anthropic.claude-sonnet-4-6";
const REGION: &str = "us-east-1";

/// Classify `err` as a failed `Converse` send.
fn classify(err: &SdkError<ConverseError, HttpResponse>) -> BedrockError {
    classify_send_error("Converse", MODEL, REGION, err)
}

/// An HTTP response with `status` and an empty body.
fn response(status: u16) -> HttpResponse {
    HttpResponse::new(status.try_into().expect("valid status"), SdkBody::empty())
}

#[test]
fn send_error_service_error_maps_to_api_with_status_and_code() {
    for (status, code) in [
        (429, "ThrottlingException"),
        (403, "AccessDeniedException"),
        (500, "InternalServerException"),
    ] {
        let meta = ErrorMetadata::builder().code(code).message("m").build();
        let err = SdkError::service_error(ConverseError::generic(meta), response(status));
        match classify(&err) {
            BedrockError::Api {
                status: got,
                code: Some(got_code),
                ..
            } => {
                assert_eq!(got, status);
                assert_eq!(got_code, code);
            }
            other => panic!("{code}: want Api, got {other:?}"),
        }
    }
}

#[test]
fn send_error_dispatch_io_maps_to_transport() {
    let err = SdkError::dispatch_failure(ConnectorError::io("connection refused".into()));
    assert!(
        matches!(classify(&err), BedrockError::Transport(_)),
        "{err:?}"
    );
}

#[test]
fn send_error_timeouts_map_to_timeout() {
    let operation = SdkError::timeout_error("operation timeout after 1ms");
    let connect = SdkError::dispatch_failure(ConnectorError::timeout("connect timeout".into()));
    for err in [operation, connect] {
        assert!(
            matches!(classify(&err), BedrockError::Timeout(_)),
            "{err:?}"
        );
    }
}

#[test]
fn send_error_credentials_error_maps_to_missing_credential() {
    let source = CredentialsError::not_loaded("no credentials in the chain");
    let err = SdkError::dispatch_failure(ConnectorError::other(Box::new(source), None));
    assert!(
        matches!(classify(&err), BedrockError::MissingCredential(_)),
        "{err:?}"
    );
}

#[test]
fn send_error_construction_failure_maps_to_other() {
    let err = SdkError::construction_failure("model_id is required");
    assert!(matches!(classify(&err), BedrockError::Other(_)), "{err:?}");
}

/// A credential provider that never yields credentials.
#[derive(Debug)]
struct NoCredentials;

impl ProvideCredentials for NoCredentials {
    fn provide_credentials<'a>(&'a self) -> future::ProvideCredentials<'a>
    where
        Self: 'a,
    {
        future::ProvideCredentials::ready(Err(CredentialsError::not_loaded("test: none")))
    }
}

/// An adapter whose client resolves no credentials and never retries.
fn adapter_without_credentials() -> BedrockAdapter {
    let config = aws_sdk_bedrockruntime::Config::builder()
        .behavior_version(BehaviorVersion::latest())
        .region(Region::new(REGION))
        .credentials_provider(NoCredentials)
        .retry_config(RetryConfig::disabled())
        .build();
    BedrockAdapter::with_client(REGION, BedrockRuntimeClient::from_conf(config))
}

/// Fail-open guard: with no credential, both entry points return the typed
/// error, never an empty response or a stream.
#[tokio::test]
async fn chat_without_credentials_returns_missing_credential() {
    let adapter = adapter_without_credentials();
    let request = ChatRequest::new(MODEL, vec![ChatMessage::user("ping")]);

    match adapter.chat(&request).await {
        Err(InferenceError::Bedrock(BedrockError::MissingCredential(_))) => {}
        Err(other) => panic!("chat: want MissingCredential, got {other:?}"),
        Ok(resp) => panic!("chat: a missing credential returned Ok: {resp:?}"),
    }
    match adapter.chat_stream(&request).await {
        Err(InferenceError::Bedrock(BedrockError::MissingCredential(_))) => {}
        Err(other) => panic!("chat_stream: want MissingCredential, got {other:?}"),
        Ok(_) => panic!("chat_stream: a missing credential returned a stream"),
    }
}

/// An application-inference-profile ARN; the account id is AWS's
/// documentation placeholder, not a real account.
const ARN_MODEL: &str =
    "arn:aws:bedrock:us-west-2:111122223333:application-inference-profile/9iatxd8u1751";
/// The account id `ARN_MODEL` carries.
const ACCOUNT: &str = "111122223333";

/// #11865: an ARN model id, or an ARN quoted by AWS in the source chain, never
/// leaks its account id into the `BedrockError` Display text (#9200).
#[test]
fn send_error_message_masks_arn_account_ids() {
    let credentials: SdkError<ConverseError, HttpResponse> =
        SdkError::dispatch_failure(ConnectorError::other(
            Box::new(CredentialsError::not_loaded("no credentials in the chain")),
            None,
        ));
    let meta = ErrorMetadata::builder()
        .code("AccessDeniedException")
        .message(format!(
            "User: arn:aws:sts::{ACCOUNT}:assumed-role/r/s is not authorized"
        ))
        .build();
    let api = SdkError::service_error(ConverseError::generic(meta), response(403));

    let missing = classify_send_error("Converse", ARN_MODEL, REGION, &credentials);
    assert!(
        matches!(missing, BedrockError::MissingCredential(_)),
        "{missing:?}"
    );
    let denied = classify_send_error("Converse", ARN_MODEL, REGION, &api);
    assert!(
        matches!(denied, BedrockError::Api { status: 403, .. }),
        "{denied:?}"
    );
    let denied_text = denied.to_string();
    assert!(
        denied_text.contains("arn:aws:sts::****:assumed-role"),
        "source-chain ARN not masked: {denied_text}"
    );
    for err in [missing, denied] {
        let text = err.to_string();
        assert!(!text.contains(ACCOUNT), "account id leaked: {text}");
        assert!(text.contains("****"), "mask absent: {text}");
    }
}
