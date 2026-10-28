//! The OS Keychain backend over the `keyring` crate (DOC-74 §15.4).
//!
//! Why: the Keychain is the zero-configuration default backend (DOC-74 §6.1)
//! and ships first (§15.4 "Order"). macOS is the only supported platform.
//! What: [`KeychainBackend`] maps a vault to the entry's service and the key
//! to its account: service `trusty/<owner>/<repo>`, account `KEY`. #10386:
//! a key's agent deny is a second item, service [`AGENTS_DENY_SERVICE`],
//! account `<vault>/<key>`, present when the key is denied to agents; a
//! vault's migration marker is service [`DENY_MIGRATED_SERVICE`], account
//! `<vault>`; the pre-#10386 opt-in flag ([`AGENTS_SERVICE`], #9070) is read
//! once by that migration. Every read goes to
//! the OS; nothing is cached (§15.5). `keyring` errors are mapped to
//! fixed diagnostic text by `keyring_reason`, which drops the bytes a
//! `BadEncoding` error carries. On any target but macOS the crate links no
//! `keyring` (#9064), and every operation fails closed with
//! [`SecretsError::UnknownBackend`] rather than reaching `keyring`'s
//! in-memory mock store.
//! #7524 P2-L7: every call runs under [`KEYCHAIN_CALL_TIMEOUT`] (see
//! `store::time_limit`), so an unanswered access prompt cannot hold the
//! caller.
//! Test: `keychain_tests.rs` beside this file (error mapping and the
//! no-backend arm, no OS access); the ignored
//! `keychain_real_roundtrip_store_list_remove` touches the OS.

use std::sync::{Arc, LazyLock};
use std::time::Duration;

use super::time_limit::TimeLimited;
use super::{Capabilities, ControlItem, SecretBackend};
use crate::api::{BackendId, SecretKey, SecretValue, SecretsError, VaultName};

/// How long one Keychain call may run before the caller gets
/// [`SecretsError::Timeout`] (#7524 P2-L7).
///
/// Why: a Keychain call waits on an OS access prompt for as long as the
/// prompt is open. 60 s matches one vendor CLI call's budget, which also
/// leaves a person time to answer a prompt.
/// What: the default and only limit; a server request's earlier deadline
/// cuts it further. Not configurable.
/// Test: `keychain_backend_runs_every_call_under_the_time_limit`,
/// `time_limited_call_that_never_returns_times_out_on_every_operation`.
pub const KEYCHAIN_CALL_TIMEOUT: Duration = Duration::from_secs(60);

/// The Keychain service holding the pre-#10386 "agents may use" flag items
/// (#9070). Read only by the deny migration.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) const AGENTS_SERVICE: &str = "trusty-secrets.agents";

/// The Keychain service holding the per-key agent deny items (#10386).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) const AGENTS_DENY_SERVICE: &str = "trusty-secrets.agents-deny";

/// The Keychain service holding each vault's deny-migration marker (#10386).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) const DENY_MIGRATED_SERVICE: &str = "trusty-secrets.agents-deny-migrated";

/// The text a control item stores. Only the item's presence is read.
#[cfg(target_os = "macos")]
const AGENTS_MARKER: &str = "1";

/// The service holding `item`'s Keychain items.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn control_service(item: ControlItem) -> &'static str {
    match item {
        ControlItem::AgentDeny => AGENTS_DENY_SERVICE,
        ControlItem::LegacyAgentsFlag => AGENTS_SERVICE,
        ControlItem::DenyMigrated => DENY_MIGRATED_SERVICE,
    }
}

/// A control item's account: `<vault>/<key>`, or `<vault>` for a per-vault
/// item.
///
/// What: a key name holds no `/`, so the last segment is always the key and
/// two (vault, key) pairs never share an account.
/// Test: `keychain_agents_account_is_vault_then_key`.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn agents_account(vault: &VaultName, key: Option<&SecretKey>) -> String {
    match key {
        Some(key) => format!("{}/{}", vault.as_str(), key.as_str()),
        None => vault.as_str().to_string(),
    }
}

