//! AES-256-GCM sealing of pairing keys, wrapping key in a [`KeyStore`].
//!
//! Why: `registry.sqlite3` holds each browser key sealed by `vault.py`; the
//! Rust broker must open those rows without a re-pair and write rows Python
//! can open (the no-re-pair guarantee, plan section 3 items 9-10).
//! What: sealed = `tb1.` + urlsafe-base64(nonce12 || ciphertext || tag), AAD =
//! the purpose (`browser:<profile>`). The 32-byte key is a base64 generic
//! password, service [`SERVICE`], account = the canonical state path, read
//! under `state/keychain.lock`. The store is a trait so tests never touch a
//! real Keychain; `KeyringStore` is the macOS login-Keychain backend.
//! Test: `fixture_registry_unseals_every_pairing`, `rust_seal_round_trips_and_binds_aad`
//! in `tests/parity_registry.rs`, `throwaway_keychain_round_trip` (opt-in) in
//! `tests/keychain_optin.rs`.

use std::collections::HashMap;
use std::fs;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use aes_gcm::aead::{Aead, Generate, KeyInit, Nonce, Payload};
use aes_gcm::{Aes256Gcm, Key};
use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE};

use crate::private;

/// Sealed-value prefix.
pub const PREFIX: &str = "tb1.";
/// Keychain service of the wrapping key.
pub const SERVICE: &str = "com.trusty.browser.local-credentials";
const NONCE: usize = 12;
const TAG: usize = 16;

/// Why the vault refused.
#[derive(Debug, thiserror::Error)]
pub enum VaultError {
    /// The state directory is not owner-only.
    #[error("Credential directory must be private")]
    NotPrivate,
    /// No key exists and the caller only reads.
    #[error("Trusty Browser Keychain item is missing")]
    Missing,
    /// The key store failed.
    #[error("Cannot access the Trusty Browser login Keychain item: {0}")]
    Store(String),
    /// The stored key is not base64 of 32 bytes.
    #[error("Trusty Browser Keychain item is malformed")]
    BadKey,
    /// Not a `tb1.` value.
    #[error("Invalid sealed credential")]
    Invalid,
    /// Wrong key, wrong purpose, tampered or non-UTF-8 plaintext.
    #[error("Cannot open credential for this workflow or profile")]
    CannotOpen,
    /// Encryption or randomness failed.
    #[error("Cannot seal credential")]
    Seal,
    /// The state path or lock file failed.
    #[error("vault I/O: {0}")]
    Io(#[from] std::io::Error),
}

/// A generic-password store addressed by service and account.
pub trait KeyStore: Send + Sync {
    /// The stored value, `None` when absent.
    fn get(&self, service: &str, account: &str) -> Result<Option<String>, VaultError>;
    /// Create or replace the value.
    fn set(&self, service: &str, account: &str, value: &str) -> Result<(), VaultError>;
    /// Remove the value; `Ok(false)` when there was none.
    fn delete(&self, service: &str, account: &str) -> Result<bool, VaultError>;
}

/// An in-memory store for tests and fixtures.
#[derive(Debug, Default)]
pub struct MemoryKeyStore {
    items: Mutex<HashMap<(String, String), String>>,
}

impl MemoryKeyStore {
    /// An empty store.
    pub fn new() -> MemoryKeyStore {
        MemoryKeyStore::default()
    }
}

impl KeyStore for MemoryKeyStore {
    fn get(&self, service: &str, account: &str) -> Result<Option<String>, VaultError> {
        let items = self
            .items
            .lock()
            .map_err(|_| VaultError::Store("poisoned".into()))?;
        Ok(items
            .get(&(service.to_owned(), account.to_owned()))
            .cloned())
    }
    fn set(&self, service: &str, account: &str, value: &str) -> Result<(), VaultError> {
        let mut items = self
            .items
            .lock()
            .map_err(|_| VaultError::Store("poisoned".into()))?;
        items.insert((service.to_owned(), account.to_owned()), value.to_owned());
        Ok(())
    }
    fn delete(&self, service: &str, account: &str) -> Result<bool, VaultError> {
        let mut items = self
            .items
            .lock()
            .map_err(|_| VaultError::Store("poisoned".into()))?;
        Ok(items
            .remove(&(service.to_owned(), account.to_owned()))
            .is_some())
    }
}

/// The macOS login Keychain through the `keyring` crate.
///
/// Never used by default tests: its backend cannot target a keychain file,
/// so any call reaches the login Keychain (and may raise a dialog).
#[cfg(target_os = "macos")]
#[derive(Debug, Default, Clone, Copy)]
pub struct KeyringStore;

#[cfg(target_os = "macos")]
impl KeyStore for KeyringStore {
    fn get(&self, service: &str, account: &str) -> Result<Option<String>, VaultError> {
        let entry =
            keyring::Entry::new(service, account).map_err(|e| VaultError::Store(e.to_string()))?;
        match entry.get_password() {
            Ok(v) => Ok(Some(v)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(VaultError::Store(e.to_string())),
        }
    }
    fn set(&self, service: &str, account: &str, value: &str) -> Result<(), VaultError> {
        let entry =
            keyring::Entry::new(service, account).map_err(|e| VaultError::Store(e.to_string()))?;
        entry
            .set_password(value)
            .map_err(|e| VaultError::Store(e.to_string()))
    }
    fn delete(&self, service: &str, account: &str) -> Result<bool, VaultError> {
        let entry =
            keyring::Entry::new(service, account).map_err(|e| VaultError::Store(e.to_string()))?;
        match entry.delete_credential() {
            Ok(()) => Ok(true),
            Err(keyring::Error::NoEntry) => Ok(false),
            Err(e) => Err(VaultError::Store(e.to_string())),
        }
    }
}

/// The vault of one state directory.
pub struct Vault {
    state: PathBuf,
    service: String,
    store: Box<dyn KeyStore>,
}

impl std::fmt::Debug for Vault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Vault")
            .field("state", &self.state)
            .field("service", &self.service)
            .finish_non_exhaustive()
    }
}

