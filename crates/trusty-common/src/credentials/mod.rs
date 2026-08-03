//! Credential resolver + secure `KeyStore` — the storage and naming half of
//! the credential authority (issue #2401; promoted to a top-level module by
//! #4564, DOC-45).
//!
//! Why: every consumer needs the same answer to "where is the secret for
//! provider X" — checked in the same order, with the same
//! never-print-the-value discipline. Before this module, each crate either
//! read `std::env::var` directly (no `.env.local` fallback, no secure-store
//! fallback) or embedded its own ad hoc dotenv call. It lived under
//! `inference::` while its only consumers were LLM providers; by #4564 four of
//! its ten registry entries were Slack/Telegram/`claude-code` tokens and the
//! path had become actively misleading, which is why consumers kept adding a
//! raw `std::env::var` read instead of finding it.
//!
//! What: [`KeyStore`] is the storage trait ([`memory_store::MemoryKeyStore`],
//! [`file_store::FileKeyStore`], and — behind the `keyring-store` feature —
//! `keyring_store::KeyringStore`). [`registry`] is the
//! provider→environment-variable table. [`resolver::resolve_key`] applies the
//! 3-tier precedence (process env var via [`registry::env_var_for`] >
//! `.env.local` via [`dotenv`] > [`resolver::default_store`]).
//! [`redact::redact_secret`] is the one credential-masking implementation,
//! also reused by `memory_core::filter`.
//!
//! This module holds **no** authorization. Which principal may resolve which
//! credential is DOC-45 §5 and lands with #4566; the storage tiers here
//! determine *where a value lives*, never *who may read it* (`C-9.8`).
//!
//! Test: `cargo test -p trusty-common --features credentials -- credentials::`
//! and (KeyringStore compile/probe-failure-path only, never a real keychain)
//! `cargo test -p trusty-common --features keyring-store -- credentials::`.

mod dotenv;
mod file_store;
#[cfg(feature = "keyring-store")]
mod keyring_store;
mod memory_store;
mod redact;
pub mod registry;
mod resolver;

pub use dotenv::{
    env_local_value, find_workspace_env_local, load_env_from_path, load_env_local_once,
    read_var_from_env_local, user_env_local_path,
};
pub use file_store::FileKeyStore;
#[cfg(feature = "keyring-store")]
pub use keyring_store::KeyringStore;
pub use memory_store::MemoryKeyStore;
pub use redact::redact_secret;
pub use registry::{REGISTRY, env_var_for, registered_providers};
pub use resolver::{default_store, resolve_key, resolve_key_with};

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
        /// Sanitized error description: kind + byte offset only. Never
        /// contains file content — the offending line of a credentials
        /// file IS a secret (see `file_store::sanitize_toml_error`).
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