/// The OS keychain, addressed by vault (service) and key (account).
///
/// Why: see the module docs.
/// What: stateless. Capabilities are `READ | WRITE`: the Keychain cannot
/// list a service's accounts, so listing goes through the names-only index.
/// Test: `keychain_capabilities_are_read_write_only`.
#[derive(Debug, Default, Clone, Copy)]
pub struct KeychainBackend {
    _private: (),
}

impl KeychainBackend {
    /// The Keychain backend.
    pub fn new() -> Self {
        Self::default()
    }

    /// The OS Keychain behind [`KEYCHAIN_CALL_TIMEOUT`].
    // #7524 P2-L7: one process-wide wrapper, so every `KeychainBackend`
    // sees which items still have an abandoned call running.
    fn limited() -> &'static TimeLimited {
        static KEYCHAIN: LazyLock<TimeLimited> =
            LazyLock::new(|| TimeLimited::new(Arc::new(OsKeychain), KEYCHAIN_CALL_TIMEOUT));
        &KEYCHAIN
    }
}

/// The unbounded `keyring` calls; reached only through [`TimeLimited`].
#[derive(Debug, Clone, Copy)]
struct OsKeychain;

impl OsKeychain {
    #[cfg(target_os = "macos")]
    fn entry(&self, vault: &VaultName, key: &SecretKey) -> Result<keyring::Entry, SecretsError> {
        keyring::Entry::new(vault.as_str(), key.as_str()).map_err(|e| failure(vault, key, &e))
    }

    /// The control `item` for `key` in `vault` (#9070, #10386).
    #[cfg(target_os = "macos")]
    fn control_entry(
        &self,
        item: ControlItem,
        vault: &VaultName,
        key: Option<&SecretKey>,
    ) -> Result<keyring::Entry, SecretsError> {
        keyring::Entry::new(control_service(item), &agents_account(vault, key))
            .map_err(|e| control_failure(vault, key, &e))
    }
}

/// A [`SecretsError::Backend`] for a failed control-item call; a per-vault
/// item names key `(none)`.
#[cfg(target_os = "macos")]
fn control_failure(
    vault: &VaultName,
    key: Option<&SecretKey>,
    err: &keyring::Error,
) -> SecretsError {
    SecretsError::Backend {
        backend: BackendId::KEYCHAIN.to_string(),
        vault: vault.to_string(),
        key: key.map_or_else(|| "(none)".to_string(), ToString::to_string),
        reason: keyring_reason(err),
    }
}

/// Diagnostic text for a `keyring` error, built without any value bytes.
///
/// Why: `keyring::Error::BadEncoding` carries the stored bytes, which are the
/// secret. Its `Debug` prints them. This function is the only path from a
/// `keyring` error to text, and it never formats the error itself for that
/// variant.
/// What: one fixed sentence per variant; platform errors append the OS
/// message (an OS status, never the value); attribute errors name the
/// attribute only. Unknown future variants get a fixed sentence.
/// Test: `keychain_error_text_never_carries_value_bytes`.
#[cfg(target_os = "macos")]
pub(crate) fn keyring_reason(err: &keyring::Error) -> String {
    match err {
        keyring::Error::PlatformFailure(e) => format!("platform secure-storage failure: {e}"),
        keyring::Error::NoStorageAccess(e) => format!("secure storage is not accessible: {e}"),
        keyring::Error::NoEntry => "no such entry".to_string(),
        keyring::Error::BadEncoding(_) => "the stored value is not valid UTF-8".to_string(),
        keyring::Error::TooLong(attr, limit) => {
            format!("the {attr} attribute exceeds the platform limit of {limit}")
        }
        keyring::Error::Invalid(attr, _) => format!("the {attr} attribute is invalid"),
        keyring::Error::Ambiguous(_) => "more than one entry matches".to_string(),
        _ => "unrecognised keyring error".to_string(),
    }
}

