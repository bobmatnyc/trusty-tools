//! The one error type every `trusty-secrets` operation returns.
//!
//! Why: a secrets failure has to be actionable without ever echoing a value.
//! Rejected input is the risky case — an operator who pastes a value where a
//! key belongs must not see it repeated in a log — so no variant carries the
//! text it rejected. Variants carry only validated names, paths, and fixed
//! reasons.
//! What: [`SecretsError`], one variant per failure class.
//! Test: `api_errors_never_echo_rejected_input`.

use std::path::PathBuf;
use std::time::Duration;

/// Every failure a `trusty-secrets` operation can report.
///
/// Why: see the module docs. Callers match on the variant; the `Display` text
/// is for humans and is safe to log.
/// What: `&'static str` reasons for rejected input; `String` fields only for
/// names that already passed validation, paths, and backend diagnostics that
/// were built without the value.
/// Test: `api_errors_never_echo_rejected_input`, plus one error-arm test per
/// fail-closed path in the store tests.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SecretsError {
    /// A secret key name failed validation.
    #[error("invalid secret key: {reason}")]
    InvalidKey {
        /// Which rule the key broke.
        reason: &'static str,
    },

    /// An owner, repository, vault, or backend name failed validation.
    #[error("invalid {what} name: {reason}")]
    InvalidName {
        /// Which kind of name: `owner`, `repository`, `vault`, `backend`.
        what: &'static str,
        /// Which rule the name broke.
        reason: &'static str,
    },

    /// A `secret://` reference did not match the grammar.
    #[error("invalid secret reference: {reason}")]
    InvalidReference {
        /// Which rule the reference broke.
        reason: &'static str,
    },

    /// A value was refused before it reached a backend.
    #[error("invalid secret value: {reason}")]
    InvalidValue {
        /// Which rule the value broke.
        reason: &'static str,
    },

    /// No scope searched holds the key.
    #[error("secret {key} not found in {searched}")]
    NotFound {
        /// The key that was looked up.
        key: String,
        /// The vault or vaults searched, in order.
        searched: String,
    },

    /// A backend refused or failed an operation.
    #[error("{backend} backend failed on {key} in {vault}: {reason}")]
    Backend {
        /// Backend id, e.g. `keychain`.
        backend: String,
        /// The vault addressed.
        vault: String,
        /// The key addressed.
        key: String,
        /// The backend's diagnostic, built without the value.
        reason: String,
    },

    /// The backend lacks the capability an operation needs.
    #[error("{backend} backend does not support {operation}")]
    Unsupported {
        /// Backend id.
        backend: String,
        /// The operation refused, e.g. `read`.
        operation: &'static str,
    },

    /// A well-formed backend id that this build does not implement.
    #[error("secrets backend {backend} is not available in this build")]
    UnknownBackend {
        /// The backend id named by config.
        backend: String,
    },

    /// The names-only index exists but does not parse. Never reset silently.
    #[error("secrets index {path} is corrupt: {reason}")]
    IndexCorrupt {
        /// The index file.
        path: PathBuf,
        /// What was wrong, by position or rule, never by content.
        reason: String,
    },

    /// A filesystem operation failed.
    #[error("I/O error on {path}: {source}")]
    Io {
        /// The path being read or written.
        path: PathBuf,
        /// The OS error.
        #[source]
        source: std::io::Error,
    },

    /// The index lock was still held when the wait bound expired.
    #[error("timed out after {waited:?} waiting for the lock {path}")]
    LockTimeout {
        /// The lock sidecar.
        path: PathBuf,
        /// The bound that expired.
        waited: Duration,
    },

    /// A config file exists but its `secrets:` section does not parse.
    #[error("secrets config {path} is invalid: {reason}")]
    Config {
        /// The config file.
        path: PathBuf,
        /// What was wrong, by position, never by content.
        reason: String,
    },

    /// No project scope could be derived for a directory.
    #[error("cannot determine the secrets scope for {dir}: {reason}")]
    ScopeUndetermined {
        /// The directory probed.
        dir: PathBuf,
        /// Why derivation failed.
        reason: &'static str,
    },

    /// `$HOME` is unknown, so a default location cannot be resolved.
    #[error("home directory is unavailable")]
    HomeUnavailable,
}
