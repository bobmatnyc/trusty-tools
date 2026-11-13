//! A read of any Keychain item, by service and account (#10998).
//!
//! Why: a named copy may read an item no trusty vault holds — a token the
//! `gh` CLI or a browser stored. [`super::KeychainBackend`] addresses only
//! `trusty/...` services by vault and key, so it cannot name such an item.
//! What: [`read_keychain_item`] reads one item on its own thread and waits at
//! most [`KEYCHAIN_CALL_TIMEOUT`], cut to the request deadline. A missing
//! item is `Ok(None)`. Every error is built by `keyring_reason`, so no error
//! carries value bytes. Off macOS it fails closed like the backend. Read
//! only: a call abandoned at the time limit can write nothing.
//! #10998 fix round 1: while a timed-out read of an item still runs, a new
//! read of that item fails at once instead of starting another thread.
//! Test: `keychain_item_read_times_out_and_passes_results_through`,
//! `keychain_item_read_of_a_hung_item_fails_fast_until_it_ends`,
//! `keychain_item_read_fails_closed_off_macos`.

use std::collections::HashMap;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use super::keychain::KEYCHAIN_CALL_TIMEOUT;
use crate::api::methods::KeychainLabel;
use crate::api::{BackendId, SecretValue, SecretsError};

/// One item read, as the thread runs it: [`os_read`], or a test's fake.
pub(crate) type ItemRead =
    fn(&KeychainLabel, &KeychainLabel) -> Result<Option<SecretValue>, SecretsError>;

/// Read the Keychain item `service`/`account` through `read`.
///
/// What: [`read_within`] under [`KEYCHAIN_CALL_TIMEOUT`].
/// Test: `keychain_item_read_fails_closed_off_macos`.
pub(crate) fn read_keychain_item(
    service: &KeychainLabel,
    account: &KeychainLabel,
    read: ItemRead,
) -> Result<Option<SecretValue>, SecretsError> {
    read_within(service, account, KEYCHAIN_CALL_TIMEOUT, read)
}

/// Run `read` on its own thread for at most `limit`.
///
/// What: the limit is cut to the time left before the request deadline;
/// none left is [`SecretsError::Timeout`] before the thread starts. Past the
/// limit the caller gets [`SecretsError::Timeout`]; a thread that ends with
/// no result (a panic) is [`SecretsError::Backend`]. While an abandoned read
/// of the item still runs ([`abandoned`]), no thread starts and the caller
/// gets [`SecretsError::Timeout`] at once.
/// Test: `keychain_item_read_times_out_and_passes_results_through`,
/// `keychain_item_read_of_a_hung_item_fails_fast_until_it_ends`.
pub(crate) fn read_within(
    service: &KeychainLabel,
    account: &KeychainLabel,
    limit: Duration,
    read: ItemRead,
) -> Result<Option<SecretValue>, SecretsError> {
    let limit = effective_limit(limit);
    let timeout = |waited| SecretsError::Timeout {
        backend: BackendId::KEYCHAIN.to_string(),
        operation: "get",
        vault: service.to_string(),
        key: account.to_string(),
        waited,
    };
    if limit.is_zero() {
        return Err(timeout(Duration::ZERO));
    }
    let failure = |reason: String| SecretsError::Backend {
        backend: BackendId::KEYCHAIN.to_string(),
        vault: service.to_string(),
        key: account.to_string(),
        reason,
    };
    let item: Item = (service.as_str().to_owned(), account.as_str().to_owned());
    // #10998 fix round 1: one thread per hung item at most.
    if abandoned().contains_key(&item) {
        return Err(timeout(Duration::ZERO));
    }
    let phase = Arc::new(Mutex::new(Phase::Running));
    let finish = Finish {
        phase: Arc::clone(&phase),
        item: item.clone(),
    };
    // Capacity 1: a late result is sent without blocking, then dropped.
    let (tx, rx) = mpsc::sync_channel(1);
    let (owned_service, owned_account) = (service.clone(), account.clone());
    std::thread::Builder::new()
        .name("trusty-secrets-keychain-item".to_string())
        .spawn(move || {
            let _finish = finish;
            let _ = tx.send(read(&owned_service, &owned_account));
        })
        .map_err(|e| failure(format!("cannot start the call thread: {e}")))?;
    match rx.recv_timeout(limit) {
        Ok(result) => result,
        Err(RecvTimeoutError::Timeout) => {
            let mut phase = lock(&phase);
            if *phase == Phase::Running {
                *phase = Phase::Abandoned;
                *abandoned().entry(item).or_insert(0) += 1;
                return Err(timeout(limit));
            }
            // It ended between the timeout and the lock; take its result.
            drop(phase);
            rx.try_recv()
                .unwrap_or_else(|_| Err(failure("the get call ended with no result".to_string())))
        }
        Err(RecvTimeoutError::Disconnected) => {
            Err(failure("the get call ended with no result".to_string()))
        }
    }
}

