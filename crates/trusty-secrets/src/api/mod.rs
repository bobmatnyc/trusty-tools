//! The `api` feature: names, references, values, method types, and errors.
//!
//! Why: callers that only name keys — trusty-console's bridge — need the
//! vocabulary without linking any store code (DOC-74 §15.2).
//! What: re-exports of the submodules below. Nothing here touches a backend
//! or the filesystem.
//! Test: `api_tests.rs` beside this file.

mod error;
pub mod methods;
mod names;
mod reference;
mod value;

pub use error::SecretsError;
pub use names::{
    BackendId, MAX_KEY_LEN, MAX_SEGMENT_LEN, OwnerName, RepoName, SecretKey, VAULT_PREFIX,
    VaultName,
};
pub use reference::{SCHEME, SecretRef};
pub use value::SecretValue;

#[cfg(test)]
#[path = "api_tests.rs"]
mod tests;
