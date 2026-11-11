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
//! Test: `keychain_item_read_times_out_and_passes_results_through`,
//! `keychain_item_read_fails_closed_off_macos`.

use std::sync::mpsc::{self, RecvTimeoutError};
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
/// no result (a panic) is [`SecretsError::Backend`].
/// Test: `keychain_item_read_times_out_and_passes_results_through`.
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
    // Capacity 1: a late result is sent without blocking, then dropped.
    let (tx, rx) = mpsc::sync_channel(1);
    let (owned_service, owned_account) = (service.clone(), account.clone());
    std::thread::Builder::new()
        .name("trusty-secrets-keychain-item".to_string())
        .spawn(move || {
            let _ = tx.send(read(&owned_service, &owned_account));
        })
        .map_err(|e| failure(format!("cannot start the call thread: {e}")))?;
    match rx.recv_timeout(limit) {
        Ok(result) => result,
        Err(RecvTimeoutError::Timeout) => Err(timeout(limit)),
        Err(RecvTimeoutError::Disconnected) => {
            Err(failure("the get call ended with no result".to_string()))
        }
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
