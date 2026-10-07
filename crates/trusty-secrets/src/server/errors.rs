//! Fixed error text for every `secrets.*` failure (DOC-74 §15.6, S3a row 1).
//!
//! Why: the UDS router's own decode error is `params do not decode: {e}`, and
//! a serde message can quote the field it rejected — for `secrets.set`, the
//! value. Every failure this server reports is therefore one of a closed set
//! of kinds, and the wire text is built from the method name and the kind
//! alone. No request field, path, or library message ever reaches it.
//! What: [`ErrorKind`] (one variant per failure class), its mapping from
//! [`SecretsError`], and the crate-private `ErrorKind::to_rpc`, which renders
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
/// string, a sentence and a code. A new kind must also join
/// `ErrorKind::ALL`, or the client reads it from the wire as `None`.
/// Test: `server_error_text_is_fixed_per_method_and_kind`,
/// `error_kind_all_lists_every_variant_once`.
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
    /// The project's `origin` remote is not on github.com.
    // #9328: owner ruling 06 R3, its own kind so it never reads as a guess.
    RemoteHostUnsupported,
    /// A value file or directory failed its mode, owner or symlink check.
    // #9326: its own kind so a refusal never reads as an I/O failure.
    StorageRefused,
    /// The tracked project config selected `file` on a Keychain build.
    // #9326: Architect ruling, basis ruling 06 R2.
    TrackedBackendRefused,
    /// The credential access audit record could not be guaranteed.
    // #4567: fail-closed on an allowed set, delete or copy (DOC-45 C-7.12).
    AuditUnavailable,
    /// The tracked project config tried to turn the audit off.
    // #4567: DOC-45 C-7.10 — only the untracked machine config may.
    TrackedAuditRefused,
    /// A server-side fault, e.g. a handler task that did not finish.
    Internal,
}

impl ErrorKind {
    /// Every kind, for the wire-to-kind lookup and the kind-table tests.
    pub(crate) const ALL: [Self; 26] = [
        Self::InvalidParams,
        Self::ProjectInvalid,
        Self::ProjectUnresolved,
        Self::VaultOutOfScope,
        Self::InvalidValue,
        Self::NotFound,
        Self::Unsupported,
        Self::UnknownBackend,
        Self::BackendFailed,
        Self::OrphanedBackendEntry,
        Self::IndexCorrupt,
        Self::IndexBusy,
        Self::StorageUnavailable,
        Self::ConfigInvalid,
        Self::HomeUnavailable,
        Self::SameBackend,
        Self::AgentUseRefused,
        Self::InvalidEnvEntry,
        Self::EnvResolutionFailed,
        Self::DotenvSyntax,
        Self::RemoteHostUnsupported,
        Self::StorageRefused,
        Self::TrackedBackendRefused,
        Self::AuditUnavailable,
        Self::TrackedAuditRefused,
        Self::Internal,
    ];

    /// The kind whose [`ErrorKind::as_str`] is `kind`; `None` for a kind this
    /// build does not know, e.g. one a newer server sent.
    /// Test: `client_rpc_failure_reads_every_kind_from_the_wire`.
    pub(crate) fn from_wire(kind: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == kind)
    }

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
            Self::RemoteHostUnsupported => "remote_host_unsupported",
            Self::StorageRefused => "storage_refused",
            Self::TrackedBackendRefused => "tracked_backend_refused",
            Self::AuditUnavailable => "audit_unavailable",
            Self::TrackedAuditRefused => "tracked_audit_refused",
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
            Self::RemoteHostUnsupported => {
                "the project's `origin` remote is not on github.com; only github.com remotes are supported"
            }
            Self::StorageRefused => {
                "a secrets file or directory failed its permission, owner or symlink check; it was refused"
            }
            Self::TrackedBackendRefused => {
                "the project's tracked config may not select the `file` backend on a build with a Keychain; set `secrets.default_backend: file` in the machine config ~/.trusty-tools/trusty-common/config.yaml instead"
            }
            // #4567: fixed text, no path or value. Usually returned before the
            // backend call; after a completed one the change stands (DOC-45 C-7.7a).
            Self::AuditUnavailable => {
                "the credential access audit log could not be written; the requested change may already have been applied"
            }
            Self::TrackedAuditRefused => {
                "the project's tracked config may not turn the credential audit off; set `secrets.audit: false` in the machine config ~/.trusty-tools/trusty-common/config.yaml instead"
            }
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
            Self::RemoteHostUnsupported => -32069,
            // #9326: the next unused code.
            Self::StorageRefused => -32070,
            Self::TrackedBackendRefused => -32071,
            // #4567: the next unused codes.
            Self::AuditUnavailable => -32072,
            Self::TrackedAuditRefused => -32073,
        }
    }

    /// The wire error for this kind on `method`.
    ///
    /// What: message `"<method>: <text>"`, code [`ErrorKind::code`], data
    /// `{"kind": <as_str>}`. `method` is always one of this server's own
    /// `&'static` method names, never the caller's string.
    /// Test: `server_error_text_is_fixed_per_method_and_kind`.
    // #9073: crate-private; `RpcError` is trusty-common's type.
    pub(crate) fn to_rpc(self, method: &'static str) -> RpcError {
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
            // #9328: ruling 06 — out-of-scope vaults and non-github.com remotes.
            SecretsError::VaultOutOfScope { .. } => Self::VaultOutOfScope,
            SecretsError::UnsupportedRemoteHost { .. } => Self::RemoteHostUnsupported,
            // #9326: the file backend's mode, owner or symlink refusal.
            SecretsError::StorageRefused { .. } => Self::StorageRefused,
            SecretsError::TrackedBackendRefused { .. } => Self::TrackedBackendRefused,
            SecretsError::AgentUseRefused { .. } => Self::AgentUseRefused,
            // #7525: `resolve_env` wraps a refusal in `EnvResolution`; keep it
            // a refusal on the wire.
            SecretsError::EnvResolution { source, .. }
                if matches!(*source, SecretsError::AgentUseRefused { .. }) =>
            {
                Self::AgentUseRefused
            }
            // #9328: likewise an out-of-scope pinned reference.
            SecretsError::EnvResolution { source, .. }
                if matches!(*source, SecretsError::VaultOutOfScope { .. }) =>
            {
                Self::VaultOutOfScope
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
