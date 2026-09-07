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
/// What: covers the main failure modes across the in-process, UDS remote, and
/// stdio sidecar paths.
///
/// #6289: the `Transport(reqwest::Error)` and `RemoteError { status, body }`
/// variants are gone with the HTTP client they belonged to — ADR-0032 retired
/// `trusty-embedderd --http`. `#[non_exhaustive]` was added in the same change
/// so the next variant is not another breaking one.
///
/// Test: `error_display` below and the bit_identical integration test.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum EmbedderError {
    /// The ONNX model raised an error during embedding.
    #[error("embedder model error: {0}")]
    ModelError(String),

    /// The remote server returned an unexpected number of vectors.
    ///
    /// Why: a mismatch here is a programming error in the daemon, not a
    /// transient failure, so it gets its own variant.
    #[error("embedder dimension mismatch: sent {sent} texts, got {got} vectors")]
    DimensionMismatch { sent: usize, got: usize },

    /// A UDS transport error occurred while communicating with trusty-embedderd.
    ///
    /// Why: UDS failures (connect refused, broken pipe, decode error) are
    /// distinct from stdio ones — a socket that is absent or wrong-moded is a
    /// deployment problem, not a crashed child. The string is the Display of
    /// the underlying `UdsSecurityError`, which names the socket path.
    #[error("embedder UDS error: {0}")]
    Uds(String),

    /// A stdio transport error occurred while communicating with a sidecar
    /// `trusty-embedderd` process spawned with piped stdin/stdout.
    ///
    /// Why: stdio failures (broken pipe, EOF before response, decode error) are
    /// distinct from UDS errors — they indicate the sidecar process
    /// has crashed or exited unexpectedly and the supervisor should respawn it.
    #[error("embedder stdio IPC error: {0}")]
    Stdio(String),
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
    fn error_display_uds() {
        // Why: verify the UDS variant's Display formatting for log messages.
        // What: format the variant and check it contains the message.
        // Test: this test.
        let e = EmbedderError::Uds(
            "connect to /tmp/trusty-embedderd.sock failed: no such file".to_string(),
        );
        let s = e.to_string();
        assert!(s.contains("UDS"), "must mention UDS");
        assert!(s.contains("no such file"), "must contain inner message");
    }

    #[test]
    fn error_display_stdio() {
        // Why: verify the Stdio variant's Display formatting for log messages.
        // What: format the variant and check the prefix and inner message.
        // Test: this test.
        let e = EmbedderError::Stdio("write to child stdin: broken pipe".to_string());
        let s = e.to_string();
        assert!(s.contains("stdio"), "must mention stdio");
        assert!(s.contains("broken pipe"), "must contain inner message");
    }
}