impl Vault {
    /// The vault for `state` (canonicalised) using [`SERVICE`].
    pub fn new(state: &Path, store: Box<dyn KeyStore>) -> Result<Vault, VaultError> {
        Vault::with_service(state, SERVICE, store)
    }

    /// The vault with another Keychain service name (fixtures, tests).
    pub fn with_service(
        state: &Path,
        service: &str,
        store: Box<dyn KeyStore>,
    ) -> Result<Vault, VaultError> {
        Ok(Vault {
            state: fs::canonicalize(state)?,
            service: service.to_owned(),
            store,
        })
    }

    /// The Keychain account: `str(state.resolve())`.
    pub fn account(&self) -> String {
        self.state.to_string_lossy().into_owned()
    }

    fn key(&self, create: bool) -> Result<Key<Aes256Gcm>, VaultError> {
        if !private::private_dir(&fs::metadata(&self.state)?) {
            return Err(VaultError::NotPrivate);
        }
        let lock = private::open_nofollow(&self.state.join("keychain.lock"), false)?;
        // SAFETY: flock on an fd we own; the lock is released when `lock` drops.
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let account = self.account();
        let value = match self.store.get(&self.service, &account)? {
            Some(v) => v,
            None if create => {
                let key = Key::<Aes256Gcm>::try_generate().map_err(|_| VaultError::Seal)?;
                let value = STANDARD.encode(key.as_slice());
                self.store.set(&self.service, &account, &value)?;
                value
            }
            None => return Err(VaultError::Missing),
        };
        let raw = STANDARD
            .decode(value.as_bytes())
            .map_err(|_| VaultError::BadKey)?;
        Key::<Aes256Gcm>::try_from(raw.as_slice()).map_err(|_| VaultError::BadKey)
    }

    /// Seal `value` for `purpose`, creating the wrapping key when absent.
    pub fn seal(&self, purpose: &str, value: &str) -> Result<String, VaultError> {
        let nonce = Nonce::<Aes256Gcm>::try_generate().map_err(|_| VaultError::Seal)?;
        self.seal_with_nonce(purpose, value, nonce.as_slice())
    }

    /// Seal with a caller-chosen 12-byte nonce (deterministic fixtures only).
    pub fn seal_with_nonce(
        &self,
        purpose: &str,
        value: &str,
        nonce: &[u8],
    ) -> Result<String, VaultError> {
        let nonce = Nonce::<Aes256Gcm>::try_from(nonce).map_err(|_| VaultError::Seal)?;
        let cipher = Aes256Gcm::new(&self.key(true)?);
        let body = cipher
            .encrypt(
                &nonce,
                Payload {
                    msg: value.as_bytes(),
                    aad: purpose.as_bytes(),
                },
            )
            .map_err(|_| VaultError::Seal)?;
        let mut raw = nonce.to_vec();
        raw.extend_from_slice(&body);
        Ok(format!("{PREFIX}{}", URL_SAFE.encode(raw)))
    }

    /// Open a sealed value; the key must already exist.
    ///
    /// Why: reads never create a key (an absent key is an error, `vault.py`).
    /// What: Python decodes with `altchars=b"-_", validate=True`, which maps
    /// `-_` to `+/` and then accepts the standard alphabet; this does the same.
    /// Test: `fixture_registry_unseals_every_pairing`, `rust_seal_round_trips_and_binds_aad`.
    pub fn unseal(&self, purpose: &str, value: &str) -> Result<String, VaultError> {
        let encoded = value.strip_prefix(PREFIX).ok_or(VaultError::Invalid)?;
        let encoded: String = encoded
            .chars()
            .map(|c| match c {
                '-' => '+',
                '_' => '/',
                c => c,
            })
            .collect();
        let raw = STANDARD
            .decode(encoded.as_bytes())
            .map_err(|_| VaultError::CannotOpen)?;
        if raw.len() < NONCE + TAG {
            return Err(VaultError::CannotOpen);
        }
        let (nonce, body) = raw.split_at(NONCE);
        let nonce = Nonce::<Aes256Gcm>::try_from(nonce).map_err(|_| VaultError::CannotOpen)?;
        let cipher = Aes256Gcm::new(&self.key(false)?);
        let plain = cipher
            .decrypt(
                &nonce,
                Payload {
                    msg: body,
                    aad: purpose.as_bytes(),
                },
            )
            .map_err(|_| VaultError::CannotOpen)?;
        String::from_utf8(plain).map_err(|_| VaultError::CannotOpen)
    }
}