/// A [`SecretsError::Backend`] for a failed Keychain call.
#[cfg(target_os = "macos")]
pub(crate) fn failure(vault: &VaultName, key: &SecretKey, err: &keyring::Error) -> SecretsError {
    SecretsError::Backend {
        backend: BackendId::KEYCHAIN.to_string(),
        vault: vault.to_string(),
        key: key.to_string(),
        reason: keyring_reason(err),
    }
}

/// Map a `get_password` result: `NoEntry` is a miss, anything else an error.
///
/// Test: `keychain_get_maps_no_entry_to_none_and_failures_to_errors`.
#[cfg(target_os = "macos")]
pub(crate) fn map_get(
    vault: &VaultName,
    key: &SecretKey,
    result: keyring::Result<String>,
) -> Result<Option<SecretValue>, SecretsError> {
    match result {
        Ok(value) => Ok(Some(SecretValue::new(value))),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(failure(vault, key, &e)),
    }
}

/// Map a `delete_credential` result: `NoEntry` is "nothing existed".
///
/// Test: `keychain_delete_maps_no_entry_to_false_and_failures_to_errors`.
#[cfg(target_os = "macos")]
pub(crate) fn map_delete(
    vault: &VaultName,
    key: &SecretKey,
    result: keyring::Result<()>,
) -> Result<bool, SecretsError> {
    match result {
        Ok(()) => Ok(true),
        Err(keyring::Error::NoEntry) => Ok(false),
        Err(e) => Err(failure(vault, key, &e)),
    }
}

/// Map a control item lookup: present is `true`, `NoEntry` is `false`,
/// anything else an error.
///
/// Why: #9070, #10386 — a lookup that failed must never read as "no deny",
/// nor as present, so the caller sees the Keychain fault.
/// Test: `keychain_flag_maps_no_entry_to_off_and_failures_to_errors`.
#[cfg(target_os = "macos")]
pub(crate) fn map_flag(
    vault: &VaultName,
    key: Option<&SecretKey>,
    result: keyring::Result<String>,
) -> Result<bool, SecretsError> {
    match result {
        Ok(_) => Ok(true),
        Err(keyring::Error::NoEntry) => Ok(false),
        Err(e) => Err(control_failure(vault, key, &e)),
    }
}

// #7524 P2-L7: each call goes through `TimeLimited`, never to `keyring`
// directly. `list_names` keeps the trait default: the Keychain cannot list.
impl SecretBackend for KeychainBackend {
    fn id(&self) -> BackendId {
        BackendId::keychain()
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::READ | Capabilities::WRITE
    }

    fn get(&self, vault: &VaultName, key: &SecretKey) -> Result<Option<SecretValue>, SecretsError> {
        Self::limited().get(vault, key)
    }

    fn set(
        &self,
        vault: &VaultName,
        key: &SecretKey,
        value: &SecretValue,
    ) -> Result<(), SecretsError> {
        Self::limited().set(vault, key, value)
    }

    fn delete(&self, vault: &VaultName, key: &SecretKey) -> Result<bool, SecretsError> {
        Self::limited().delete(vault, key)
    }

    fn holds_agent_controls(&self) -> bool {
        Self::limited().holds_agent_controls()
    }

    fn control_item(
        &self,
        item: ControlItem,
        vault: &VaultName,
        key: Option<&SecretKey>,
    ) -> Result<bool, SecretsError> {
        Self::limited().control_item(item, vault, key)
    }

    fn set_control_item(
        &self,
        item: ControlItem,
        vault: &VaultName,
        key: Option<&SecretKey>,
        present: bool,
    ) -> Result<(), SecretsError> {
        Self::limited().set_control_item(item, vault, key, present)
    }
}

impl SecretBackend for OsKeychain {
    fn id(&self) -> BackendId {
        BackendId::keychain()
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::READ | Capabilities::WRITE
    }

