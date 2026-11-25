//! `trusty-browser migrate-keychain --from-stdin`: take over the wrapping key from Python.
//!
//! Why: the Keychain item holding the AES wrapping key was created by the uv
//! Python, and its ACL is pinned to that interpreter's code hash (Keychain
//! spike 828b63cadb, plan section 6 item 2). The Rust binary reading it would
//! prompt, and a prompt under launchd hangs the service. The spike's
//! recommendation is a handoff: the trusted Python exports the key and pipes
//! it in, so the Rust binary never reads the Python-owned item, then the Rust
//! binary re-creates the item as its own creator.
//! What: [`migrate`] reads one base64 key (32 bytes) from the given reader —
//! the CLI passes stdin only, never argv or a file — proves it is the right
//! key by unsealing every pairing in `registry.sqlite3` with it, then under
//! `keychain.lock` deletes the old item, writes its own and reads it back.
//! Nothing is printed or returned that contains the key.
//! Test: `migrate_keychain_verifies_then_replaces_the_item`,
//! `migrate_keychain_refuses_a_wrong_key_without_touching_the_store`,
//! `migrate_keychain_reports_a_lost_write` in `tests/keychain_migrate.rs`.

use std::fs;
use std::io::{self, Read};
use std::os::fd::AsRawFd;
use std::path::Path;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use sha2::{Digest, Sha256};

use crate::registry::Registry;
use crate::vault::{KeyStore, MemoryKeyStore, Vault, VaultError};

/// The longest accepted input (a 32-byte key is 44 base64 characters).
const MAX_INPUT: u64 = 4096;

/// Why the migration stopped.
#[derive(Debug, thiserror::Error)]
pub enum MigrateError {
    /// The input is not base64 of 32 bytes.
    #[error("stdin must hold the base64 Trusty Browser key (32 bytes) on one line")]
    BadKey,
    /// No `registry.sqlite3`, or no pairing to verify the key against.
    #[error("no pairing in {0} to verify the key against; nothing was changed")]
    NothingToVerify(String),
    /// A pairing did not open with the key.
    #[error("the key does not open the pairing of profile {0}; nothing was changed")]
    WrongKey(String),
    /// The old item is gone and the new one was not written.
    #[error(
        "the old Keychain item was deleted but the new one could not be written ({0}); re-run with the same key"
    )]
    WriteLost(String),
    /// The item read back differs from the key written.
    #[error("the Keychain item read back does not match the key written")]
    ReadBack,
    /// Store, registry or I/O failure before any change.
    #[error("{0}")]
    Other(String),
}

impl From<VaultError> for MigrateError {
    fn from(e: VaultError) -> MigrateError {
        MigrateError::Other(e.to_string())
    }
}

impl From<io::Error> for MigrateError {
    fn from(e: io::Error) -> MigrateError {
        MigrateError::Other(e.to_string())
    }
}

/// What a successful migration did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    /// Pairings the key opened.
    pub pairings: usize,
    /// Whether an old item existed and was deleted.
    pub replaced: bool,
    /// The first 12 hex characters of sha256(key), for matching runs in a log.
    pub fingerprint: String,
}

/// Read the key: one line of base64 decoding to 32 bytes.
fn read_key(input: &mut dyn Read) -> Result<String, MigrateError> {
    let mut raw = String::new();
    input
        .take(MAX_INPUT + 1)
        .read_to_string(&mut raw)
        .map_err(|_| MigrateError::BadKey)?;
    let key = raw
        .strip_suffix("\r\n")
        .or_else(|| raw.strip_suffix('\n'))
        .unwrap_or(&raw);
    if raw.len() as u64 > MAX_INPUT || key.contains(['\n', '\r']) {
        return Err(MigrateError::BadKey);
    }
    match STANDARD.decode(key.as_bytes()) {
        Ok(bytes) if bytes.len() == 32 => Ok(key.to_owned()),
        _ => Err(MigrateError::BadKey),
    }
}

/// Unseal every pairing with `key`; the count when all open.
fn verify(state: &Path, service: &str, key: &str) -> Result<usize, MigrateError> {
    let shown = state.display().to_string();
    if !state.join("registry.sqlite3").is_file() {
        return Err(MigrateError::NothingToVerify(shown));
    }
    let registry = Registry::open(state).map_err(|e| MigrateError::Other(e.to_string()))?;
    let pairings = registry
        .pairings()
        .map_err(|e| MigrateError::Other(e.to_string()))?;
    if pairings.is_empty() {
        return Err(MigrateError::NothingToVerify(shown));
    }
    let probe = MemoryKeyStore::new();
    let account = fs::canonicalize(state)?.to_string_lossy().into_owned();
    probe.set(service, &account, key)?;
    let vault = Vault::with_service(state, service, Box::new(probe))?;
    for (profile, _) in &pairings {
        let row = registry
            .pairing(profile)
            .map_err(|e| MigrateError::Other(e.to_string()))?
            .ok_or_else(|| MigrateError::WrongKey(profile.clone()))?;
        vault
            .unseal(&format!("browser:{profile}"), &row.browser_key)
            .map_err(|_| MigrateError::WrongKey(profile.clone()))?;
    }
    Ok(pairings.len())
}

/// Verify `input`'s key against `state`'s pairings, then make `store` hold it.
///
/// Why: see the module doc; a wrong key must never replace a working one.
/// What: every check runs before the first store call; then, holding
/// `state/keychain.lock` (the lock [`Vault`] takes), delete, set, and get.
/// Preconditions: `state` is a private directory with `registry.sqlite3`.
/// Postcondition: on `Ok`, `store.get(service, canonical state)` equals the
/// key and that key opens every pairing.
pub fn migrate(
    state: &Path,
    store: &dyn KeyStore,
    service: &str,
    input: &mut dyn Read,
) -> Result<Report, MigrateError> {
    let key = read_key(input)?;
    let state = fs::canonicalize(state)?;
    if !crate::private::private_dir(&fs::symlink_metadata(&state)?) {
        return Err(VaultError::NotPrivate.into());
    }
    let pairings = verify(&state, service, &key)?;
    let account = state.to_string_lossy().into_owned();
    let lock = crate::private::open_nofollow(&state.join("keychain.lock"), false)?;
    // SAFETY: flock on an fd we own; released when `lock` drops.
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) } != 0 {
        return Err(io::Error::last_os_error().into());
    }
    let replaced = store.delete(service, &account)?;
    store
        .set(service, &account, &key)
        .map_err(|e| MigrateError::WriteLost(e.to_string()))?;
    if store.get(service, &account)?.as_deref() != Some(key.as_str()) {
        return Err(MigrateError::ReadBack);
    }
    let digest = Sha256::digest(key.as_bytes());
    Ok(Report {
        pairings,
        replaced,
        fingerprint: hex::encode(digest)[..12].to_owned(),
    })
}
