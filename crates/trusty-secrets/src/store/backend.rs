//! The [`SecretBackend`] trait and its capability flags (DOC-74 §15.4).
//!
//! Why: one trait covers native stores (Keychain), CLI sources (1Password,
//! Keeper), and write-only sync targets (Vercel, GitHub Actions). Capability
//! flags let [`super::SecretStore`] refuse an operation a backend cannot do —
//! reading back a sync target, for example — before calling it.
//! What: [`Capabilities`], [`SecretBackend`], and [`open_backend`], which maps
//! a configured [`BackendId`] to an implementation.
//! Test: `store_capabilities_gate_operations`, `store_open_backend_knows_only_keychain`.

use std::fmt;
use std::ops::BitOr;
use std::sync::Arc;

use super::KeychainBackend;
use crate::api::{BackendId, SecretKey, SecretValue, SecretsError, VaultName};

/// What a backend can do.
///
/// What: a small bit set. Combine with `|`; test with
/// [`Capabilities::contains`].
/// Test: `store_capabilities_gate_operations`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Capabilities(u8);

impl Capabilities {
    /// Can return a value to the `store` feature.
    pub const READ: Self = Self(1);
    /// Can store a value.
    pub const WRITE: Self = Self(1 << 1);
    /// Can list key names without reading values.
    pub const LIST_NAMES: Self = Self(1 << 2);
    /// Write-only destination; never read back.
    pub const SYNC_TARGET: Self = Self(1 << 3);

    const NAMES: [(Self, &'static str); 4] = [
        (Self::READ, "READ"),
        (Self::WRITE, "WRITE"),
        (Self::LIST_NAMES, "LIST_NAMES"),
        (Self::SYNC_TARGET, "SYNC_TARGET"),
    ];

    /// No capabilities.
    pub const fn empty() -> Self {
        Self(0)
    }

    /// Whether every flag in `other` is set here.
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

impl BitOr for Capabilities {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

impl fmt::Debug for Capabilities {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let set: Vec<&str> = Self::NAMES
            .iter()
            .filter(|(flag, _)| self.contains(*flag))
            .map(|(_, name)| *name)
            .collect();
        write!(f, "Capabilities({})", set.join(" | "))
    }
}

/// A place secret values live.
///
/// Why: see the module docs.
/// What: value operations addressed by vault and key. Contract for every
/// implementation:
/// - A miss is `Ok(None)` from `get` and `Ok(false)` from `delete`. Any other
///   failure is an `Err` — never folded into a miss.
/// - No error, log line, or `Debug` output carries a value.
/// - No implementation caches values unless DOC-74 §15.5 allows it
///   (CLI-backed sources only).
///
/// Test: `store_backend_errors_are_never_downgraded`.
pub trait SecretBackend: Send + Sync + fmt::Debug {
    /// The backend id, e.g. `keychain`.
    fn id(&self) -> BackendId;

    /// What this backend can do.
    fn capabilities(&self) -> Capabilities;

    /// Read `key` from `vault`.
    fn get(&self, vault: &VaultName, key: &SecretKey) -> Result<Option<SecretValue>, SecretsError>;

    /// Store `value` under `key` in `vault`, replacing any previous value.
    fn set(
        &self,
        vault: &VaultName,
        key: &SecretKey,
        value: &SecretValue,
    ) -> Result<(), SecretsError>;

    /// Remove `key` from `vault`. Returns whether an entry existed.
    fn delete(&self, vault: &VaultName, key: &SecretKey) -> Result<bool, SecretsError>;

    /// Key names in `vault`, for backends with [`Capabilities::LIST_NAMES`].
    ///
    /// What: the default refuses with [`SecretsError::Unsupported`].
    fn list_names(&self, vault: &VaultName) -> Result<Vec<SecretKey>, SecretsError> {
        let _ = vault;
        Err(SecretsError::Unsupported {
            backend: self.id().to_string(),
            operation: "list_names",
        })
    }
}

/// The implementation for a configured backend id.
///
/// Why: config names a backend (DOC-74 §6.1); this is the one place that name
/// becomes code. A name this build does not implement fails closed rather
/// than falling back to the Keychain.
/// What: `keychain` → [`KeychainBackend`]; anything else →
/// [`SecretsError::UnknownBackend`].
/// Test: `store_open_backend_knows_only_keychain`.
pub fn open_backend(id: &BackendId) -> Result<Arc<dyn SecretBackend>, SecretsError> {
    match id.as_str() {
        BackendId::KEYCHAIN => Ok(Arc::new(KeychainBackend::new())),
        _ => Err(SecretsError::UnknownBackend {
            backend: id.to_string(),
        }),
    }
}
