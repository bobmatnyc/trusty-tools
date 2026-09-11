//! The one error type every attachment operation returns (#7370).
//!
//! Why: the guards this module exists to hold — traversal, absolute names,
//! the size cap, an unknown id — must REJECT, never repair. A rename-and-
//! continue guard writes the user's file somewhere they will not find it and
//! reports success, which is the failure mode the prior attempt on this issue
//! was blocked for. Naming each refusal as its own variant is what lets the
//! HTTP layer map it to a status code without string-matching a message.
//! What: a `thiserror` enum (library crate convention — see this repo's
//! CLAUDE.md) carrying the rejected value in every variant, so the caller can
//! report WHAT was refused and not merely that something was.
//! Test: `super::tests::store_tests` — every variant has a producing case.

use std::path::PathBuf;

/// Every way an attachment operation can refuse or fail.
#[derive(Debug, thiserror::Error)]
pub enum AttachmentError {
    /// A file name that is not a single, ordinary path segment.
    #[error("attachment file name `{name}` is rejected: {reason}")]
    UnsafeFileName { name: String, reason: String },

    /// A session id that is not a single, ordinary path segment.
    #[error("session id `{session}` is rejected: {reason}")]
    UnsafeSessionId { session: String, reason: String },

    /// The payload is larger than [`super::MAX_ATTACHMENT_BYTES`].
    #[error("attachment `{name}` is {size} bytes, over the {cap}-byte limit")]
    TooLarge { name: String, size: u64, cap: u64 },

    /// An id that is not the 32-hex-character shape this module mints.
    #[error("`{0}` is not an attachment id")]
    InvalidId(String),

    /// No manifest row with this id in this session.
    #[error("no attachment `{id}` in session `{session}`")]
    NotFound { id: String, session: String },

    /// The stored bytes are gone while the manifest row survives.
    #[error("attachment `{id}` is recorded but its file is missing at {path}")]
    MissingFile { id: String, path: PathBuf },

    /// Any filesystem failure, always naming the path it happened at.
    #[error("attachment store i/o failed at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// A manifest that is present but not decodable.
    #[error("attachment manifest at {path} is not readable as JSON: {source}")]
    Manifest {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },

    /// A manifest row whose stored name does not resolve inside its session
    /// directory.
    ///
    /// Why: the manifest is a plain JSON file in the user's own home tree, so
    /// it is EDITABLE — by the user, by anything else that can write there, or
    /// by a process that should not be able to. A row saying
    /// `stored_name: "/etc/passwd"` would otherwise resolve straight through
    /// `Path::join`, whose absolute-component rule replaces the base rather
    /// than appending to it. This is the refusal for that row: never repaired,
    /// never renamed, and never a client error, because the request was fine
    /// and the stored state is not.
    /// Test: `super::tests::manifest_tests::an_absolute_stored_name_is_refused`,
    /// `super::tests::manifest_tests::a_traversal_stored_name_is_refused`.
    #[error("attachment `{id}` does not resolve inside its session directory ({path}): {reason}")]
    TamperedManifest {
        path: PathBuf,
        id: String,
        reason: String,
    },
}

impl AttachmentError {
    /// Whether this refusal is the caller's fault (a `4xx`) rather than the
    /// server's (a `5xx`).
    ///
    /// Why: the HTTP layer must not string-match error text to pick a status.
    /// What: every guard variant is a client error; i/o and a corrupt manifest
    /// are not.
    /// Test: `super::tests::store_tests::client_errors_are_classified`.
    pub fn is_client_error(&self) -> bool {
        matches!(
            self,
            Self::UnsafeFileName { .. }
                | Self::UnsafeSessionId { .. }
                | Self::TooLarge { .. }
                | Self::InvalidId(_)
                | Self::NotFound { .. }
        )
    }
}
