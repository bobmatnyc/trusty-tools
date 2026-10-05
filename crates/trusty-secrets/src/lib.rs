//! Project- and owner-scoped secret storage for the trusty-* tools.
//!
//! Why: DOC-74 §15 moves secret entry out of agent sessions and into a
//! service the console and `tm` both call. That service needs one crate that
//! owns the vocabulary (keys, scopes, `secret://` references), the store
//! abstraction, and the masking rules, with no dependency on any binary crate.
//!
//! What: two features (DOC-74 §15.2).
//! - `api` — validated names ([`SecretKey`], [`VaultName`], …), the
//!   [`SecretRef`] grammar, the redacting [`SecretValue`], the `secrets.*`
//!   method request/response types, and [`SecretsError`].
//! - `store` — the [`store::SecretBackend`] trait and its Keychain
//!   implementation, the names-only [`store::NamesIndex`], scope resolution,
//!   [`store::mask_secret`], and config resolution.
//!
//! No item in this crate prints, logs, or formats a secret value. Errors and
//! `Debug` output carry names and locations only.
//!
//! Test: unit tests beside each module; the real-Keychain round trip is the
//! ignored `keychain_real_roundtrip_store_list_remove` in
//! `tests/keychain_roundtrip.rs`.
//!
//! Governing document: DOC-74 §15.2–§15.6
//! (`docs/specs/DOC-74-secrets-integration.md`).

#[cfg(feature = "api")]
pub mod api;
#[cfg(feature = "store")]
pub mod store;

#[cfg(feature = "api")]
pub use api::{
    BackendId, OwnerName, RepoName, SecretKey, SecretRef, SecretValue, SecretsError, VaultName,
};
