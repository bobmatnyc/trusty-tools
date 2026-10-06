//! Fixed error text for every `secrets.*` failure (DOC-74 §15.6, S3a row 1).
//!
//! Why: the UDS router's own decode error is `params do not decode: {e}`, and
//! a serde message can quote the field it rejected — for `secrets.set`, the
//! value. Every failure this server reports is therefore one of a closed set
//! of kinds, and the wire text is built from the method name and the kind
//! alone. No request field, path, or library message ever reaches it.
//! What: [`ErrorKind`] (one variant per failure class), its mapping from
//! [`SecretsError`], and [`ErrorKind::to_rpc`], which renders
//! `"<method>: <fixed text>"` plus `data: {"kind": "<kind>"}`.
//! Test: `server_error_text_is_fixed_per_method_and_kind`,
//! `server_malformed_set_never_echoes_its_value`,
//! `server_resolver_errors_have_their_own_wire_kinds`.

use trusty_common::uds::server::{CODE_INTERNAL_ERROR, CODE_INVALID_PARAMS, RpcError};

use crate::api::SecretsError;

/// Every failure a `secrets.*` method reports.
///
/// Why: see the module docs. A variant carries no data, so nothing a caller
/// sent can travel back in it.
/// What: `Copy`; [`ErrorKind::as_str`] is the machine-readable kind,
/// [`ErrorKind::text`] the human sentence, [`ErrorKind::code`] the JSON-RPC
/// code. `#[non_exhaustive]` binds other crates only: the matches in this
/// file stay exhaustive, so a new kind fails the build until it has a kind
/// string, a sentence and a code.
/// Test: `server_error_text_is_fixed_per_method_and_kind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ErrorKind {
    /// The params are not an object of the method's shape.
    InvalidParams,
    /// The `project` path is not an absolute path to a directory.
    ProjectInvalid,
    /// No project or owner scope could be derived for the project.
    ProjectUnresolved,
    /// The request named a vault outside the project's scopes.
    VaultOutOfScope,
    /// The value was refused before it reached a backend.
    InvalidValue,
    /// The key is not in the vault.
    NotFound,
    /// The backend cannot perform the operation.
    Unsupported,
    /// The configured backend is not in this build.
    UnknownBackend,
    /// The backend failed.
    BackendFailed,
    /// The backend took a new key, the index write failed, and the cleanup
    /// delete failed too: the backend may hold an entry no index row lists.
    // #9065: distinct from `BackendFailed` so a caller knows to reconcile.
    OrphanedBackendEntry,
    /// The names-only index does not parse. Never reset.
    IndexCorrupt,
    /// The index lock stayed held past its wait bound.
    IndexBusy,
    /// A secrets file could not be read or written.
    StorageUnavailable,
    /// A `secrets:` config section does not parse.
    ConfigInvalid,
    /// `$HOME` is unknown.
    HomeUnavailable,
    /// `copy` named the same backend twice.
    SameBackend,
    /// An agent-parent process asked for a key not flagged "agents may use".
    // #7525: its own kind so a refusal never reads as a miss or a failure.
    AgentUseRefused,
    /// An env-map entry broke a rule before any resolution started.
    InvalidEnvEntry,
    /// A `secret://` reference in an env map did not resolve.
    EnvResolutionFailed,
    /// A `.env` line is outside the supported subset.
    DotenvSyntax,
    /// A server-side fault, e.g. a handler task that did not finish.
    Internal,
}

