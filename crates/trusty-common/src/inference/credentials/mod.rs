//! Credential resolver + secure `KeyStore` (issue #2401, epic #2400 Wave 1).
//!
//! Why: every inference-adapter consumer needs the same answer to "where is
//! the API key for provider X" — checked in the same order, with the same
//! never-print-the-value discipline. Before this module, each crate either
//! read `std::env::var` directly (no `.env.local` fallback, no secure-store
//! fallback) or embedded its own ad hoc dotenv call. Centralising the trait,
//! the three backends, and the precedence chain here means every future
//! `inference-client` consumer (and, eventually, the migrated
//! trusty-agents / trusty-search dotenvy call sites) gets identical
//! behaviour for free.
//!
//! What: [`KeyStore`] is the storage trait ([`memory_store::MemoryKeyStore`],
//! [`file_store::FileKeyStore`], and — behind the `keyring-store` feature —
//! `keyring_store::KeyringStore`). [`resolver::resolve_key`] applies the
//! 3-tier precedence (process env var via [`resolver::env_var_for`] >
//! `.env.local` via [`dotenv`] > [`resolver::default_store`]).
//! [`redact::redact_secret`] is the one credential-masking implementation,
//! also reused by `memory_core::filter`.
//!
//! Test: `cargo test -p trusty-common --features credentials -- inference::credentials::`
//! and (KeyringStore compile/probe-failure-path only, never a real keychain)
//! `cargo test -p trusty-common --features keyring-store -- inference::credentials::`.

mod dotenv;
mod file_store;
#[cfg(feature = "keyring-store")]
mod keyring_store;
mod memory_store;
mod redact;
mod resolver;

pub use dotenv::{find_workspace_env_local, load_env_from_path, load_env_local_once};
pub use file_store::FileKeyStore;
#[cfg(feature = "keyring-store")]
pub use keyring_store::KeyringStore;
pub use memory_store::MemoryKeyStore;
pub use redact::redact_secret;
pub use resolver::{default_store, env_var_for, resolve_key, resolve_key_with};

use std::path::PathBuf;

/// Errors raised by a [`KeyStore`] backend.
///
/// Why: callers (the future `config` clap module, the resolver's store tier)
/// need to distinguish "no home directory" from a genuine I/O or parse
/// failure so they can log or degrade appropriately rather than panicking.
/// What: one variant per failure class the file-backed and keyring-backed
/// stores can hit. `MemoryKeyStore` never constructs any of these — its
/// operations are infallible.
/// Test: `file_store_tests::*`, `keyring_store_tests::*` (probe-failure path
/// only).
#[derive(Debug, thiserror::Error)]
pub enum KeyStoreError {
    /// Reading or writing the credential file failed (other than a benign
    /// not-found, which `FileKeyStore` treats as an empty store).
    #[error("credential store I/O error at {path}: {source}")]
    Io {
        /// The path the failed operation targeted.
        path: PathBuf,
        /// The underlying I/O error.
        source: std::io::Error,
    },

    /// The credential TOML could not be parsed or serialised.
    #[error("credential store TOML error at {path}: {message}")]
    Toml {
        /// The path being parsed/serialised when the error occurred.
        path: PathBuf,
        /// The `toml` crate's error message.
        message: String,
    },

    /// `dirs::home_dir()` returned `None` (a stripped CI/container env).
    #[error("credential store home directory unavailable")]
    HomeUnavailable,

    /// The OS keychain backend rejected the operation (locked, denied,
    /// unsupported platform, or no `keyring-store` feature support for this
    /// target). Carries the backend's message; never the secret value.
    #[error("keyring backend error: {0}")]
    Keyring(String),
}

/// Storage backend for provider API keys.
///
/// Why: the resolver's store tier (and the future `config` clap `set` /
/// `list` / `unset` verbs) must work identically against an in-memory test
/// double, a `0600` TOML file, or the OS keychain — one trait, three
/// interchangeable implementations, selected at runtime by
/// [`resolver::default_store`].
/// What: `get` returns `None` on any failure (absent key, unreadable store,
/// locked keychain) — callers cannot distinguish "not set" from "backend
/// error" by design, since the resolver only ever needs a fallthrough
/// signal. `set`/`unset` surface [`KeyStoreError`] because a failed *write*
/// is actionable. `list` returns provider **names only** — a `KeyStore`
/// implementation must never return a value from `list`.
/// Test: `memory_store_tests::*`, `file_store_tests::*`.
pub trait KeyStore: Send + Sync {
    /// Look up the stored credential for `provider`. `None` on any miss or
    /// backend failure — never panics, never logs the (absent) value.
    fn get(&self, provider: &str) -> Option<String>;

    /// Store `value` under `provider`, overwriting any existing entry.
    fn set(&self, provider: &str, value: &str) -> Result<(), KeyStoreError>;

    /// Remove `provider`'s entry, if present. Not an error when absent.
    fn unset(&self, provider: &str) -> Result<(), KeyStoreError>;

    /// List every provider **name** currently stored. Never returns values.
    fn list(&self) -> Vec<String>;
}
