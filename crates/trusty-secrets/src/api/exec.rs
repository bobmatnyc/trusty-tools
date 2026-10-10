//! Request and response types for the exec-grant methods (DOC-74 §15.8,
//! #9070 slice 2).
//!
//! Why: `tm secrets exec` registers a grant with `secrets.grant`, the granted
//! child reads values with `secrets.resolve`, and tm removes the grant with
//! `secrets.revoke`. The Rust client (S9) and the Python and npm clients
//! (S10) speak the same shapes, so they live with the other `api` types.
//! What: one request/response pair per method. A token is plain text on the
//! wire; every type that carries one has a `Debug` that redacts it, and
//! [`ResolveResponse`]'s value is a [`SecretValue`], whose `Debug` redacts.
//! `secrets.grant` takes `project` beside its fields, as every S2 method
//! does; `secrets.resolve` and `secrets.revoke` take none, because the grant
//! already names its project.
//! Test: `api_exec_grant_types_redact_tokens_and_values`,
//! `api_exec_grant_requests_match_the_wire_shape`.

use std::collections::BTreeSet;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::api::{SecretKey, SecretValue, VaultName};

/// `secrets.grant` request, beside `project`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct ExecGrantRequest {
    /// The granted child's pid; it and its descendants may resolve.
    pub child_pid: u32,
    /// The keys the child may resolve. Must not be empty.
    pub keys: BTreeSet<SecretKey>,
    /// The requested lifetime in seconds; the server caps it.
    pub ttl_secs: u64,
    /// Remove the grant on its first successful resolve.
    #[serde(default)]
    pub one_shot: bool,
}

impl ExecGrantRequest {
    /// A reusable grant of `keys` to `child_pid` for `ttl_secs`.
    pub fn new(child_pid: u32, keys: impl IntoIterator<Item = SecretKey>, ttl_secs: u64) -> Self {
        Self {
            child_pid,
            keys: keys.into_iter().collect(),
            ttl_secs,
            one_shot: false,
        }
    }

    /// The same request, removed after its first successful resolve.
    pub fn one_shot(mut self) -> Self {
        self.one_shot = true;
        self
    }
}

/// `secrets.grant` response. `Debug` redacts the token.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ExecGrantResponse {
    /// The token to hand the child, through its environment only.
    pub token: String,
    /// Expiry, in seconds since the Unix epoch.
    pub expires_at: u64,
    /// The lifetime granted, after the server's cap.
    pub ttl_secs: u64,
}

impl fmt::Debug for ExecGrantResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ExecGrantResponse")
            .field("token", &"<redacted>")
            .field("expires_at", &self.expires_at)
            .field("ttl_secs", &self.ttl_secs)
            .finish()
    }
}

/// `secrets.resolve` request. `Debug` redacts the token.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct ResolveRequest {
    /// The grant token from the child's environment.
    pub token: String,
    /// The key to read.
    pub key: SecretKey,
}

impl ResolveRequest {
    /// A request to read `key` under `token`.
    pub fn new(token: impl Into<String>, key: SecretKey) -> Self {
        Self {
            token: token.into(),
            key,
        }
    }
}

impl fmt::Debug for ResolveRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ResolveRequest")
            .field("token", &"<redacted>")
            .field("key", &self.key)
            .finish()
    }
}

/// `secrets.resolve` response: the only response that carries a value.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ResolveResponse {
    /// The key read.
    pub key: SecretKey,
    /// The value; `Debug` redacts it.
    pub value: SecretValue,
}

/// `secrets.revoke` request. `Debug` redacts the token.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct RevokeRequest {
    /// The grant token to remove.
    pub token: String,
}

impl RevokeRequest {
    /// A request to remove the grant `token` names.
    pub fn new(token: impl Into<String>) -> Self {
        Self {
            token: token.into(),
        }
    }
}

impl fmt::Debug for RevokeRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RevokeRequest")
            .field("token", &"<redacted>")
            .finish()
    }
}

/// `secrets.revoke` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct RevokeResponse {
    /// Whether a live grant matched the token.
    pub revoked: bool,
}

/// `secrets.set_agents_may_use` request, beside `project` (#9070 slice 3).
///
/// Why: DOC-74 §15.8 — the "agents may use" flag is set through the socket,
/// so the server can judge the caller's ancestry and audit the change.
/// What: the same shape as `secrets.set`, with `allowed` in place of the
/// value. It carries no secret.
/// Test: `api_set_agents_may_use_request_matches_the_wire_shape`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct SetAgentsMayUseRequest {
    /// The vault that holds the key.
    pub vault: VaultName,
    /// The key whose flag changes.
    pub key: SecretKey,
    /// `true` turns the flag on, `false` off.
    pub allowed: bool,
}

impl SetAgentsMayUseRequest {
    /// A request to set `key`'s flag in `vault` to `allowed`.
    pub fn new(vault: VaultName, key: SecretKey, allowed: bool) -> Self {
        Self {
            vault,
            key,
            allowed,
        }
    }
}

/// `secrets.set_agents_may_use` response: the flag as set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct SetAgentsMayUseResponse {
    /// The vault that holds the key.
    pub vault: VaultName,
    /// The key whose flag changed.
    pub key: SecretKey,
    /// The flag's value now.
    pub allowed: bool,
}
