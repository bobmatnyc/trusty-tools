//! The [`SecretBackend`] trait and its capability flags (DOC-74 §15.4).
//!
//! Why: one trait covers native stores (Keychain), CLI sources (1Password,
//! Keeper), and write-only sync targets (Vercel, GitHub Actions). Capability
//! flags let [`super::SecretStore`] refuse an operation a backend cannot do —
//! reading back a sync target, for example — before calling it.
//! What: [`Capabilities`], [`SecretBackend`], [`open_backend`], which maps
//! a configured [`BackendId`] to an implementation, [`default_backend`],
//! the backend used when no config names one, and [`local_backends`], every
//! backend a `delete` must clear.
//! Test: `store_capabilities_gate_operations`, `store_open_backend_knows_keychain_and_file`,
//! `store_keychain_failure_never_falls_through_to_file`.

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

/// Whether this build links an OS Keychain backend (macOS only, #9064).
pub(crate) const KEYCHAIN_COMPILED: bool = cfg!(target_os = "macos");

/// The backend used when neither the project nor the machine config names
/// one (DOC-74 §6.1).
///
/// Why: owner ruling f5 (2026-10-06) — the Keychain is the default; the
/// 0600 file backend is the default only where no Keychain exists.
/// What: `keychain` on a build with a Keychain backend (macOS), `file`
/// otherwise. Decided at build time, never by probing the Keychain, so a
/// Keychain that fails at run time is never swapped for files.
/// Test: `store_default_backend_is_keychain_unless_none_is_compiled`.
pub fn default_backend() -> BackendId {
    default_backend_for(KEYCHAIN_COMPILED)
}

/// [`default_backend`] for a build that does or does not link a Keychain.
pub(crate) fn default_backend_for(keychain_compiled: bool) -> BackendId {
    if keychain_compiled {
        BackendId::keychain()
    } else {
        // #9326: ruling f5 — files only where no Keychain is compiled in.
        BackendId::file()
    }
}

/// Every backend this build can have stored a value in (#7519).
///
/// Why: A5 — `delete` must clear every backend that may hold a key, not
/// only the configured one. A backend this build does not link holds
/// nothing it wrote, and off macOS every Keychain call fails closed, so
/// including it would fail every delete there.
/// What: `keychain` on a build with a Keychain backend, then `file` on Unix.
/// Decided at build time, like [`default_backend`], never by probing.
/// Test: `store_local_backends_follow_the_build`.
pub fn local_backends() -> Vec<BackendId> {
    local_backends_for(KEYCHAIN_COMPILED)
}

/// [`local_backends`] for a build that does or does not link a Keychain.
pub(crate) fn local_backends_for(keychain_compiled: bool) -> Vec<BackendId> {
    let mut ids = Vec::with_capacity(2);
    if keychain_compiled {
        ids.push(BackendId::keychain());
    }
    if cfg!(unix) {
        ids.push(BackendId::file());
    }
    ids
}

/// The implementation for a configured backend id.
///
/// Why: config names a backend (DOC-74 §6.1); this is the one place that name
/// becomes code. A name this build does not implement fails closed rather
/// than falling back to the Keychain.
/// What: `keychain` → [`KeychainBackend`]; `file` → the value-file backend
/// at its default location (Unix; elsewhere [`SecretsError::UnknownBackend`]);
/// anything else → [`SecretsError::UnknownBackend`].
/// Test: `store_open_backend_knows_keychain_and_file`.
pub fn open_backend(id: &BackendId) -> Result<Arc<dyn SecretBackend>, SecretsError> {
    open_backend_from(id, || Ok(Arc::new(KeychainBackend::new())), open_file)
}

/// [`open_backend`] with the two openers injected.
///
/// Why: #9326 — a Keychain failure must surface as itself and never fall
/// through to the file backend. Injecting the openers lets a test hand in a
/// failing Keychain and prove the file opener is never called.
/// What: each id calls only its own opener and returns its result as-is.
/// Test: `store_keychain_failure_never_falls_through_to_file`.
pub(crate) fn open_backend_from(
    id: &BackendId,
    keychain: impl FnOnce() -> Result<Arc<dyn SecretBackend>, SecretsError>,
    file: impl FnOnce() -> Result<Arc<dyn SecretBackend>, SecretsError>,
) -> Result<Arc<dyn SecretBackend>, SecretsError> {
    match id.as_str() {
        // #9326: never `keychain().or_else(|_| file())` — no silent downgrade.
        BackendId::KEYCHAIN => keychain(),
        BackendId::FILE => file(),
        _ => Err(SecretsError::UnknownBackend {
            backend: id.to_string(),
        }),
    }
}

/// The file backend at `~/.trusty-tools/trusty-secrets/values/`.
#[cfg(unix)]
fn open_file() -> Result<Arc<dyn SecretBackend>, SecretsError> {
    Ok(Arc::new(super::FileBackend::default_location()?))
}

/// No file backend off Unix: its mode and owner checks need Unix metadata.
#[cfg(not(unix))]
fn open_file() -> Result<Arc<dyn SecretBackend>, SecretsError> {
    Err(SecretsError::UnknownBackend {
        backend: BackendId::FILE.to_string(),
    })
}
