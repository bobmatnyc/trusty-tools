//! Validated names: secret keys, owners, repositories, vaults, backend ids.
//!
//! Why: every name here becomes a keychain service or account, an index file
//! name, or a `secret://` segment. A name that carries `/`, `..`, a control
//! character, or an empty segment could address a different vault or escape
//! the index directory, so each type validates once at construction and fails
//! closed. Code holding a [`VaultName`] never re-checks it.
//! What: one newtype per kind of name, each with a fallible constructor,
//! `TryFrom<String>` (so serde rejects a bad name at the wire), and `Display`.
//! Owner, repository, and vault names are folded to ASCII lowercase because
//! GitHub treats them case-insensitively; secret keys keep their case.
//! Test: `api_key_validation_table`, `api_vault_validation_table`,
//! `api_owner_and_repo_fold_to_lowercase`.

use std::fmt;

use serde::{Deserialize, Serialize};

use super::SecretsError;

/// First segment of every vault name (DOC-74 §6.3).
pub const VAULT_PREFIX: &str = "trusty";

/// Longest secret key accepted.
pub const MAX_KEY_LEN: usize = 256;

/// Longest owner, repository, or vault segment accepted (GitHub's repository
/// name limit).
pub const MAX_SEGMENT_LEN: usize = 100;

/// Longest backend id accepted.
const MAX_BACKEND_LEN: usize = 32;

/// Characters an owner, repository, or vault segment may hold.
fn is_segment_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')
}

/// Validate one path-like segment and return it lowercased.
///
/// Why: owners, repositories, and vault segments share one rule set, so it is
/// one function.
/// What: non-empty, at most [`MAX_SEGMENT_LEN`], not `.` or `..`, no control
/// character, only `[A-Za-z0-9._-]`. `/` and `\` fail the character rule.
/// Test: `api_vault_validation_table`.
fn checked_segment(what: &'static str, raw: &str) -> Result<String, SecretsError> {
    let fail = |reason| Err(SecretsError::InvalidName { what, reason });
    if raw.is_empty() {
        return fail("is empty");
    }
    if raw.len() > MAX_SEGMENT_LEN {
        return fail("is longer than 100 characters");
    }
    if raw == "." || raw == ".." {
        return fail("is a relative path component");
    }
    if raw.chars().any(char::is_control) {
        return fail("contains a control character");
    }
    if !raw.chars().all(is_segment_char) {
        return fail("may hold only [A-Za-z0-9._-]");
    }
    Ok(raw.to_ascii_lowercase())
}

