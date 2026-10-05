//! Request and response types for the `secrets.*` methods (DOC-74 §15.2).
//!
//! Why: the console bridge and `tm` both speak these methods, and the bridge
//! depends on the `api` feature only. The types live here so both sides
//! share one definition and one set of validation rules.
//! What: one request/response pair per S1-shaped method. Name fields use the
//! validated newtypes, so a bad name fails at deserialization. Requests deny
//! unknown fields. `secrets.resolve` (S8) and `secrets.doctor` are not here.
//! The server side is S2.
//! Test: `api_requests_fail_closed_on_bad_names_and_unknown_fields`,
//! `api_debug_of_value_carrying_types_hides_the_value`.

use serde::{Deserialize, Serialize};

use super::{BackendId, SecretKey, SecretValue, VaultName};

/// Method names.
pub mod method {
    /// List the owner and project scopes a caller can pick.
    pub const SCOPES: &str = "secrets.scopes";
    /// List the keys in one vault.
    pub const LIST: &str = "secrets.list";
    /// Upsert one key.
    pub const SET: &str = "secrets.set";
    /// Remove one key.
    pub const DELETE: &str = "secrets.delete";
    /// Copy keys between backends inside one project.
    pub const COPY: &str = "secrets.copy";
}

/// Which kind of scope a vault serves (DOC-74 §15.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScopeKind {
    /// `trusty/<owner>/<repo>`, or a `secrets.vault` override.
    Project,
    /// `trusty/<owner>`.
    Owner,
}

/// One pickable scope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopeInfo {
    /// Project or owner.
    pub kind: ScopeKind,
    /// The vault that scope reads and writes.
    pub vault: VaultName,
}

/// `secrets.scopes` response, project scope first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopesResponse {
    /// The scopes, in resolution order.
    pub scopes: Vec<ScopeInfo>,
}

/// Per-key metadata. Never carries characters of the value.
///
/// Why: DOC-74 §15.6 — `list` shows length and `updated_at` only.
/// What: the row the names-only index stores, as it crosses the wire.
/// Test: `store_list_reports_length_and_time_never_characters`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyMeta {
    /// The key name.
    pub name: SecretKey,
    /// Value length in characters.
    pub length: usize,
    /// Last write, in seconds since the Unix epoch.
    pub updated_at: u64,
    /// Whether `exec` may inject this key into a Claude Code process tree
    /// (DOC-74 §15.8). Default OFF.
    pub agents_may_use: bool,
}

/// `secrets.list` request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListRequest {
    /// The vault to list.
    pub vault: VaultName,
}

/// `secrets.list` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListResponse {
    /// The vault listed.
    pub vault: VaultName,
    /// Its keys, sorted by name.
    pub keys: Vec<KeyMeta>,
}

/// `secrets.set` request. The only request that carries a value.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetRequest {
    /// The vault to write.
    pub vault: VaultName,
    /// The key to write.
    pub key: SecretKey,
    /// The value; `Debug` redacts it.
    pub value: SecretValue,
}

/// Whether `set` created or replaced a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SetOutcome {
    /// The key did not exist.
    New,
    /// The key existed and was replaced.
    Updated,
}

/// `secrets.set` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetResponse {
    /// New or updated.
    pub outcome: SetOutcome,
    /// `mask_secret` output, shown once as the set confirmation.
    pub masked: String,
}

/// `secrets.delete` request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeleteRequest {
    /// The vault.
    pub vault: VaultName,
    /// The key to remove.
    pub key: SecretKey,
}

/// `secrets.delete` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeleteResponse {
    /// Whether the backend or the index held the key.
    pub removed: bool,
}

/// `secrets.copy` request: one project's keys, between two backends.
///
/// Why: DOC-74 §13 Q6 — copy stays in-project. The request names no vault;
/// the server uses the caller's own project scope, so a cross-project copy
/// cannot be expressed. The copy itself ships with S2.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CopyRequest {
    /// Source backend.
    pub from_backend: BackendId,
    /// Destination backend.
    pub to_backend: BackendId,
    /// Keys to copy; empty means every indexed key.
    #[serde(default)]
    pub keys: Vec<SecretKey>,
}

/// `secrets.copy` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CopyResponse {
    /// Keys copied.
    pub copied: Vec<SecretKey>,
    /// Keys that failed.
    pub failed: Vec<SecretKey>,
}
