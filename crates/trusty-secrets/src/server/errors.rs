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
/// Every variant carries an explicit discriminant, and those values are a
/// public contract: a new kind takes the next unused value after the last
/// one, never a slot before it (see #7524 and the trusty-secrets 0.1.2
/// accepted-break declaration).
/// Test: `server_error_text_is_fixed_per_method_and_kind`,
/// `error_kind_all_lists_every_variant_once`,
/// `error_kind_discriminants_are_pinned`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ErrorKind {
    /// The params are not an object of the method's shape.
    InvalidParams = 0,
    /// The `project` path is not an absolute path to a directory.
    ProjectInvalid = 1,
    /// No project or owner scope could be derived for the project.
    ProjectUnresolved = 2,
    /// The request named a vault outside the project's scopes.
    VaultOutOfScope = 3,
    /// The value was refused before it reached a backend.
    InvalidValue = 4,
    /// The key is not in the vault.
    NotFound = 5,
    /// The backend cannot perform the operation.
    Unsupported = 6,
    /// The configured backend is not in this build.
    UnknownBackend = 7,
    /// The backend failed.
    BackendFailed = 8,
    /// The backend took a new key, the index write failed, and the cleanup
    /// delete failed too: the backend may hold an entry no index row lists.
    // #9065: distinct from `BackendFailed` so a caller knows to reconcile.
    OrphanedBackendEntry = 9,
    /// The names-only index does not parse. Never reset.
    IndexCorrupt = 10,
    /// The index lock stayed held past its wait bound.
    IndexBusy = 11,
    /// A secrets file could not be read or written.
    StorageUnavailable = 12,
    /// A `secrets:` config section does not parse.
    ConfigInvalid = 13,
    /// `$HOME` is unknown.
    HomeUnavailable = 14,
    /// `copy` named the same backend twice.
    SameBackend = 15,
    /// An agent-parent process asked for a key not flagged "agents may use".
    // #7525: its own kind so a refusal never reads as a miss or a failure.
    AgentUseRefused = 16,
    /// An env-map entry broke a rule before any resolution started.
    InvalidEnvEntry = 17,
    /// A `secret://` reference in an env map did not resolve.
    EnvResolutionFailed = 18,
    /// A `.env` line is outside the supported subset.
    DotenvSyntax = 19,
    /// The project's `origin` remote is not on github.com.
    // #9328: owner ruling 06 R3, its own kind so it never reads as a guess.
    RemoteHostUnsupported = 20,
    /// A value file or directory failed its mode, owner or symlink check.
    // #9326: its own kind so a refusal never reads as an I/O failure.
    StorageRefused = 21,
    /// The tracked project config selected `file` on a Keychain build.
    // #9326: Architect ruling, basis ruling 06 R2.
    TrackedBackendRefused = 22,
    /// The credential access audit record could not be guaranteed.
    // #4567: fail-closed on an allowed set, delete or copy (DOC-45 C-7.12).
    AuditUnavailable = 23,
    /// The tracked project config tried to turn the audit off.
    // #4567: DOC-45 C-7.10 — only the untracked machine config may.
    TrackedAuditRefused = 24,
    /// The tracked project config set a CLI `account` or `config_path`.
    // #7519: owner ruling 2026-10-07 — only the machine config may.
    TrackedCliSettingRefused = 25,
    /// A CLI-backed backend's program is not installed.
    // #7519: A4 — fails closed with its own kind, never a generic failure.
    CliNotInstalled = 26,
    /// A CLI-backed backend is locked or signed out; never a miss.
    // #7519: A3/A4.
    BackendLocked = 27,
    /// A value write into `file` on a Keychain build without the machine
    /// config's selection.
    // #7524 H1: owner ruling item 74.
    FileBackendNotSelected = 28,
    /// A server-side fault, e.g. a handler task that did not finish.
    Internal = 29,
    /// A CLI-backed backend the machine config has not enabled.
    // #7519: P1 carry-over (a); refused before any process is spawned.
    BackendNotEnabled = 30,
}

impl ErrorKind {
    /// Every kind, for the wire-to-kind lookup and the kind-table tests.
    pub(crate) const ALL: [Self; 31] = [
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
        Self::TrackedCliSettingRefused,
        Self::CliNotInstalled,
        Self::BackendLocked,
        Self::FileBackendNotSelected,
        Self::Internal,
        Self::BackendNotEnabled,
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
            Self::TrackedCliSettingRefused => "tracked_cli_setting_refused",
            Self::CliNotInstalled => "cli_not_installed",
            Self::BackendLocked => "backend_locked",
            Self::FileBackendNotSelected => "file_backend_not_selected",
            Self::BackendNotEnabled => "backend_not_enabled",
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
            Self::TrackedCliSettingRefused => {
                "the project's tracked config may not set a backend `account` or `config_path`; set it in the machine config ~/.trusty-tools/trusty-common/config.yaml instead"
            }
            // #7524 P2-M2: names the `program` pin; no backend searches PATH.
            Self::CliNotInstalled => {
                "the secrets backend's command-line tool was not found; set `secrets.<backend>.program` in the machine config ~/.trusty-tools/trusty-common/config.yaml to its absolute path (1Password also looks in fixed system directories, never PATH)"
            }
            // #7519 P3: ruling 6 — the wire text names Keeper's one-time step.
            Self::BackendLocked => {
                "the secrets backend is locked or signed out; unlock or sign in to it and retry (Keeper first needs a person to approve this device and turn on persistent login)"
            }
            Self::FileBackendNotSelected => {
                "this build has a Keychain, so a value is written to the plaintext `file` backend only when the machine config selects it; set `secrets.default_backend: file` in the machine config ~/.trusty-tools/trusty-common/config.yaml to allow it"
            }
            Self::BackendNotEnabled => {
                "the secrets backend is not enabled on this machine; enable it in the machine config ~/.trusty-tools/trusty-common/config.yaml"
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
            // #7519: the next unused codes.
            Self::TrackedCliSettingRefused => -32074,
            Self::CliNotInstalled => -32075,
            Self::BackendLocked => -32076,
            // #7524: the next unused code.
            Self::FileBackendNotSelected => -32077,
            // #7519: the next unused code after #7524's.
            Self::BackendNotEnabled => -32078,
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
            // #7519: the tracked CLI-setting refusal and the CLI backends' failures.
            SecretsError::TrackedCliSettingRefused { .. } => Self::TrackedCliSettingRefused,
            SecretsError::CliNotInstalled { .. } => Self::CliNotInstalled,
            SecretsError::BackendLocked { .. } => Self::BackendLocked,
            // #7524 H1: a write into `file` the machine config did not select.
            SecretsError::FileBackendNotSelected => Self::FileBackendNotSelected,
            SecretsError::BackendNotEnabled { .. } => Self::BackendNotEnabled,
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
