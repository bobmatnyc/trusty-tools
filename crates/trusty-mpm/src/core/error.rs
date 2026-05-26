//! Error types shared across trusty-mpm.
//!
//! Why: A single error enum lets every crate use `?` uniformly and gives clients
//! a stable, serializable failure surface over IPC.
//! What: `Error` covers IO, serialization, artifact parsing, and protocol faults.
//! Test: `cargo test -p trusty-mpm-core` checks `Display` output is non-empty.

use thiserror::Error;

/// Crate-wide result alias.
pub type Result<T> = std::result::Result<T, Error>;

/// All failure modes surfaced by trusty-mpm-core.
#[derive(Debug, Error)]
pub enum Error {
    /// Filesystem access failed.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    /// JSON (de)serialization failed.
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    /// YAML frontmatter parsing failed.
    #[error("yaml error: {0}")]
    Yaml(#[from] serde_yaml::Error),

    /// An artifact file was malformed or missing required fields.
    #[error("invalid artifact: {0}")]
    Artifact(String),

    /// The IPC peer sent a message that violated the protocol.
    #[error("protocol error: {0}")]
    Protocol(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artifact_error_displays_message() {
        let err = Error::Artifact("missing frontmatter".into());
        assert!(err.to_string().contains("missing frontmatter"));
    }
}