/// Implements `Display`, `as_str`, `TryFrom<String>`, and `From<T> for String`
/// for a validated string newtype.
macro_rules! name_newtype_impls {
    ($ty:ident) => {
        impl $ty {
            /// The validated name.
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $ty {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl TryFrom<String> for $ty {
            type Error = SecretsError;

            fn try_from(raw: String) -> Result<Self, Self::Error> {
                Self::new(&raw)
            }
        }

        impl From<$ty> for String {
            fn from(name: $ty) -> String {
                name.0
            }
        }
    };
}

/// A secret's name within a vault, e.g. `OPENAI_API_KEY`.
///
/// Why: the key is the keychain account and an index row; it must be
/// addressable in both and must not parse as a `secret://` path.
/// What: 1–[`MAX_KEY_LEN`] characters of `[A-Za-z0-9_.-]`, starting with a
/// letter, digit, or `_`. Case is preserved.
/// Test: `api_key_validation_table`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct SecretKey(String);

impl SecretKey {
    /// Validate `raw` as a key name.
    ///
    /// Test: `api_key_validation_table`.
    pub fn new(raw: &str) -> Result<Self, SecretsError> {
        let fail = |reason| Err(SecretsError::InvalidKey { reason });
        let Some(first) = raw.chars().next() else {
            return fail("is empty");
        };
        if raw.len() > MAX_KEY_LEN {
            return fail("is longer than 256 characters");
        }
        if raw.chars().any(char::is_control) {
            return fail("contains a control character");
        }
        if !(first.is_ascii_alphanumeric() || first == '_') {
            return fail("must start with a letter, digit, or `_`");
        }
        if !raw
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
        {
            return fail("may hold only [A-Za-z0-9_.-]");
        }
        Ok(Self(raw.to_string()))
    }
}

name_newtype_impls!(SecretKey);

/// A GitHub owner (user or organisation), lowercased.
///
/// Test: `api_owner_and_repo_fold_to_lowercase`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct OwnerName(String);

impl OwnerName {
    /// Validate `raw` as an owner segment.
    ///
    /// Test: `api_owner_and_repo_fold_to_lowercase`.
    pub fn new(raw: &str) -> Result<Self, SecretsError> {
        checked_segment("owner", raw).map(Self)
    }
}

name_newtype_impls!(OwnerName);

/// A repository name, lowercased.
///
/// Test: `api_owner_and_repo_fold_to_lowercase`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct RepoName(String);

impl RepoName {
    /// Validate `raw` as a repository segment.
    ///
    /// Test: `api_owner_and_repo_fold_to_lowercase`.
    pub fn new(raw: &str) -> Result<Self, SecretsError> {
        checked_segment("repository", raw).map(Self)
    }
}

name_newtype_impls!(RepoName);

/// A vault: `trusty/<owner>` or `trusty/<owner>/<repo>` (DOC-74 §6.3, §15.3).
///
/// Why: the vault is the keychain service name and the index file stem. The
/// grammar is closed — exactly the `trusty` prefix plus one or two segments —
/// so a `secrets.vault` override can share a vault across repositories but
/// cannot name a path outside the grammar.
/// What: parsed from text by [`VaultName::new`], or built from validated parts
/// by [`VaultName::owner`] / [`VaultName::project`]. Lowercased.
/// Test: `api_vault_validation_table`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct VaultName(String);

impl VaultName {
    /// Parse a full vault name.
    ///
    /// What: `trusty/<seg>` or `trusty/<seg>/<seg>`, each segment checked like
    /// an owner. Any other prefix, depth, or empty segment is refused.
    /// Test: `api_vault_validation_table`.
    pub fn new(raw: &str) -> Result<Self, SecretsError> {
        let fail = |reason| {
            Err(SecretsError::InvalidName {
                what: "vault",
                reason,
            })
        };
        let mut parts = raw.split('/');
        if parts.next() != Some(VAULT_PREFIX) {
            return fail("must start with `trusty/`");
        }
        let segments: Vec<&str> = parts.collect();
        if segments.is_empty() || segments.len() > 2 {
            return fail("must be `trusty/<owner>` or `trusty/<owner>/<repo>`");
        }
        let mut name = String::from(VAULT_PREFIX);
        for segment in segments {
            name.push('/');
            name.push_str(&checked_segment("vault", segment)?);
        }
        Ok(Self(name))
    }

    /// The owner vault `trusty/<owner>`.
    pub fn owner(owner: &OwnerName) -> Self {
        Self(format!("{VAULT_PREFIX}/{owner}"))
    }

    /// The project vault `trusty/<owner>/<repo>`.
    pub fn project(owner: &OwnerName, repo: &RepoName) -> Self {
        Self(format!("{VAULT_PREFIX}/{owner}/{repo}"))
    }

    /// The vault rendered as one flat file-name stem: `/` becomes `%2F`.
    ///
    /// Why: `%` never appears in a validated vault, so the encoding is
    /// unambiguous and the index stays one directory deep.
    /// Test: `index_file_names_are_flat_and_distinct`.
    pub fn file_stem(&self) -> String {
        self.0.replace('/', "%2F")
    }
}

name_newtype_impls!(VaultName);

/// A backend id such as `keychain` (DOC-74 §6.1).
///
/// What: 1–32 characters of `[a-z0-9-]`, lowercased.
/// Test: `config_backend_precedence_table`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct BackendId(String);

impl BackendId {
    /// The shipped default backend (DOC-74 §6.1).
    pub const KEYCHAIN: &'static str = "keychain";

    /// Validate `raw` as a backend id.
    ///
    /// Test: `config_backend_precedence_table`.
    pub fn new(raw: &str) -> Result<Self, SecretsError> {
        let fail = |reason| {
            Err(SecretsError::InvalidName {
                what: "backend",
                reason,
            })
        };
        if raw.is_empty() || raw.len() > MAX_BACKEND_LEN {
            return fail("must be 1 to 32 characters");
        }
        let lowered = raw.to_ascii_lowercase();
        if !lowered
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        {
            return fail("may hold only [a-z0-9-]");
        }
        Ok(Self(lowered))
    }

    /// The 0600 value-file backend (#9326): the default only on a host with
    /// no Keychain backend, otherwise chosen only by config.
    pub const FILE: &'static str = "file";

    /// The 1Password CLI backend (#7519), compiled under `cli-backends`.
    pub const ONEPASSWORD: &'static str = "onepassword";

    /// The `onepassword` backend id.
    pub fn onepassword() -> Self {
        Self(Self::ONEPASSWORD.to_string())
    }

    /// The `keychain` backend id.
    pub fn keychain() -> Self {
        Self(Self::KEYCHAIN.to_string())
    }

    /// The `file` backend id.
    pub fn file() -> Self {
        Self(Self::FILE.to_string())
    }

    /// A backend id from a compile-time literal the crate itself defines.
    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn from_static(id: &'static str) -> Self {
        debug_assert!(Self::new(id).is_ok(), "static backend id must be valid");
        Self(id.to_string())
    }
}

name_newtype_impls!(BackendId);