/// One Keychain item: (service, account).
type Item = (String, String);

/// Where one read is; changed only under its own lock, taken before
/// [`abandoned`]'s.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Running,
    Abandoned,
    Done,
}

fn lock(phase: &Mutex<Phase>) -> MutexGuard<'_, Phase> {
    phase.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The items whose read timed out and is still running, and how many such
/// reads each has (#10998 fix round 1).
///
/// Why: code-critic MEDIUM on f11dc5c68f — every retry on a hung item (an
/// unanswered access prompt) left one more thread and one more prompt.
/// What: process-wide, like `TimeLimited`'s abandoned set; a read of a
/// listed item fails with [`SecretsError::Timeout`] at once, without a
/// thread. The entry goes when the last abandoned read ends.
/// Test: `keychain_item_read_of_a_hung_item_fails_fast_until_it_ends`.
fn abandoned() -> MutexGuard<'static, HashMap<Item, usize>> {
    static ABANDONED: LazyLock<Mutex<HashMap<Item, usize>>> = LazyLock::new(Mutex::default);
    ABANDONED.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Ends a read on its own thread: marks it done and, if its caller gave up
/// on it, clears its item. Runs on return and on panic alike.
struct Finish {
    phase: Arc<Mutex<Phase>>,
    item: Item,
}

impl Drop for Finish {
    fn drop(&mut self) {
        let mut phase = lock(&self.phase);
        if *phase == Phase::Abandoned {
            let mut items = abandoned();
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

/// `limit`, cut to the request deadline when one is set.
fn effective_limit(limit: Duration) -> Duration {
    super::deadline::remaining().map_or(limit, |left| left.min(limit))
}

/// The OS Keychain read of one item; the server's default [`ItemRead`].
#[cfg(target_os = "macos")]
pub(crate) fn os_read(
    service: &KeychainLabel,
    account: &KeychainLabel,
) -> Result<Option<SecretValue>, SecretsError> {
    let failure = |e: &keyring::Error| SecretsError::Backend {
        backend: BackendId::KEYCHAIN.to_string(),
        vault: service.to_string(),
        key: account.to_string(),
        // #10998: the one path to text; it drops `BadEncoding`'s bytes.
        reason: super::keychain::keyring_reason(e),
    };
    let entry = keyring::Entry::new(service.as_str(), account.as_str()).map_err(|e| failure(&e))?;
    match entry.get_password() {
        Ok(value) => Ok(Some(SecretValue::new(value))),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(failure(&e)),
    }
}

/// Off macOS no `keyring` is linked (#9064): fail closed.
#[cfg(not(target_os = "macos"))]
pub(crate) fn os_read(
    _: &KeychainLabel,
    _: &KeychainLabel,
) -> Result<Option<SecretValue>, SecretsError> {
    Err(super::keychain::no_os_backend())
}

#[cfg(test)]
#[path = "keychain_item_tests.rs"]
mod tests;
