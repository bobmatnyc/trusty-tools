//! Time-bounded, single-flight credential-store reads (#8236, #7965, #8335).
//!
//! Why: [`super::KeyringStore::get`] is a synchronous `keyring` call, and under
//! launchd the FIRST read by a freshly-built binary raises a blocking macOS
//! SecurityAgent dialog. A measured experiment (keyring 3.6.3, ad-hoc signed
//! binary, launchd one-shot agent, no terminal) took 12 s when a human clicked
//! and never returned when nobody did; every later read by the same cdhash took
//! 10–20 ms. A daemon that calls that on its async path stalls a runtime thread
//! on a human. This module makes the CALLER time out while the read keeps
//! waiting, so a later approval is not discarded.
//!
//! What: three mechanisms, in the order a call meets them.
//!
//! 1. **Negative cache** ([`STORE_ERROR_CACHE_TTL`]). A recent failure for a key
//!    answers immediately from [`ERROR_CACHE`] without touching the store, so a
//!    wedged dialog is not re-raised on every supervisor tick. It holds an error
//!    KIND, never a value, and it EXPIRES — an operator who approves the dialog
//!    is picked up on the next resolve, with no daemon restart.
//! 2. **Single flight** ([`INFLIGHT`]). At most one store read per provider key
//!    is ever outstanding. Concurrent callers subscribe to the same flight, so N
//!    callers raise ONE dialog and park ONE thread, not N of each.
//! 3. **Caller-side timeout** ([`STORE_READ_TIMEOUT`]). The reading thread is
//!    detached and never cancelled — cancelling it is what threw away a
//!    1.8-seconds-late approval in the experiment. The caller stops waiting; the
//!    read does not.
//!
//! Every failure is an [`SecretResolveError`] naming the VARIABLE and the error
//! KIND. Nothing here returns, logs, or formats a credential value, and no arm
//! falls back to a default, a stale value, or a second read — see the
//! fail-closed contract on [`SecretResolveError`].
//!
//! Test: `bounded_store/tests.rs`.

use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use super::{KeyStore, KeyStoreError};
use crate::credential_registry::{env_var_for, provider_for_env_var};

/// How long a caller waits for the credential store before giving up.
///
/// Why: the daemon must never wait on a human. Three seconds is far above the
/// 10–20 ms an approved Keychain read costs and far below the 12 s a human took
/// to click the dialog, so it separates "the store is slow" from "a dialog is
/// on screen" without ever holding the runtime.
/// Test: `a_store_that_never_returns_times_out_within_the_bound`.
pub const STORE_READ_TIMEOUT: Duration = Duration::from_secs(3);

/// How long a store failure suppresses a fresh read of the same key.
///
/// Why: without it, a wedged Keychain dialog is re-raised on every supervisor
/// tick. Why it expires: an operator who approves the dialog a minute later must
/// be picked up without restarting the daemon. While it is live the feature
/// stays OFF — this suppresses the READ, never the failure.
/// Test: `a_cached_error_is_returned_without_a_second_read`,
/// `the_error_cache_expires_and_the_next_read_is_issued`.
pub const STORE_ERROR_CACHE_TTL: Duration = Duration::from_secs(45);

/// What went wrong in the store, as a kind a log line may carry.
///
/// Why: item 7 of #8236 — every error arm logs the key NAME and the error KIND.
/// A kind is an enum precisely so a caller branches on it rather than on
/// message text, and so no arm can smuggle a value into a log.
/// Test: `error_kinds_render_without_any_value`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreErrorKind {
    /// The caller's bound elapsed. The read may still be outstanding.
    Timeout,
    /// The OS keychain backend refused: locked, denied, or unsupported.
    Keyring,
    /// The file-backed store could not be read or written.
    Io,
    /// The store's TOML could not be parsed.
    Toml,
    /// No home directory, so no store location could be resolved.
    HomeUnavailable,
}

impl StoreErrorKind {
    /// Stable, value-free label for a log line.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Timeout => "timeout",
            Self::Keyring => "keyring-backend",
            Self::Io => "io",
            Self::Toml => "toml",
            Self::HomeUnavailable => "home-unavailable",
        }
    }
}

