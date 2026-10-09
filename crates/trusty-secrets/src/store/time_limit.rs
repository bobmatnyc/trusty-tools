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
//! started at all. While an abandoned call on an item is still running, a
//! later call on that item fails at once with the same error, so the late
//! write can never land after a newer one.
//! Test: `time_limit_tests.rs` beside this file.

use std::collections::HashMap;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use super::{Capabilities, SecretBackend};
use crate::api::{BackendId, SecretKey, SecretValue, SecretsError, VaultName};

/// A backend whose every call returns within a time limit.
///
/// Why: see the module docs.
/// What: forwards each [`SecretBackend`] call to the wrapped backend on a
/// new thread and waits for the result up to the limit.
/// Test: `time_limited_call_that_never_returns_times_out_on_every_operation`,
/// `time_limited_call_passes_results_and_errors_through`,
/// `time_limited_item_with_an_abandoned_call_fails_fast_until_that_call_ends`.
#[derive(Debug, Clone)]
pub(crate) struct TimeLimited {
    inner: Arc<dyn SecretBackend>,
    limit: Duration,
    abandoned: Arc<Abandoned>,
}

/// Which item of a key a call touches: its value, its "agents may use" flag
/// item (#9070), or the whole vault.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Part {
    Value,
    Flag,
    Vault,
}

/// One backend item: for the Keychain, one (service, account) pair.
type Item = (Part, VaultName, Option<SecretKey>);

/// The items with an abandoned call still running, and how many.
#[derive(Debug, Default)]
struct Abandoned(Mutex<HashMap<Item, usize>>);

impl Abandoned {
    fn items(&self) -> MutexGuard<'_, HashMap<Item, usize>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Where one call is. Changed only under its own lock, which is always
/// taken before [`Abandoned`]'s.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Running,
    Abandoned,
    Done,
}

fn lock(phase: &Mutex<Phase>) -> MutexGuard<'_, Phase> {
    phase.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Ends a call on its own thread: marks it done and, if its caller gave up
/// on it, clears its item. Runs on return and on panic alike.
struct Finish {
    phase: Arc<Mutex<Phase>>,
    abandoned: Arc<Abandoned>,
    item: Item,
}

impl Drop for Finish {
    fn drop(&mut self) {
        let mut phase = lock(&self.phase);
        if *phase == Phase::Abandoned {
            let mut items = self.abandoned.items();
            if let Some(count) = items.get_mut(&self.item) {
                *count -= 1;
                if *count == 0 {
                    items.remove(&self.item);
                }
            }
        }
        *phase = Phase::Done;
    }
}

impl TimeLimited {
    /// `inner`, with each call bounded by `limit`.
    pub(crate) fn new(inner: Arc<dyn SecretBackend>, limit: Duration) -> Self {
        Self {
            inner,
            limit,
            abandoned: Arc::default(),
        }
    }

    /// Run `call` against the wrapped backend within the time limit.
    ///
    /// What: the limit is the smaller of this wrapper's and the time left
    /// before the request deadline. No time left: [`SecretsError::Timeout`]
    /// before the call starts. The call runs on its own thread; past the
    /// limit the caller gets [`SecretsError::Timeout`], and a call that
    /// panics is a [`SecretsError::Backend`] failure. While an abandoned
    /// call on the same item is still running, the call is not started and
    /// the caller gets [`SecretsError::Timeout`] at once.
    // See #7524: a timed-out call's thread is abandoned, not stopped. It
    // stays blocked until the OS call returns (for the Keychain, until its
    // prompt is answered or dismissed), and a write it carries may still
    // land then. No thread can be killed safely, so this is accepted. That
    // late write lands after the index lock is released (#9064), so until
    // the thread ends every later call on its item fails fast: the late
    // write cannot replace a newer one, and no call on that item starts
    // another thread that could be abandoned.
    fn run<T: Send + 'static>(
        &self,
        operation: &'static str,
        part: Part,
        vault: &VaultName,
        key: Option<&SecretKey>,
        call: impl FnOnce(&dyn SecretBackend) -> Result<T, SecretsError> + Send + 'static,
    ) -> Result<T, SecretsError> {
        let item: Item = (part, vault.clone(), key.cloned());
        let limit = self.effective_limit();
        if limit.is_zero() || self.abandoned.items().contains_key(&item) {
            return Err(self.timeout(operation, vault, key, Duration::ZERO));
        }
        let phase = Arc::new(Mutex::new(Phase::Running));
        let finish = Finish {
            phase: Arc::clone(&phase),
            abandoned: Arc::clone(&self.abandoned),
            item: item.clone(),
        };
        // Capacity 1: a late result is sent without blocking, then dropped.
        let (tx, rx) = mpsc::sync_channel(1);
        let inner = Arc::clone(&self.inner);
        std::thread::Builder::new()
            .name("trusty-secrets-backend-call".to_string())
            .spawn(move || {
                let _finish = finish;
                // The receiver is gone after a timeout; nothing to report.
                let _ = tx.send(call(inner.as_ref()));
            })
            .map_err(|e| self.failure(vault, key, format!("cannot start the call thread: {e}")))?;
        let ended = || {
            self.failure(
                vault,
                key,
                format!("the {operation} call ended with no result"),
            )
        };
        match rx.recv_timeout(limit) {
            Ok(result) => result,
            Err(RecvTimeoutError::Timeout) => {
                let mut phase = lock(&phase);
                if *phase == Phase::Running {
                    *phase = Phase::Abandoned;
                    *self.abandoned.items().entry(item).or_insert(0) += 1;
                    return Err(self.timeout(operation, vault, key, limit));
                }
                // It ended between the timeout and the lock; take its result.
                drop(phase);
                rx.try_recv().unwrap_or_else(|_| Err(ended()))
            }
            Err(RecvTimeoutError::Disconnected) => Err(ended()),
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
        self.run("get", Part::Value, vault, Some(key), move |b| b.get(&v, &k))
    }

    fn set(
        &self,
        vault: &VaultName,
        key: &SecretKey,
        value: &SecretValue,
    ) -> Result<(), SecretsError> {
        let (v, k, value) = (vault.clone(), key.clone(), value.clone());
        self.run("set", Part::Value, vault, Some(key), move |b| {
            b.set(&v, &k, &value)
        })
    }

    fn delete(&self, vault: &VaultName, key: &SecretKey) -> Result<bool, SecretsError> {
        let (v, k) = (vault.clone(), key.clone());
        self.run("delete", Part::Value, vault, Some(key), move |b| {
            b.delete(&v, &k)
        })
    }

    fn list_names(&self, vault: &VaultName) -> Result<Vec<SecretKey>, SecretsError> {
        let v = vault.clone();
        self.run("list_names", Part::Vault, vault, None, move |b| {
            b.list_names(&v)
        })
    }

    fn agents_may_use(&self, vault: &VaultName, key: &SecretKey) -> Result<bool, SecretsError> {
        let (v, k) = (vault.clone(), key.clone());
        self.run("agents_may_use", Part::Flag, vault, Some(key), move |b| {
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
        self.run(
            "set_agents_may_use",
            Part::Flag,
            vault,
            Some(key),
            move |b| b.set_agents_may_use(&v, &k, allowed),
        )
    }
}

#[cfg(test)]
#[path = "time_limit_tests.rs"]
mod tests;
