//! Classify a failed Bedrock send or stream receive into a typed
//! [`BedrockError`] (#10892, #11228).
//!
//! Why: `Converse` and `ConverseStream` fail with an AWS `SdkError` whose
//! variant and source chain already say what went wrong. Flattening it to a
//! string threw that away; this module keeps it.
//! What: [`classify_send_error`] covers a failed `send()`;
//! [`classify_stream_error`] covers a failed receive after the stream opened.
//! Both render the context message the adapter always logged and pick the
//! [`BedrockError`] variant from the `SdkError`.
//! Test: `super::send_error_tests`.

use aws_credential_types::provider::error::CredentialsError;
use aws_sdk_bedrockruntime::config::http::HttpResponse;
use aws_sdk_bedrockruntime::error::{ProvideErrorMetadata, SdkError};
use aws_smithy_types::error::display::DisplayErrorContext;
use aws_smithy_types::event_stream::RawMessage;

use super::arn::mask_account_ids;
use super::stream::mid_stream_error_message;
use crate::inference::error::BedrockError;

/// Map one failed `send()` of `operation` to a [`BedrockError`].
///
/// Why: both adapter entry points must classify a failure the same way, and a
/// missing credential or a timeout must surface as its own typed error rather
/// than as an anonymous provider failure.
/// What: the message is
/// `"<operation> call failed (model=<model>, region=<region>): <chain>"`, the
/// text the adapter produced before #10892, with every ARN account id masked.
/// Precedence:
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
/// `send_error_construction_failure_maps_to_other`,
/// `send_error_message_masks_arn_account_ids`.
pub(crate) fn classify_send_error<E>(
    operation: &str,
    model: &str,
    region: &str,
    err: &SdkError<E, HttpResponse>,
) -> BedrockError
where
    E: ProvideErrorMetadata + std::error::Error + 'static,
{
    // #11865: mask account ids per #9200
    let message = mask_account_ids(&format!(
        "{operation} call failed (model={model}, region={region}): {}",
        DisplayErrorContext(err)
    ))
    .into_owned();
    classify(message, err, |raw| raw.status().as_u16())
}

/// Map one failed `ConverseStream` receive to a [`BedrockError`].
///
/// Why (#11228): a mid-stream failure was flattened to
/// [`InferenceError::Provider`](crate::inference::error::InferenceError::Provider),
/// so a stream caller could not tell a throttle from a dropped connection.
/// What: the message is [`mid_stream_error_message`]'s text, unchanged, so the
/// display text is the one the `Provider` string had. The variant follows the
/// same precedence as [`classify_send_error`]. A modeled exception arrives as
/// an event-stream frame inside a response that already answered 200, so its
/// `Api` status is 200 and the AWS error code says which exception it was.
/// Test: `stream_error_service_error_maps_to_api_with_code`,
/// `stream_error_dispatch_io_maps_to_transport`,
/// `stream_error_timeouts_map_to_timeout`,
/// `stream_error_credentials_error_maps_to_missing_credential`,
/// `stream_error_unreadable_frame_maps_to_other`,
/// `stream_error_keeps_mid_stream_message_and_masks_arns`.
pub(crate) fn classify_stream_error<E>(
    model: &str,
    region: &str,
    err: &SdkError<E, RawMessage>,
) -> BedrockError
where
    E: ProvideErrorMetadata + std::error::Error + 'static,
{
    let message = mid_stream_error_message(model, region, &DisplayErrorContext(err).to_string());
    // #11228: the stream's HTTP status was 200; the frame carries no other.
    classify(message, err, |_| 200)
}

/// Pick the [`BedrockError`] variant for `err`, carrying `message`.
///
/// `status` reads the HTTP status for a service error from the raw value.
fn classify<E, R>(
    message: String,
    err: &SdkError<E, R>,
    status: impl FnOnce(&R) -> u16,
) -> BedrockError
where
    E: ProvideErrorMetadata + std::error::Error + 'static,
    R: std::fmt::Debug + 'static,
{
    if chain_has_credentials_error(err) {
        return BedrockError::MissingCredential(message);
    }
    match err {
        SdkError::TimeoutError(_) => BedrockError::Timeout(message),
        SdkError::DispatchFailure(d) if d.is_timeout() => BedrockError::Timeout(message),
        // #11228: an identity-resolution timeout also lands here, as Transport.
        // Telling it apart needs `aws-smithy-runtime`, not a dependency today.
        SdkError::DispatchFailure(_) => BedrockError::Transport(message),
        SdkError::ServiceError(ctx) => BedrockError::Api {
            status: status(ctx.raw()),
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
