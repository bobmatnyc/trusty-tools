//! OAuth token models, on-disk storage, and refresh manager.
//!
//! Why: Google access tokens live ~1 hour; refresh tokens persist longer but
//! must be cared for. We share the JSON file layout with the Python project
//! so a user who authenticated via the Python CLI can run this Rust port
//! without re-auth.
//! What: `models` (serde-compatible types), `storage` (read/write
//! tokens.json), `manager` (refresh via Google OAuth token endpoint).
//! Test: `models` has unit tests for `is_expired`; storage has an
//! integration test under `tests/auth_models.rs`.

pub mod manager;
pub mod models;
pub mod oauth;
pub mod perms;
pub mod storage;

#[cfg(test)]
pub(crate) mod test_support;

pub use manager::OAuthManager;
pub use models::{OAuthToken, StoredToken, TokenMetadata};
pub use oauth::{ConsentOutcome, run_consent};
pub use storage::TokenStorage;
