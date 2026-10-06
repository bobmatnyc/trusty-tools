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
//!   [`store::mask_secret`], config resolution, and the in-process
//!   `secret://` resolver, env map and `.env` parser `tm secrets exec` uses
//!   ([`store::resolve`], [`store::parse_dotenv`]).
//! - `server` — the `secrets.*` methods on an on-demand Unix socket, the
//!   `trusty-secrets` binary that serves them, and the client helper that
//!   spawns it ([`server`]; Unix only).
//!
//! No item in this crate prints, logs, or formats a secret value. Errors and
//! `Debug` output carry names and locations only.
//!
//! # Compatibility
//!
//! The request, response and config structs and the public enums are
//! `#[non_exhaustive]`. Build a request with its constructor, read a response
//! through serde, and give every `match` on an enum from this crate a
//! wildcard arm.
//!
//! Adding a field, a variant or a provided trait method is a compatible
//! change. While the crate is 0.x, a compatible change ships as a patch
//! release (0.1.0 to 0.1.1), which a `trusty-secrets = "0.1"` dependency
//! picks up. Only a breaking change moves the minor number (0.1 to 0.2).
//!
//! On the wire, every request denies unknown fields, so a server refuses a
//! field newer than itself. A new request field is therefore optional, with a
//! serde default that keeps the old behaviour, and a client must not send it
//! to a server older than the release that added it. A new response field
//! also takes a serde default, so a client still decodes an older server's
//! answer.
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
// #9065: S2 — the on-demand socket. trusty-common's UDS stack is Unix-only.
#[cfg(all(unix, feature = "server"))]
pub mod server;

#[cfg(feature = "api")]
pub use api::{
    BackendId, OwnerName, RepoName, SecretKey, SecretRef, SecretValue, SecretsError, VaultName,
};
