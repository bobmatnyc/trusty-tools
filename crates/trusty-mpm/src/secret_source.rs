//! The daemon's one credential read (#8236 item 5).
//!
//! Why: three readers — `daemon::llm_overseer::resolve_api_key`,
//! `telegram::resolve_token` and `core::sm::providers::resolve` — each walked
//! `.env.local`, then `.env`, then `std::env::var` by hand. None of them could
//! reach the `0600` file store or the Keychain, so the only way to configure
//! the daemon was a plaintext file, and the plaintext LaunchAgent plist of
//! #8236 was the natural end of that. They also could not tell "not
//! configured" from "the store refused", so every failure looked like the
//! former.
//!
//! What: [`resolve_secret`] routes them all through
//! [`trusty_common::credentials::resolve_env_var_bounded`] — process env, then
//! `.env.local`, then the bounded credential store — and logs every failure at
//! ERROR with the variable NAME and the error KIND.
//!
//! **Precedence changed, deliberately.** The old order put `.env.local` and
//! `.env` AHEAD of the process environment, so a stale committed `.env` beat an
//! explicit `export`. The shipped resolver puts the process environment first.
//! The one tier the resolver does not cover is `.env` itself, and it is not
//! reinstated here: `.env` is a committed, non-gitignored file, and a
//! credential in one is the same defect as a credential in a plist. An operator
//! relying on `.env` moves the value to `.env.local` or the store; `tm doctor`'s
//! `credential_reach` row says which credentials are unreachable.
//!
//! **Fail-closed, every arm.** A failure returns `None` and the caller leaves
//! the dependent feature DISABLED. No arm retries, falls back to a default,
//! reuses a cached value, or re-reads the old `.env` path — see
//! `secret_source_tests.rs`, which pins one test per arm per reader.
//!
//! Test: `secret_source_tests.rs`.

#[cfg(test)]
use std::sync::Arc;
#[cfg(test)]
use std::time::Duration;

use trusty_common::credential_registry::is_registered_credential_env_var;
#[cfg(test)]
use trusty_common::credential_registry::provider_for_env_var;
#[cfg(test)]
use trusty_common::credentials::{KeyStore, resolve_provider_bounded_with};
use trusty_common::credentials::{
    SecretResolveError, load_env_local_once, resolve_env_var_bounded,
};

/// Resolve `var` through the shipped resolver, logging any failure by kind.
///
/// Why: one place where the daemon reads a credential, so the fail-closed
/// contract is one function to audit rather than three.
/// What: `Some(value)` when the process environment, `.env.local`, or the
/// bounded credential store holds a non-empty value. `None` on EVERY failure,
/// after an ERROR log naming the variable and the error kind — never the value.
///
/// An UNREGISTERED name — `[llm] api_key_env` lets an operator name any
/// variable — has no provider and therefore no store tier, so it is read from
/// the process environment after `.env.local` has been loaded into it. That is
/// strictly the env tier of the same precedence, not a second path: no `.env`,
/// no file, no default.
/// Test: `an_absent_secret_is_none_and_logs_absent`,
/// `a_store_timeout_is_none_and_logs_timeout`,
/// `a_store_error_is_none_and_logs_the_kind`,
/// `an_unregistered_variable_reads_only_the_process_environment`.
#[must_use]
pub fn resolve_secret(var: &str) -> Option<String> {
    if !is_registered_credential_env_var(var) {
        load_env_local_once();
        return process_env_only(var);
    }
    report(resolve_env_var_bounded(var))
}

/// Hermetic core of [`resolve_secret`]: the same arms against an injected store.
///
/// Why: every failure arm of [`resolve_secret`] runs through the credential
/// store, and a test that reached the real one would need an OS keychain, a
/// real `$HOME`, and — for the timeout arm — a human at a dialog. Injecting the
/// store is the only way to prove each arm returns `None` rather than a default.
/// What: identical to [`resolve_secret`] except that the store tier and its
/// bound are the caller's, and `.env.local` is not loaded (a test must not have
/// the host's own dotenv decide its outcome).
/// Test: `an_absent_secret_is_none_and_logs_absent`,
/// `a_store_timeout_is_none_and_logs_timeout`,
/// `a_store_error_is_none_and_logs_the_kind`,
/// `an_unregistered_variable_reads_only_the_process_environment`.
#[cfg(test)]
#[must_use]
fn resolve_secret_with(var: &str, store: Arc<dyn KeyStore>, timeout: Duration) -> Option<String> {
    let Some(provider) = provider_for_env_var(var) else {
        return process_env_only(var);
    };
    report(resolve_provider_bounded_with(provider, store, timeout))
}

/// The process-environment tier, on its own.
///
/// Why: an UNREGISTERED name has no provider and therefore no store tier, so
/// this IS its whole resolution — never a `.env` read, never a default.
fn process_env_only(var: &str) -> Option<String> {
    match std::env::var(var) {
        Ok(value) if !value.is_empty() => Some(value),
        _ => {
            log_failure(&SecretResolveError::Absent {
                var: var.to_string(),
            });
            None
        }
    }
}

/// Turn a resolution outcome into the fail-closed `Option` callers see.
///
/// Why: one place where a failure becomes `None`, so no arm can grow a fallback
/// without this function changing.
fn report(outcome: Result<String, SecretResolveError>) -> Option<String> {
    match outcome {
        Ok(value) => Some(value),
        Err(e) => {
            log_failure(&e);
            None
        }
    }
}

/// Log one resolution failure at ERROR, by name and kind.
///
/// Why (#8236 item 7): a credential that silently fails to resolve looks
/// exactly like a feature the operator turned off. The log line is what makes
/// a Keychain refusal after a reinstall diagnosable.
/// What: `tracing::error!` with the VARIABLE and the KIND as fields. The error's
/// own `Display` carries neither a value nor a backend message — see
/// `trusty_common::credentials::SecretResolveError`.
/// Test: `a_store_timeout_is_none_and_logs_timeout`.
fn log_failure(e: &SecretResolveError) {
    tracing::error!(
        credential = e.var(),
        kind = e.kind(),
        "credential unavailable — the dependent feature stays DISABLED; no fallback is applied: {e}"
    );
}

#[cfg(test)]
#[path = "secret_source_tests.rs"]
mod tests;
