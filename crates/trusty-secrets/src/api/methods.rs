//! Request and response types for the `secrets.*` methods (DOC-74 §15.2).
//!
//! Why: the console bridge and `tm` both speak these methods, and the bridge
//! depends on the `api` feature only. The types live here so both sides
//! share one definition and one set of validation rules.
//! What: one request/response pair per S1-shaped method. Name fields use the
//! validated newtypes, so a bad name fails at deserialization. Requests deny
//! unknown fields. `secrets.resolve` (S8) and `secrets.doctor` are not here.
//! The server side is S2.
//!
//! Every request, every response, [`ScopeKind`] and [`SetOutcome`] are
//! `#[non_exhaustive]`. Build a request with its `new` constructor, read a
//! response through serde, and give a `match` on either enum a wildcard arm,
//! so a field or variant added in a patch release does not break callers. A
//! new response field takes a serde default; the wire rule for a new request
//! field is in the crate docs, "Compatibility".
//! Test: `api_requests_fail_closed_on_bad_names_and_unknown_fields`,
//! `api_debug_of_value_carrying_types_hides_the_value`,
//! `api_request_constructors_match_the_wire_shape`.

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
#[non_exhaustive]
pub enum ScopeKind {
    /// `trusty/<owner>/<repo>`, or a `secrets.vault` override.
    Project,
    /// `trusty/<owner>`.
    Owner,
}

/// One pickable scope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ScopeInfo {
    /// Project or owner.
    pub kind: ScopeKind,
    /// The vault that scope reads and writes.
    pub vault: VaultName,
}

/// `secrets.scopes` response, project scope first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
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
#[non_exhaustive]
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
#[non_exhaustive]
pub struct ListRequest {
    /// The vault to list.
    pub vault: VaultName,
}

impl ListRequest {
    /// A request to list `vault`.
    pub fn new(vault: VaultName) -> Self {
        Self { vault }
    }
}

/// `secrets.list` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ListResponse {
    /// The vault listed.
    pub vault: VaultName,
    /// Its keys, sorted by name.
    pub keys: Vec<KeyMeta>,
}

/// `secrets.set` request. The only request that carries a value.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct SetRequest {
    /// The vault to write.
    pub vault: VaultName,
    /// The key to write.
    pub key: SecretKey,
    /// The value; `Debug` redacts it.
    pub value: SecretValue,
}

impl SetRequest {
    /// A request to write `value` under `key` in `vault`.
    pub fn new(vault: VaultName, key: SecretKey, value: SecretValue) -> Self {
        Self { vault, key, value }
    }
}

/// Whether `set` created or replaced a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum SetOutcome {
    /// The key did not exist.
    New,
    /// The key existed and was replaced.
    Updated,
}

/// `secrets.set` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct SetResponse {
    /// New or updated.
    pub outcome: SetOutcome,
    /// `mask_secret` output, shown once as the set confirmation.
    pub masked: String,
}

/// `secrets.delete` request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct DeleteRequest {
    /// The vault.
    pub vault: VaultName,
    /// The key to remove.
    pub key: SecretKey,
}

impl DeleteRequest {
    /// A request to remove `key` from `vault`.
    pub fn new(vault: VaultName, key: SecretKey) -> Self {
        Self { vault, key }
    }
}

/// `secrets.delete` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
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
#[non_exhaustive]
pub struct CopyRequest {
    /// Source backend.
    pub from_backend: BackendId,
    /// Destination backend.
    pub to_backend: BackendId,
    /// Keys to copy; empty means every indexed key.
    #[serde(default)]
    pub keys: Vec<SecretKey>,
}

impl CopyRequest {
    /// A request to copy every indexed key from `from_backend` to
    /// `to_backend`; narrow it with [`CopyRequest::with_keys`].
    ///
    /// What: `keys` starts empty, the same value the wire decodes when the
    /// field is omitted.
    /// Test: `api_request_constructors_match_the_wire_shape`.
    pub fn new(from_backend: BackendId, to_backend: BackendId) -> Self {
        Self {
            from_backend,
            to_backend,
            keys: Vec::new(),
        }
    }

    /// Copy only `keys`. An empty list means every indexed key.
    pub fn with_keys(mut self, keys: impl IntoIterator<Item = SecretKey>) -> Self {
        self.keys = keys.into_iter().collect();
        self
    }
}

/// `secrets.copy` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct CopyResponse {
    /// Keys copied.
    pub copied: Vec<SecretKey>,
    /// Keys that failed.
    pub failed: Vec<SecretKey>,
}
