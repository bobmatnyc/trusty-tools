//! The log drain's error contract (#6533).
//!
//! Why: the drain fails in six materially different ways and the caller acts
//! differently on each. A malformed URI is an operator typo fixed in config; a
//! credential failure is an environment problem that will not resolve on
//! retry; a transport failure is transient and the next scheduled run may
//! succeed; a missing identity is a REFUSAL, not a failure, and must never be
//! retried into an upload. Collapsing those into one opaque string would make
//! the scheduler Phase 3 adds unable to tell "stop asking" from "try later".
//! What: [`DrainError`], a `thiserror` enum. `#[non_exhaustive]` so a later
//! phase can add a variant without a breaking change on a `0.x` crate.
//! Test: `super::tests::uri_*` (the URI and scheme variants),
//! `super::tests::run_once_refuses_an_empty_owner` and
//! `super::tests::run_once_refuses_an_empty_project` (identity refusal).

use std::path::PathBuf;

/// Every way the log drain can fail or refuse.
///
/// Why: see the module docs — the caller's response differs per variant, so
/// the variants are the contract, not decoration.
/// What: seven variants covering scheme rejection, URI syntax, credential
/// resolution, object-store transport, local IO, manifest decoding, and the
/// fail-closed identity refusal.
/// Test: `super::tests::uri_table_rejects`, `super::tests::uri_reserved_schemes`,
/// `super::tests::run_once_refuses_an_empty_owner`.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DrainError {
    /// A URI naming a scheme the drain deliberately does not implement.
    ///
    /// `gs://` and `az://` reach this variant on purpose: the parser reserves
    /// them so a config pointing at Google or Azure storage fails with a clear
    /// message instead of the generic "malformed URI" a bare rejection gives.
    #[error(
        "unsupported destination scheme `{scheme}://` in `{uri}` — \
         the log drain supports `s3://` and `file://` only"
    )]
    UnsupportedScheme {
        /// The scheme as written, without the `://`.
        scheme: String,
        /// The full URI the operator supplied.
        uri: String,
    },

    /// A URI whose syntax the parser could not make sense of.
    #[error("malformed destination URI `{uri}`: {reason}")]
    Uri {
        /// The full URI the operator supplied.
        uri: String,
        /// What specifically was wrong, in operator-facing terms.
        reason: String,
    },

    /// The AWS credential chain produced no usable credentials.
    ///
    /// Distinct from [`DrainError::Transport`] because retrying does not help:
    /// the environment has no credentials, and the next scheduled run will fail
    /// identically until an operator fixes it.
    #[error("AWS credentials unavailable for `{uri}`: {source}")]
    Credentials {
        /// The destination whose credentials could not be resolved.
        uri: String,
        /// The underlying provider-chain error.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    /// The object store rejected or could not complete a request.
    #[error("object-store {op} failed for `{key}`: {source}")]
    Transport {
        /// Which operation failed — `put`, `head`, or `list`.
        op: &'static str,
        /// The full object key (or prefix, for `list`).
        key: String,
        /// The underlying `object_store` error.
        #[source]
        source: object_store::Error,
    },

    /// A local filesystem operation failed.
    #[error("io error at `{}`: {source}", path.display())]
    Io {
        /// The path being read, written, or created.
        path: PathBuf,
        /// The underlying IO error.
        #[source]
        source: std::io::Error,
    },

    /// A manifest object exists but could not be decoded.
    ///
    /// Never fatal to a run: [`super::run_once`] treats an undecodable manifest
    /// as an absent one and re-uploads, because uploading a file twice is
    /// strictly safer than skipping one that was never uploaded.
    #[error("drain manifest at `{key}` is unreadable: {reason}")]
    Manifest {
        /// The manifest's object key.
        key: String,
        /// Why it could not be decoded.
        reason: String,
    },

    /// The caller supplied an empty identity component.
    ///
    /// Why this is an error and not a default: the key layout puts every
    /// uploaded byte under `<owner>/<project>` (#6657). An empty component
    /// collapses one project's logs into a shared, unattributable prefix, so
    /// the drain fails closed rather than uploading under a guessed identity.
    #[error(
        "refusing to upload: `{field}` is empty — \
         the log drain never writes under an unknown identity"
    )]
    MissingIdentity {
        /// Which component was empty: `owner` or `project`.
        field: &'static str,
    },
}
