//! Classify a failed Bedrock send into a typed [`BedrockError`] (#10892).
//!
//! Why: `Converse` and `ConverseStream` fail with an AWS `SdkError` whose
//! variant and source chain already say what went wrong. Flattening it to a
//! string threw that away; this module keeps it.
//! What: [`classify_send_error`] renders the same context message the adapter
//! always logged and picks the [`BedrockError`] variant from the `SdkError`.
//! Test: `super::send_error_tests`.

use aws_credential_types::provider::error::CredentialsError;
use aws_sdk_bedrockruntime::config::http::HttpResponse;
use aws_sdk_bedrockruntime::error::{ProvideErrorMetadata, SdkError};
use aws_smithy_types::error::display::DisplayErrorContext;

use crate::inference::error::BedrockError;

/// Map one failed `send()` of `operation` to a [`BedrockError`].
///
/// Why: both adapter entry points must classify a failure the same way, and a
/// missing credential or a timeout must surface as its own typed error rather
/// than as an anonymous provider failure.
/// What: the message is
/// `"<operation> call failed (model=<model>, region=<region>): <chain>"`, the
/// text the adapter produced before #10892. Precedence:
/// 1. a `CredentialsError` anywhere in the source chain → `MissingCredential`
///    (the SDK reports identity failures as a dispatch failure);
/// 2. `TimeoutError`, or a dispatch failure the connector marks as a timeout
///    → `Timeout`;
/// 3. any other dispatch failure → `Transport`;
/// 4. a service error → `Api` with the HTTP status and AWS error code;
/// 5. anything else (construction failure, unreadable response) → `Other`.
///
/// Test: `send_error_service_error_maps_to_api_with_status_and_code`,
/// `send_error_dispatch_io_maps_to_transport`,
/// `send_error_timeouts_map_to_timeout`,
/// `send_error_credentials_error_maps_to_missing_credential`,
/// `send_error_construction_failure_maps_to_other`.
pub(crate) fn classify_send_error<E>(
    operation: &str,
    model: &str,
    region: &str,
    err: &SdkError<E, HttpResponse>,
) -> BedrockError
where
    E: ProvideErrorMetadata + std::error::Error + 'static,
{
    let message = format!(
        "{operation} call failed (model={model}, region={region}): {}",
        DisplayErrorContext(err)
    );
    if chain_has_credentials_error(err) {
        return BedrockError::MissingCredential(message);
    }
    match err {
        SdkError::TimeoutError(_) => BedrockError::Timeout(message),
        SdkError::DispatchFailure(d) if d.is_timeout() => BedrockError::Timeout(message),
        SdkError::DispatchFailure(_) => BedrockError::Transport(message),
        SdkError::ServiceError(ctx) => BedrockError::Api {
            status: ctx.raw().status().as_u16(),
            code: ctx.err().code().map(str::to_owned),
            message,
        },
        _ => BedrockError::Other(message),
    }
}

/// Whether `err` or any error in its source chain is a `CredentialsError`.
fn chain_has_credentials_error(err: &(dyn std::error::Error + 'static)) -> bool {
    let mut current = Some(err);
    while let Some(e) = current {
        if e.is::<CredentialsError>() {
            return true;
        }
        current = e.source();
    }
    false
}
