//! Native interactive OAuth (authorization-code + PKCE) consent flow.
//!
//! Why: Issue #2631 — the crate must be able to MINT its own
//! `~/.gworkspace-mcp/tokens.json` so onboarding no longer depends on the
//! Python CLI. This module mirrors the Python `auth/oauth_manager.py`
//! interactive flow while writing the identical on-disk token schema.
//! What: `pkce` holds the offline crypto/encoding primitives, `callback`
//! runs the loopback redirect receiver, and `flow` orchestrates the end-to-end
//! consent → token-exchange → persist sequence. `client_store` (issue #3518)
//! adds PER-PROFILE OAuth client persistence/resolution so a profile can
//! authorize (and later refresh) against its own client instead of the
//! shared global one — `flow::run_consent_with` and `manager::OAuthManager`
//! both resolve through it.
//! Test: `pkce`, `callback`, and the pure helpers in `flow` are unit-tested;
//! the live browser round-trip is deferred (needs real Google creds).
//! `client_store`'s file-resolution logic is fully unit-tested (isolated
//! `HOME`); `manager`'s wiremock tests cover the end-to-end refresh choice.

pub mod callback;
pub mod client_store;
pub mod errors;
pub mod flow;
pub mod pkce;

pub use client_store::{
    ClientSource, persist_profile_client, profile_client_path, profile_client_source,
    resolve_client_creds_for_profile,
};
pub use flow::{
    ClientCreds, ConsentOutcome, DefaultMode, resolve_client_creds, run_consent, run_consent_with,
};