impl std::fmt::Display for StoreErrorKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl From<&KeyStoreError> for StoreErrorKind {
    fn from(e: &KeyStoreError) -> Self {
        match e {
            KeyStoreError::Io { .. } => Self::Io,
            KeyStoreError::Toml { .. } => Self::Toml,
            KeyStoreError::HomeUnavailable => Self::HomeUnavailable,
            KeyStoreError::Keyring(_) => Self::Keyring,
        }
    }
}

/// Why a credential could not be resolved.
///
/// Why: `Option<String>` collapsed "not configured", "the keychain refused" and
/// "a dialog is waiting on screen" into one `None`, and a caller that cannot
/// tell them apart cannot tell an operator what to do. #8236 item 7 additionally
/// requires that NO arm re-enables the dependent feature: every variant here is
/// terminal for the call, and no constructor of one carries a value.
/// What: four variants, each naming the environment VARIABLE and, where there is
/// one, the store error KIND. `cached` says the answer came from
/// [`STORE_ERROR_CACHE_TTL`]'s suppression window rather than from a fresh read.
/// Test: `error_kinds_render_without_any_value`,
/// `an_unregistered_variable_is_not_resolvable`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum SecretResolveError {
    /// The name is not in [`crate::credential_registry::REGISTRY`], so there is
    /// no provider to resolve it under.
    #[error(
        "`{var}` is not a registered credential name — register it in \
         credential_registry::REGISTRY before it can be resolved"
    )]
    Unregistered {
        /// The environment-variable name asked for.
        var: String,
    },

    /// Nothing holds a value for this key: not the process env, not
    /// `.env.local`, not the store.
    #[error(
        "no value configured for `{var}` in the environment, `.env.local`, or the credential store"
    )]
    Absent {
        /// The canonical environment-variable name.
        var: String,
    },

    /// The caller's bound elapsed. The store read may still be outstanding, and
    /// a later approval will be picked up by a subsequent resolve.
    #[error(
        "reading `{var}` from the credential store timed out after {waited_ms} ms \
         (a Keychain approval dialog may be waiting on screen)"
    )]
    Timeout {
        /// The canonical environment-variable name.
        var: String,
        /// The bound that elapsed.
        waited_ms: u64,
        /// True when answered from the error cache rather than a fresh read.
        cached: bool,
    },

    /// The store answered with a failure.
    #[error("the credential store could not supply `{var}`: {kind}")]
    Store {
        /// The canonical environment-variable name.
        var: String,
        /// What class of failure it was. Never a value, never a backend message.
        kind: StoreErrorKind,
        /// True when answered from the error cache rather than a fresh read.
        cached: bool,
    },
}

impl SecretResolveError {
    /// Value-free kind label, for a log line or a doctor row.
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Unregistered { .. } => "unregistered",
            Self::Absent { .. } => "absent",
            Self::Timeout { .. } => StoreErrorKind::Timeout.as_str(),
            Self::Store { kind, .. } => kind.as_str(),
        }
    }

    /// The environment variable this failure concerns.
    #[must_use]
    pub fn var(&self) -> &str {
        match self {
            Self::Unregistered { var }
            | Self::Absent { var }
            | Self::Timeout { var, .. }
            | Self::Store { var, .. } => var,
        }
    }
}

/// One outstanding store read, shared by every caller waiting on it.
///
/// Why: see mechanism 2 in the module docs. The `Condvar` is what lets a caller
/// stop waiting without cancelling the read.
struct Flight {
    /// `None` until the detached reader finishes.
    outcome: Mutex<Option<Result<Option<String>, StoreErrorKind>>>,
    /// Signalled once when the outcome lands.
    done: Condvar,
}

/// Provider key → the read currently outstanding for it.
static INFLIGHT: OnceLock<Mutex<HashMap<String, Arc<Flight>>>> = OnceLock::new();

