//! The source locator of a named `secrets.copy` (#10998, DOC-74 §13 Q6).
//!
//! Why: owner ruling 2026-11-10 22:22Z — on a single-user machine, copying a
//! secret between stores is a user-allowable action. The copy before it read
//! only this project's own vault, under the same key name. A locator names
//! the source in its store's own terms, apart from the destination key.
//! What: [`CopySource`] is either a Keychain item by service and account,
//! or a key in a trusty vault (the owner vault, this project's or another
//! project's). The backend is the request's `from_backend`. Both names of a
//! Keychain item are [`KeychainLabel`]s: validated text, never a value.
//! Test: `api_copy_source_round_trips_and_refuses_bad_labels`,
//! `api_copy_request_without_source_keeps_the_old_wire_shape`.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::api::{SecretKey, SecretsError, VaultName};

/// Longest Keychain service or account accepted, in bytes.
pub const MAX_KEYCHAIN_LABEL_LEN: usize = 255;

/// A Keychain item's service or account, as the caller named it.
///
/// Why: the name goes on the audit record and into a Keychain lookup, so it
/// is checked once at the wire: never empty, never a control character.
/// What: 1–[`MAX_KEYCHAIN_LABEL_LEN`] bytes with no control character or
/// leading or trailing whitespace. Case is kept: the Keychain matches it
/// exactly.
/// Test: `api_copy_source_round_trips_and_refuses_bad_labels`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct KeychainLabel(String);

impl KeychainLabel {
    /// Validate `raw` as a Keychain service or account.
    ///
    /// Test: `api_copy_source_round_trips_and_refuses_bad_labels`.
    pub fn new(raw: &str) -> Result<Self, SecretsError> {
        let fail = |reason| {
            Err(SecretsError::InvalidName {
                what: "keychain item",
                reason,
            })
        };
        if raw.is_empty() {
            return fail("is empty");
        }
        if raw.len() > MAX_KEYCHAIN_LABEL_LEN {
            return fail("is longer than 255 bytes");
        }
        if raw.chars().any(char::is_control) {
            return fail("contains a control character");
        }
        if raw.trim() != raw {
            return fail("starts or ends with whitespace");
        }
        Ok(Self(raw.to_owned()))
    }

    /// The validated text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for KeychainLabel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<String> for KeychainLabel {
    type Error = SecretsError;

    fn try_from(raw: String) -> Result<Self, Self::Error> {
        Self::new(&raw)
    }
}

impl From<KeychainLabel> for String {
    fn from(label: KeychainLabel) -> String {
        label.0
    }
}

/// Where a named copy reads its one value (#10998).
///
/// Why: see the module docs.
/// What: on the wire, `{"keychain_item": {"service": …, "account": …}}` or
/// `{"vault": {"vault": "trusty/<owner>[/<repo>]", "key": …}}`. A Keychain
/// item needs `from_backend` `keychain`; a vault key is read from
/// `from_backend`. The server refuses a Keychain item in trusty's own
/// namespace: a trusty key is named as a `vault` source, so its agent deny
/// is checked.
/// Test: `api_copy_source_round_trips_and_refuses_bad_labels`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
#[non_exhaustive]
pub enum CopySource {
    /// Any Keychain item, by its service and account.
    KeychainItem {
        /// The item's service.
        service: KeychainLabel,
        /// The item's account.
        account: KeychainLabel,
    },
    /// A key in a trusty vault on `from_backend`.
    Vault {
        /// The vault, e.g. the owner vault `trusty/<owner>`.
        vault: VaultName,
        /// The key in that vault.
        key: SecretKey,
    },
}

impl CopySource {
    /// A Keychain item source.
    pub fn keychain_item(service: KeychainLabel, account: KeychainLabel) -> Self {
        Self::KeychainItem { service, account }
    }

    /// A trusty vault key source.
    pub fn vault(vault: VaultName, key: SecretKey) -> Self {
        Self::Vault { vault, key }
    }
}
