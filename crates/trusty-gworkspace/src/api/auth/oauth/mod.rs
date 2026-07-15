//! Native interactive OAuth (authorization-code + PKCE) consent flow.
//!
//! Why: Issue #2631 — the crate must be able to MINT its own
//! `~/.gworkspace-mcp/tokens.json` so onboarding no longer depends on the
//! Python CLI. This module mirrors the Python `auth/oauth_manager.py`
//! interactive flow while writing the identical on-disk token schema.
//! What: `pkce` holds the offline crypto/encoding primitives, `callback`
//! runs the loopback redirect receiver, and `flow` orchestrates the end-to-end
//! consent → token-exchange → persist sequence.
//! Test: `pkce`, `callback`, and the pure helpers in `flow` are unit-tested;
//! the live browser round-trip is deferred (needs real Google creds).

pub mod callback;
pub mod flow;
pub mod pkce;

pub use flow::{ConsentOutcome, DefaultMode, resolve_client_creds, run_consent};