/// Provider key → (when it failed, how it failed). See [`STORE_ERROR_CACHE_TTL`].
static ERROR_CACHE: OnceLock<Mutex<HashMap<String, (Instant, StoreErrorKind)>>> = OnceLock::new();

/// Lock a `OnceLock<Mutex<_>>` map, recovering a poisoned lock.
///
/// Why: a panic in one caller must not permanently disable credential
/// resolution for the process. The maps hold no invariant a panic can corrupt —
/// an entry is either present or not — so the poisoned guard is safe to take.
fn map<V: 'static>(
    cell: &'static OnceLock<Mutex<HashMap<String, V>>>,
) -> &'static Mutex<HashMap<String, V>> {
    cell.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Read `provider` from `store`, bounded, single-flight, negative-cached.
///
/// Why: the one entry point every daemon-reachable store access goes through.
/// See the module docs for the three mechanisms and why the reading thread is
/// never cancelled.
/// What: returns `Ok(None)` when the store answered and holds nothing, `Ok(Some)`
/// with the value, or the failing [`StoreErrorKind`]. A failure is recorded in
/// the negative cache; a success is not cached at all.
///
/// # Errors
///
/// [`StoreErrorKind::Timeout`] when `timeout` elapses first, otherwise the kind
/// the backend reported.
///
/// Test: `a_store_that_never_returns_times_out_within_the_bound`,
/// `concurrent_resolves_issue_exactly_one_store_read`,
/// `a_cached_error_is_returned_without_a_second_read`,
/// `the_error_cache_expires_and_the_next_read_is_issued`.
pub fn store_get_bounded(
    store: Arc<dyn KeyStore>,
    provider: &str,
    timeout: Duration,
) -> Result<Option<String>, StoreErrorKind> {
    if let Some(kind) = cached_error(provider) {
        return Err(kind);
    }

    let (flight, is_leader) = join_or_start(provider);
    if is_leader {
        spawn_reader(store, provider.to_string(), Arc::clone(&flight));
    }

    let mut guard = flight
        .outcome
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let deadline = Instant::now() + timeout;
    while guard.is_none() {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            // #8236: the caller stops waiting; the reader keeps going, so an
            // approval that lands late still grants the ACL for the NEXT read.
            drop(guard);
            record_error(provider, StoreErrorKind::Timeout);
            return Err(StoreErrorKind::Timeout);
        }
        let (next, _) = flight
            .done
            .wait_timeout(guard, remaining)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard = next;
    }

    match guard.clone() {
        Some(Ok(value)) => Ok(value),
        Some(Err(kind)) => Err(kind),
        // Unreachable: the loop above exits only once the outcome is present.
        None => Err(StoreErrorKind::Timeout),
    }
}

/// The still-live cached error for `provider`, if any.
fn cached_error(provider: &str) -> Option<StoreErrorKind> {
    let mut cache = map(&ERROR_CACHE)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match cache.get(provider) {
        Some((at, kind)) if at.elapsed() < STORE_ERROR_CACHE_TTL => Some(*kind),
        Some(_) => {
            cache.remove(provider);
            None
        }
        None => None,
    }
}

/// Record a failure so the next tick does not re-raise the same dialog.
fn record_error(provider: &str, kind: StoreErrorKind) {
    map(&ERROR_CACHE)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(provider.to_string(), (Instant::now(), kind));
}

/// Join the outstanding flight for `provider`, or become its leader.
fn join_or_start(provider: &str) -> (Arc<Flight>, bool) {
    let mut inflight = map(&INFLIGHT)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(existing) = inflight.get(provider) {
        return (Arc::clone(existing), false);
    }
    let flight = Arc::new(Flight {
        outcome: Mutex::new(None),
        done: Condvar::new(),
    });
    inflight.insert(provider.to_string(), Arc::clone(&flight));
    (flight, true)
}

