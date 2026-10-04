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
//! **The one credential door (#9121).** Every daemon read of `.env.local`, the
//! credential store or the Keychain goes through this file: [`resolve_secret`],
//! [`resolve_bounded_gated`] and [`credential_store`], all behind the sandbox
//! latch. The source scan in `secret_source_scan_tests.rs` fails on a direct
//! read anywhere else in the crate.
//!
//! Test: `secret_source_tests.rs`.
//!
//! [`resolve_secret`]: crate::secret_source::resolve_secret
//! [`resolve_bounded_gated`]: crate::secret_source::resolve_bounded_gated
//! [`credential_store`]: crate::secret_source::credential_store

#[cfg(test)]
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(test)]
use std::time::Duration;

use trusty_common::credential_registry::is_registered_credential_env_var;
#[cfg(test)]
use trusty_common::credential_registry::provider_for_env_var;
#[cfg(test)]
use trusty_common::credentials::resolve_provider_bounded_with;
use trusty_common::credentials::{
    KeyStore, MemoryKeyStore, SecretResolveError, default_store, load_env_local_once,
    resolve_env_var_bounded,
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
    // #9121: a sandboxed daemon reads no tier — not `.env.local`, not the store.
    resolve_gated(sandboxed(), var, || {
        if !is_registered_credential_env_var(var) {
            load_env_local_once();
            return process_env_only(var);
        }
        report(resolve_env_var_bounded(var))
    })
}

/// The process-wide sandbox latch (#9121). One-way: nothing clears it.
static SANDBOX: AtomicBool = AtomicBool::new(false);

/// Put this process in sandbox mode: every later [`resolve_secret`] is `None`,
/// [`resolve_bounded_gated`] is `Absent`, and [`credential_store`] is empty.
///
/// Why (#9121): a live-check daemon started with its own `$HOME` still reached
/// the operator's credentials, because the Keychain is per-user, not per-HOME.
/// What: sets the private `SANDBOX` latch. There is no way back; a sandbox
/// that could be left would be one bug away from reading the store.
/// Test: `sandbox_mode_never_consults_the_store`; end to end in its own
/// process, `tests/sandbox_latch_9121.rs`. The call site is
/// `tm daemon --sandbox` (`commands::daemon_sandbox::enter`).
pub fn enter_sandbox() {
    SANDBOX.store(true, Ordering::SeqCst);
}

/// True once [`enter_sandbox`] has run in this process.
#[must_use]
pub fn sandboxed() -> bool {
    SANDBOX.load(Ordering::SeqCst)
}

/// Run `resolve` only outside sandbox mode.
///
/// Why: the gate is a parameter so a test can prove the tiers are skipped
/// without setting the process-wide latch under its parallel siblings.
/// What: `sandbox` → a WARN naming the variable, then `None`; `resolve` is never
/// called. Otherwise `resolve()`.
/// Test: `sandbox_mode_never_consults_the_store`,
/// `outside_sandbox_mode_the_store_is_consulted`.
fn resolve_gated(
    sandbox: bool,
    var: &str,
    resolve: impl FnOnce() -> Option<String>,
) -> Option<String> {
    if sandbox {
        tracing::warn!(
            credential = var,
            "sandbox mode (#9121): no credential tier is consulted — the dependent feature stays DISABLED"
        );
        return None;
    }
    resolve()
}

/// The credential store every daemon inference adapter is built over.
///
/// Why (#9121): `Configurator::build` reads the store it is handed, and the
/// two daemon call sites handed it `default_store()` — the per-user Keychain —
/// past the latch that guards [`resolve_secret`].
/// What: [`credential_store_or`] with `default_store` as the real store.
/// Test: `a_sandbox_store_never_builds_the_real_store`; end to end,
/// `tests/sandbox_latch_9121.rs`.
#[must_use]
pub fn credential_store() -> Box<dyn KeyStore> {
    credential_store_or(default_store)
}

/// [`credential_store`] with the real store's constructor injected.
///
/// Why: a test proves the sandbox never builds the real store by passing a
/// constructor that panics, so a regression fails instead of reading it.
/// What: in sandbox mode, an empty `MemoryKeyStore` and `real` is never
/// called; otherwise `real()`.
/// Test: as [`credential_store`].
#[must_use]
pub fn credential_store_or(real: impl FnOnce() -> Box<dyn KeyStore>) -> Box<dyn KeyStore> {
    store_gated(sandboxed(), real)
}

/// The latch decision behind [`credential_store_or`], with the latch a parameter.
///
/// Test: `a_sandbox_store_never_builds_the_real_store`,
/// `outside_sandbox_mode_the_real_store_is_built`.
fn store_gated(sandbox: bool, real: impl FnOnce() -> Box<dyn KeyStore>) -> Box<dyn KeyStore> {
    if sandbox {
        tracing::info!("sandbox mode (#9121): inference uses an empty credential store");
        return Box::new(MemoryKeyStore::new());
    }
    real()
}

/// The bounded, non-logging resolver behind the latch.
///
/// Why (#9121): the `/activity` key-presence probe read
/// `resolve_env_var_bounded` directly, which walks `.env.local` and the
/// Keychain. It must not log (#8563), so it cannot use [`resolve_secret`].
/// What: in sandbox mode, `Absent` for `var` and no tier is read; otherwise
/// `resolve_env_var_bounded(var)`. Logs nothing on either path.
/// Test: `a_sandboxed_bounded_resolve_is_absent_and_reads_nothing`,
/// `outside_sandbox_mode_the_bounded_resolver_runs`.
pub fn resolve_bounded_gated(var: &str) -> Result<String, SecretResolveError> {
    bounded_gated(sandboxed(), var, resolve_env_var_bounded)
}

/// [`resolve_bounded_gated`] with the latch and the resolver as parameters.
fn bounded_gated(
    sandbox: bool,
    var: &str,
    resolve: impl FnOnce(&str) -> Result<String, SecretResolveError>,
) -> Result<String, SecretResolveError> {
    if sandbox {
        return Err(SecretResolveError::Absent {
            var: var.to_string(),
        });
    }
    resolve(var)
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
pub(crate) fn resolve_secret_with(
    var: &str,
    store: Arc<dyn KeyStore>,
    timeout: Duration,
) -> Option<String> {
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

// #9121: fails CI on a credential read anywhere else in the crate.
#[cfg(test)]
#[path = "secret_source_scan_tests.rs"]
mod scan_tests;

// #9123: fails CI on a credential TEST anywhere in the workspace that is not
// `#[serial]` and sandboxed; reuses `scan_tests`' lexer.
#[cfg(test)]
#[path = "credential_test_scan_tests.rs"]
mod credential_test_scan;

// #9121/#9123: the env guard and redacting assert every credential test uses.
#[cfg(test)]
#[path = "secret_source_test_env.rs"]
pub(crate) mod test_env;
