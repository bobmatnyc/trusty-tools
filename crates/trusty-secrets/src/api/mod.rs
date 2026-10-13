//! The `api` feature: names, references, values, method types, and errors.
//!
//! Why: callers that only name keys — trusty-console's bridge — need the
//! vocabulary without linking any store code (DOC-74 §15.2).
//! What: re-exports of the submodules below, including [`mask_secret`] and
//! the console bridge's [`BridgeAssertion`] (#9066). Nothing here touches a
//! backend or the filesystem.
//! Test: `api_tests.rs` beside this file.

// #9066: S3a, the console bridge's assertion and method names.
pub mod bridge;
mod error;
// #9066: here, not in `store`, so an `api`-only caller can mask a value.
mod mask;
pub mod methods;
mod names;
mod reference;
mod value;

pub use bridge::{BRIDGE_FIELD, BackendChoice, BackendsResponse, BridgeAssertion, BridgeSource};
pub use error::SecretsError;
pub use mask::{MASK_HEAD_CHARS, mask_secret};
// #9071: one name for tm and every tier-3 client.
pub use methods::TOKEN_FD_ENV;
pub use names::{
    BackendId, MAX_KEY_LEN, MAX_SEGMENT_LEN, OwnerName, RepoName, SecretKey, VAULT_PREFIX,
    VaultName,
};
pub use reference::{SCHEME, SecretRef};
pub use value::SecretValue;

#[cfg(test)]
#[path = "api_tests.rs"]
mod tests;