/// Run the blocking store read on a detached thread.
///
/// Why: `keyring::Entry::get_password` can block indefinitely on a
/// SecurityAgent dialog. Detached, never joined, never cancelled — see the
/// module docs.
fn spawn_reader(store: Arc<dyn KeyStore>, provider: String, flight: Arc<Flight>) {
    let spawned = std::thread::Builder::new()
        .name("cred-store-read".to_string())
        .spawn(move || {
            let outcome = store
                .try_get(&provider)
                .map_err(|e| StoreErrorKind::from(&e));
            finish(&provider, &flight, outcome);
        });
    if spawned.is_err() {
        // #8236: a thread we could not spawn is a failure, never a fallthrough
        // to an unbounded read on this thread.
        finish(&provider, &flight, Err(StoreErrorKind::Io));
    }
}

/// Publish an outcome, wake every waiter, and clear the flight.
fn finish(provider: &str, flight: &Flight, outcome: Result<Option<String>, StoreErrorKind>) {
    if let Err(kind) = &outcome {
        record_error(provider, *kind);
    }
    map(&INFLIGHT)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(provider);
    let mut slot = flight
        .outcome
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    *slot = Some(outcome);
    drop(slot);
    flight.done.notify_all();
}

/// Resolve `var`'s credential: process env, `.env.local`, then the bounded store.
///
/// Why: the shipped precedence ([`super::resolve_key`]) with the store tier made
/// safe for a daemon. Consumers migrated off ad hoc `.env.local` → `.env` →
/// `std::env::var` readers by #8236 call this.
/// What: maps `var` through [`provider_for_env_var`], then applies
/// [`super::resolve_key`]'s env tier (which already reflects `.env.local`, since
/// `dotenvy` never overrides a set variable), then [`store_get_bounded`] against
/// [`super::default_store`] under [`STORE_READ_TIMEOUT`].
///
/// # Errors
///
/// Every failure is terminal for the call and leaves the dependent feature
/// disabled: [`SecretResolveError::Unregistered`], `Absent`, `Timeout`, `Store`.
/// No arm falls back to a default or a cached value.
///
/// Test: `an_unregistered_variable_is_not_resolvable`,
/// `the_env_tier_answers_without_touching_the_store`.
pub fn resolve_env_var_bounded(var: &str) -> Result<String, SecretResolveError> {
    let Some(provider) = provider_for_env_var(var) else {
        return Err(SecretResolveError::Unregistered {
            var: var.to_string(),
        });
    };
    super::dotenv::load_env_local_once();
    resolve_provider_bounded_with(
        provider,
        Arc::from(super::default_store()),
        STORE_READ_TIMEOUT,
    )
}

/// Hermetic core of [`resolve_env_var_bounded`]: env tier, then `store`.
///
/// Why: separated so a test injects a store — including one that never returns —
/// without an OS keychain, a real `$HOME`, or a dialog.
/// What: the non-empty `std::env::var` tier for `provider`'s canonical variable,
/// then [`store_get_bounded`].
///
/// # Errors
///
/// As [`resolve_env_var_bounded`].
///
/// Test: `the_env_tier_answers_without_touching_the_store`,
/// `a_store_that_never_returns_times_out_within_the_bound`,
/// `an_absent_value_is_absent_not_an_error`.
pub fn resolve_provider_bounded_with(
    provider: &str,
    store: Arc<dyn KeyStore>,
    timeout: Duration,
) -> Result<String, SecretResolveError> {
    let var = env_var_for(provider)
        .map(str::to_string)
        .unwrap_or_else(|| provider.to_string());
    if let Ok(value) = std::env::var(&var)
        && !value.is_empty()
    {
        return Ok(value);
    }
    match store_get_bounded(store, provider, timeout) {
        Ok(Some(value)) if !value.is_empty() => Ok(value),
        Ok(_) => Err(SecretResolveError::Absent { var }),
        Err(StoreErrorKind::Timeout) => Err(SecretResolveError::Timeout {
            var,
            waited_ms: u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX),
            cached: false,
        }),
        Err(kind) => Err(SecretResolveError::Store {
            var,
            kind,
            cached: false,
        }),
    }
}

#[cfg(test)]
#[path = "bounded_store/tests.rs"]
mod tests;
