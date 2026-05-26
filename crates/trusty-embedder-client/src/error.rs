//! Error types for the embedder client.
//!
//! Why: library code uses `thiserror` structured errors so downstream callers
//! can match on variants rather than inspecting strings. `anyhow` is reserved
//! for binary / application-layer error handling.
//!
//! What: `EmbedderError` is the single error type returned by all
//! `EmbedderClient` implementations.
//!
//! Test: `error_display` in this module verifies `Display` formatting.

/// Error returned by `EmbedderClient::embed_batch`.
///
/// Why: distinct variants allow callers to decide whether to retry (transport
/// errors), report a bug (dimension mismatch), or surface a model issue.
///
/// What: covers the main failure modes across the in-process, HTTP-remote,
/// and UDS-remote paths.
///
/// Test: `error_display_*` below and the bit_identical integration test.
#[derive(Debug, thiserror::Error)]
pub enum EmbedderError {
    /// The ONNX model raised an error during embedding.
    #[error("embedder model error: {0}")]
    ModelError(String),

    /// An HTTP transport error occurred while communicating with trusty-embedderd.
    #[error("embedder transport error: {0}")]
    Transport(#[from] reqwest::Error),

    /// The remote server returned an unexpected number of vectors.
    ///
    /// Why: a mismatch here is a programming error in the daemon, not a
    /// transient failure, so it gets its own variant.
    #[error("embedder dimension mismatch: sent {sent} texts, got {got} vectors")]
    DimensionMismatch { sent: usize, got: usize },

    /// The remote server returned an error response body (HTTP path).
    #[error("embedder remote error (HTTP {status}): {body}")]
    RemoteError { status: u16, body: String },

    /// A UDS transport or protocol error occurred.
    ///
    /// Why: UDS failures need to be distinguishable from model errors so
    /// callers can decide whether to retry on a different transport.
    #[error("embedder UDS error: {0}")]
    UdsError(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_display_model_error() {
        // Why: verify Display formatting for model errors used in log messages.
        // What: format the variant and check it contains the message.
        // Test: use assert! with contains check.
        let e = EmbedderError::ModelError("ort session failed".to_string());
        assert!(e.to_string().contains("ort session failed"));
    }

    #[test]
    fn error_display_dimension_mismatch() {
        let e = EmbedderError::DimensionMismatch { sent: 5, got: 3 };
        let s = e.to_string();
        assert!(s.contains("5") && s.contains("3"));
    }

    #[test]
    fn error_display_remote_error() {
        let e = EmbedderError::RemoteError {
            status: 500,
            body: "internal server error".to_string(),
        };
        assert!(e.to_string().contains("500"));
    }
}
