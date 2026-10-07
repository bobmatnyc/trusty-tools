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

    /// The index write failed after a new key reached the backend, and the
    /// delete that should have undone it failed too. The backend may still
    /// hold an entry that no index row lists.
    #[error(
        "{backend} backend may still hold {key} in {vault} with no index row: \
         the index write failed ({source}) and the cleanup delete failed ({cleanup})"
    )]
    OrphanedBackendEntry {
        /// Backend id.
        backend: String,
        /// The vault addressed.
        vault: String,
        /// The key that may be orphaned.
        key: String,
        /// The index publish failure.
        #[source]
        source: Box<SecretsError>,
        /// The failed cleanup delete.
        cleanup: Box<SecretsError>,
    },

    /// A process whose parent chain includes Claude Code asked for a key that
    /// is not flagged "agents may use" (DOC-74 §15.8). Raised before any
    /// backend read.
    #[error(
        "secret {key} in {vault} is not flagged \"agents may use\"; \
         refusing to resolve it under a Claude Code parent"
    )]
    AgentUseRefused {
        /// The key refused.
        key: String,
        /// The vault whose index row carries the flag.
        vault: String,
    },

    /// An env-map entry is unusable before any resolution starts.
    #[error("env entry {position}: {reason}")]
    InvalidEnvEntry {
        /// 1-based position of the entry in the env map.
        position: usize,
        /// Which rule the entry broke. Never the name or value text.
        reason: &'static str,
    },

    /// A `secret://` reference in an env map did not resolve.
    #[error("env {name}: cannot resolve {reference}: {source}")]
    EnvResolution {
        /// The env variable name, already validated.
        name: String,
        /// The canonical reference, or a fixed phrase when it did not parse.
        reference: String,
        /// Why resolution failed.
        #[source]
        source: Box<SecretsError>,
    },

    /// A `.env` line is outside the supported subset.
    #[error(".env line {line}: {reason}")]
    DotenvSyntax {
        /// 1-based line number.
        line: usize,
        /// Which rule the line broke. Never the line's text.
        reason: &'static str,
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

    /// A pinned reference or a tracked `secrets.vault` override named a vault
    /// outside the caller's own scopes. Raised before any index or backend
    /// read, so it says nothing about what that vault holds.
    // #9328: owner ruling 06 R1 and R2.
    #[error("vault {vault} is outside this project's scopes: {reason}")]
    VaultOutOfScope {
        /// The vault named, already validated.
        vault: String,
        /// Which rule refused it.
        reason: &'static str,
    },

    /// The `origin` remote is not on github.com.
    // #9328: owner ruling 06 R3 — GHES and other hosts come later.
    #[error(
        "cannot determine the secrets scope for {dir}: the `origin` remote is not on \
         github.com, and only github.com remotes are supported"
    )]
    UnsupportedRemoteHost {
        /// The directory probed.
        dir: PathBuf,
    },

    /// A value file or directory failed its safety check: a symlink, the
    /// wrong file type, permissions beyond 0600/0700, or another owner.
    /// Raised before any value is read; the operation does not proceed.
    // #9326: the file backend's refusal; names the path, never the value.
    #[error("secrets storage {path} is refused: it {reason}")]
    StorageRefused {
        /// The file or directory refused.
        path: PathBuf,
        /// Which rule it broke.
        reason: &'static str,
    },

    /// A tracked project config selected the `file` backend on a build with a
    /// Keychain, where only the untracked machine config may. Raised before
    /// any backend is opened.
    // #9326: Architect ruling, basis ruling 06 R2; never echoes the file.
    #[error(
        "secrets config {path} is tracked and may not select the `file` backend \
         on a build with a Keychain; remove `backend: file` from it and set \
         `secrets.default_backend: file` in the machine config \
         ~/.trusty-tools/trusty-common/config.yaml instead"
    )]
    TrackedBackendRefused {
        /// The tracked project config file.
        path: PathBuf,
    },

    /// `$HOME` is unknown, so a default location cannot be resolved.
    #[error("home directory is unavailable")]
    HomeUnavailable,
}