impl ErrorKind {
    /// The machine-readable kind, sent as `error.data.kind`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InvalidParams => "invalid_params",
            Self::ProjectInvalid => "project_invalid",
            Self::ProjectUnresolved => "project_unresolved",
            Self::VaultOutOfScope => "vault_out_of_scope",
            Self::InvalidValue => "invalid_value",
            Self::NotFound => "not_found",
            Self::Unsupported => "unsupported",
            Self::UnknownBackend => "unknown_backend",
            Self::BackendFailed => "backend_failed",
            Self::OrphanedBackendEntry => "orphaned_backend_entry",
            Self::IndexCorrupt => "index_corrupt",
            Self::IndexBusy => "index_busy",
            Self::StorageUnavailable => "storage_unavailable",
            Self::ConfigInvalid => "config_invalid",
            Self::HomeUnavailable => "home_unavailable",
            Self::SameBackend => "same_backend",
            Self::AgentUseRefused => "agent_use_refused",
            Self::InvalidEnvEntry => "invalid_env_entry",
            Self::EnvResolutionFailed => "env_resolution_failed",
            Self::DotenvSyntax => "dotenv_syntax",
            Self::Internal => "internal",
        }
    }

    /// The fixed human sentence for this kind.
    pub fn text(self) -> &'static str {
        match self {
            Self::InvalidParams => "the params do not match this method's request shape",
            Self::ProjectInvalid => "`project` must be an absolute path to a directory",
            Self::ProjectUnresolved => {
                "cannot determine the project's scopes from its git remote or config"
            }
            Self::VaultOutOfScope => "the vault is not one of this project's scopes",
            Self::InvalidValue => "the secret value was refused",
            Self::NotFound => "the key is not in that vault",
            Self::Unsupported => "the backend does not support this operation",
            Self::UnknownBackend => "the configured backend is not available in this build",
            Self::BackendFailed => "the secrets backend failed",
            Self::OrphanedBackendEntry => {
                "the index write and its cleanup both failed; the backend may hold an entry no index row lists"
            }
            Self::IndexCorrupt => "the secrets index is corrupt; it was left untouched",
            Self::IndexBusy => "the secrets index is busy; retry",
            Self::StorageUnavailable => "a secrets file could not be read or written",
            Self::ConfigInvalid => "a `secrets:` config section is invalid",
            Self::HomeUnavailable => "the home directory is unavailable",
            Self::SameBackend => "the source and destination backends are the same",
            Self::AgentUseRefused => {
                "the key is not flagged \"agents may use\"; refused under a Claude Code parent"
            }
            Self::InvalidEnvEntry => "an env-map entry is invalid",
            Self::EnvResolutionFailed => "a `secret://` reference in the env map did not resolve",
            Self::DotenvSyntax => "a `.env` line is outside the supported syntax",
            Self::Internal => "internal error",
        }
    }

    /// The JSON-RPC error code: `-32602` for bad params, `-32603` for
    /// [`ErrorKind::Internal`], and `-32050` minus the kind's index otherwise.
    /// A new kind takes the next unused code, so no code changes meaning.
    pub fn code(self) -> i64 {
        match self {
            Self::InvalidParams => CODE_INVALID_PARAMS,
            Self::Internal => CODE_INTERNAL_ERROR,
            Self::ProjectInvalid => -32050,
            Self::ProjectUnresolved => -32051,
            Self::VaultOutOfScope => -32052,
            Self::InvalidValue => -32053,
            Self::NotFound => -32054,
            Self::Unsupported => -32055,
            Self::UnknownBackend => -32056,
            Self::BackendFailed => -32057,
            Self::IndexCorrupt => -32058,
            Self::IndexBusy => -32059,
            Self::StorageUnavailable => -32060,
            Self::ConfigInvalid => -32061,
            Self::HomeUnavailable => -32062,
            Self::SameBackend => -32063,
            Self::OrphanedBackendEntry => -32064,
            Self::AgentUseRefused => -32065,
            Self::InvalidEnvEntry => -32066,
            Self::EnvResolutionFailed => -32067,
            Self::DotenvSyntax => -32068,
        }
    }

    /// The wire error for this kind on `method`.
    ///
    /// What: message `"<method>: <text>"`, code [`ErrorKind::code`], data
    /// `{"kind": <as_str>}`. `method` is always one of this server's own
    /// `&'static` method names, never the caller's string.
    /// Test: `server_error_text_is_fixed_per_method_and_kind`.
    pub fn to_rpc(self, method: &'static str) -> RpcError {
        RpcError::new(self.code(), format!("{method}: {}", self.text()))
            .with_data(serde_json::json!({ "kind": self.as_str() }))
    }
}

impl From<SecretsError> for ErrorKind {
    /// Fold a store error into its kind, dropping every field it carried.
    fn from(error: SecretsError) -> Self {
        match error {
            SecretsError::InvalidKey { .. }
            | SecretsError::InvalidName { .. }
            | SecretsError::InvalidReference { .. } => Self::InvalidParams,
            SecretsError::InvalidValue { .. } => Self::InvalidValue,
            SecretsError::NotFound { .. } => Self::NotFound,
            SecretsError::Backend { .. } => Self::BackendFailed,
            SecretsError::OrphanedBackendEntry { .. } => Self::OrphanedBackendEntry,
            SecretsError::Unsupported { .. } => Self::Unsupported,
            SecretsError::UnknownBackend { .. } => Self::UnknownBackend,
            SecretsError::IndexCorrupt { .. } => Self::IndexCorrupt,
            SecretsError::Io { .. } => Self::StorageUnavailable,
            SecretsError::LockTimeout { .. } => Self::IndexBusy,
            SecretsError::Config { .. } => Self::ConfigInvalid,
            SecretsError::ScopeUndetermined { .. } => Self::ProjectUnresolved,
            SecretsError::AgentUseRefused { .. } => Self::AgentUseRefused,
            // #7525: `resolve_env` wraps a refusal in `EnvResolution`; keep it
            // a refusal on the wire.
            SecretsError::EnvResolution { source, .. }
                if matches!(*source, SecretsError::AgentUseRefused { .. }) =>
            {
                Self::AgentUseRefused
            }
            SecretsError::EnvResolution { .. } => Self::EnvResolutionFailed,
            SecretsError::InvalidEnvEntry { .. } => Self::InvalidEnvEntry,
            SecretsError::DotenvSyntax { .. } => Self::DotenvSyntax,
            // Exhaustive on purpose: inside this crate `#[non_exhaustive]` does
            // not apply, so a new `SecretsError` variant fails the build here
            // until it is given a kind.
            SecretsError::HomeUnavailable => Self::HomeUnavailable,
        }
    }
}