    #[cfg(target_os = "macos")]
    fn get(&self, vault: &VaultName, key: &SecretKey) -> Result<Option<SecretValue>, SecretsError> {
        map_get(vault, key, self.entry(vault, key)?.get_password())
    }

    #[cfg(not(target_os = "macos"))]
    fn get(&self, _: &VaultName, _: &SecretKey) -> Result<Option<SecretValue>, SecretsError> {
        Err(no_os_backend())
    }

    #[cfg(target_os = "macos")]
    fn set(
        &self,
        vault: &VaultName,
        key: &SecretKey,
        value: &SecretValue,
    ) -> Result<(), SecretsError> {
        self.entry(vault, key)?
            .set_password(value.expose())
            .map_err(|e| failure(vault, key, &e))
    }

    #[cfg(not(target_os = "macos"))]
    fn set(&self, _: &VaultName, _: &SecretKey, _: &SecretValue) -> Result<(), SecretsError> {
        Err(no_os_backend())
    }

    #[cfg(target_os = "macos")]
    fn delete(&self, vault: &VaultName, key: &SecretKey) -> Result<bool, SecretsError> {
        map_delete(vault, key, self.entry(vault, key)?.delete_credential())
    }

    #[cfg(not(target_os = "macos"))]
    fn delete(&self, _: &VaultName, _: &SecretKey) -> Result<bool, SecretsError> {
        Err(no_os_backend())
    }

    // #9070, #10386: control items are their own Keychain items, never index
    // fields. Off macOS every call fails closed, like a value call.
    fn holds_agent_controls(&self) -> bool {
        true
    }

    #[cfg(target_os = "macos")]
    fn control_item(
        &self,
        item: ControlItem,
        vault: &VaultName,
        key: Option<&SecretKey>,
    ) -> Result<bool, SecretsError> {
        map_flag(
            vault,
            key,
            self.control_entry(item, vault, key)?.get_password(),
        )
    }

    #[cfg(not(target_os = "macos"))]
    fn control_item(
        &self,
        _: ControlItem,
        _: &VaultName,
        _: Option<&SecretKey>,
    ) -> Result<bool, SecretsError> {
        Err(no_os_backend())
    }

    #[cfg(target_os = "macos")]
    fn set_control_item(
        &self,
        item: ControlItem,
        vault: &VaultName,
        key: Option<&SecretKey>,
        present: bool,
    ) -> Result<(), SecretsError> {
        let entry = self.control_entry(item, vault, key)?;
        if present {
            return entry
                .set_password(AGENTS_MARKER)
                .map_err(|e| control_failure(vault, key, &e));
        }
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(control_failure(vault, key, &e)),
        }
    }

    #[cfg(not(target_os = "macos"))]
    fn set_control_item(
        &self,
        _: ControlItem,
        _: &VaultName,
        _: Option<&SecretKey>,
        _: bool,
    ) -> Result<(), SecretsError> {
        Err(no_os_backend())
    }
}

/// The error every Keychain operation returns on a target with no OS backend.
///
/// Why: #9064 — `keyring` with no platform feature falls back to an
/// in-memory mock that accepts a write and loses it at exit. A store that
/// reported success there would lose secrets silently, so this build links no
/// `keyring` off macOS and refuses instead.
/// What: [`SecretsError::UnknownBackend`] naming `keychain`. Compiled on every
/// target so the macOS test suite covers it too.
/// Test: `keychain_without_os_backend_fails_closed`,
/// `keychain_backend_fails_closed_off_macos`.
#[cfg_attr(target_os = "macos", allow(dead_code))]
pub(crate) fn no_os_backend() -> SecretsError {
    SecretsError::UnknownBackend {
        backend: BackendId::KEYCHAIN.to_string(),
    }
}

#[cfg(test)]
#[path = "keychain_tests.rs"]
mod tests;
