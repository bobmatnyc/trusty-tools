//! A time limit on every call into a synchronous backend (#7524 P2-L7).
//!
//! Why: a Keychain call can block until someone answers an OS access
//! prompt, which may be never. The `keyring` calls take no timeout, and
//! the request deadline (`store::deadline`) only stops vendor CLIs, so a
//! caller waited on the prompt with no bound.
//! What: [`TimeLimited`] wraps a [`SecretBackend`]. Each call runs on a
//! thread of its own; the caller waits at most the limit, or less when the
//! server's request deadline leaves less, and then gets
//! [`SecretsError::Timeout`] naming the operation. A timeout is never a
//! miss, an empty value or a success. With no time left the call is not
//! started at all.
//! Test: `time_limit_tests.rs` beside this file.

use std::sync::Arc;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::Duration;

use super::{Capabilities, SecretBackend};
use crate::api::{BackendId, SecretKey, SecretValue, SecretsError, VaultName};

/// A backend whose every call returns within a time limit.
///
/// Why: see the module docs.
/// What: forwards each [`SecretBackend`] call to the wrapped backend on a
/// new thread and waits for the result up to the limit.
/// Test: `time_limited_call_that_never_returns_times_out_on_every_operation`,
/// `time_limited_call_passes_results_and_errors_through`.
#[derive(Debug, Clone)]
pub(crate) struct TimeLimited {
    inner: Arc<dyn SecretBackend>,
    limit: Duration,
}

impl TimeLimited {
    /// `inner`, with each call bounded by `limit`.
    pub(crate) fn new(inner: Arc<dyn SecretBackend>, limit: Duration) -> Self {
        Self { inner, limit }
    }

    /// Run `call` against the wrapped backend within the time limit.
    ///
    /// What: the limit is the smaller of this wrapper's and the time left
    /// before the request deadline. No time left: [`SecretsError::Timeout`]
    /// before the call starts. The call runs on its own thread; past the
    /// limit the caller gets [`SecretsError::Timeout`], and a call that
    /// panics is a [`SecretsError::Backend`] failure.
    // See #7524: a timed-out call's thread is abandoned, not stopped. It
    // stays blocked until the OS call returns (for the Keychain, until its
    // prompt is answered or dismissed), and a write it carries may still
    // land then. No thread can be killed safely, so this is accepted.
    fn run<T: Send + 'static>(
        &self,
        operation: &'static str,
        vault: &VaultName,
        key: Option<&SecretKey>,
        call: impl FnOnce(&dyn SecretBackend) -> Result<T, SecretsError> + Send + 'static,
    ) -> Result<T, SecretsError> {
        let limit = self.effective_limit();
        if limit.is_zero() {
            return Err(self.timeout(operation, vault, key, limit));
        }
        // Capacity 1: a late result is sent without blocking, then dropped.
        let (tx, rx) = mpsc::sync_channel(1);
        let inner = Arc::clone(&self.inner);
        std::thread::Builder::new()
            .name("trusty-secrets-backend-call".to_string())
            .spawn(move || {
                // The receiver is gone after a timeout; nothing to report.
                let _ = tx.send(call(inner.as_ref()));
            })
            .map_err(|e| self.failure(vault, key, format!("cannot start the call thread: {e}")))?;
        match rx.recv_timeout(limit) {
            Ok(result) => result,
            Err(RecvTimeoutError::Timeout) => Err(self.timeout(operation, vault, key, limit)),
            Err(RecvTimeoutError::Disconnected) => Err(self.failure(
                vault,
                key,
                format!("the {operation} call ended with no result"),
            )),
        }
    }

    /// This wrapper's limit, cut to the request deadline when one is set.
    fn effective_limit(&self) -> Duration {
        #[cfg(any(feature = "server", feature = "cli-backends"))]
        if let Some(left) = super::deadline::remaining() {
            return left.min(self.limit);
        }
        self.limit
    }

    fn timeout(
        &self,
        operation: &'static str,
        vault: &VaultName,
        key: Option<&SecretKey>,
        waited: Duration,
    ) -> SecretsError {
        SecretsError::Timeout {
            backend: self.inner.id().to_string(),
            operation,
            vault: vault.to_string(),
            key: key_text(key),
            waited,
        }
    }

    fn failure(&self, vault: &VaultName, key: Option<&SecretKey>, reason: String) -> SecretsError {
        SecretsError::Backend {
            backend: self.inner.id().to_string(),
            vault: vault.to_string(),
            key: key_text(key),
            reason,
        }
    }
}

/// The key an error names; `(none)` for a vault-wide call.
fn key_text(key: Option<&SecretKey>) -> String {
    key.map_or_else(|| "(none)".to_string(), ToString::to_string)
}

impl SecretBackend for TimeLimited {
    fn id(&self) -> BackendId {
        self.inner.id()
    }

    fn capabilities(&self) -> Capabilities {
        self.inner.capabilities()
    }

    fn get(&self, vault: &VaultName, key: &SecretKey) -> Result<Option<SecretValue>, SecretsError> {
        let (v, k) = (vault.clone(), key.clone());
        self.run("get", vault, Some(key), move |b| b.get(&v, &k))
    }

    fn set(
        &self,
        vault: &VaultName,
        key: &SecretKey,
        value: &SecretValue,
    ) -> Result<(), SecretsError> {
        let (v, k, value) = (vault.clone(), key.clone(), value.clone());
        self.run("set", vault, Some(key), move |b| b.set(&v, &k, &value))
    }

    fn delete(&self, vault: &VaultName, key: &SecretKey) -> Result<bool, SecretsError> {
        let (v, k) = (vault.clone(), key.clone());
        self.run("delete", vault, Some(key), move |b| b.delete(&v, &k))
    }

    fn list_names(&self, vault: &VaultName) -> Result<Vec<SecretKey>, SecretsError> {
        let v = vault.clone();
        self.run("list_names", vault, None, move |b| b.list_names(&v))
    }

    fn agents_may_use(&self, vault: &VaultName, key: &SecretKey) -> Result<bool, SecretsError> {
        let (v, k) = (vault.clone(), key.clone());
        self.run("agents_may_use", vault, Some(key), move |b| {
            b.agents_may_use(&v, &k)
        })
    }

    fn set_agents_may_use(
        &self,
        vault: &VaultName,
        key: &SecretKey,
        allowed: bool,
    ) -> Result<(), SecretsError> {
        let (v, k) = (vault.clone(), key.clone());
        self.run("set_agents_may_use", vault, Some(key), move |b| {
            b.set_agents_may_use(&v, &k, allowed)
        })
    }
}

#[cfg(test)]
#[path = "time_limit_tests.rs"]
mod tests;
