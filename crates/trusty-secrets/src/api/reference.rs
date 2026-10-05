//! The `secret://` reference grammar (DOC-74 §15.3).
//!
//! Why: a `.env` file or an `exec --env` flag names a secret by reference, so
//! the value never sits in a file or a transcript. The grammar has three
//! forms and nothing else; anything that does not match is refused rather
//! than guessed at.
//! What: [`SecretRef::parse`] and its canonical `Display`.
//! - `secret://KEY` — project vault first, then owner vault.
//! - `secret://<owner>/KEY` — the owner vault.
//! - `secret://<owner>/<repo>/KEY` — a project vault.
//!
//! Test: `api_reference_grammar_table`, `api_reference_display_round_trips`.

use std::fmt;
use std::str::FromStr;

use super::{OwnerName, RepoName, SecretKey, SecretsError, VaultName};

/// The reference scheme.
pub const SCHEME: &str = "secret://";

/// A parsed `secret://` reference.
///
/// Why: see the module docs.
/// What: the scope the reference pins (none, an owner, or a project) and the
/// key. Holds names only.
/// Test: `api_reference_grammar_table`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SecretRef {
    /// `secret://KEY`: resolved project-first, then owner.
    Unscoped {
        /// The key.
        key: SecretKey,
    },
    /// `secret://<owner>/KEY`.
    Owner {
        /// The owner whose vault is named.
        owner: OwnerName,
        /// The key.
        key: SecretKey,
    },
    /// `secret://<owner>/<repo>/KEY`.
    Project {
        /// The owner.
        owner: OwnerName,
        /// The repository.
        repo: RepoName,
        /// The key.
        key: SecretKey,
    },
}

impl SecretRef {
    /// Parse a reference.
    ///
    /// Why: one parser for every consumer, so `exec`, `.env` handling, and
    /// the console agree on what a reference means.
    /// What: requires the exact `secret://` prefix, one to three non-empty
    /// `/`-separated segments, and no control characters; then validates the
    /// owner, repository, and key. Errors name the broken rule, never the
    /// input.
    /// Test: `api_reference_grammar_table`.
    pub fn parse(raw: &str) -> Result<Self, SecretsError> {
        let fail = |reason| Err(SecretsError::InvalidReference { reason });
        let Some(rest) = raw.strip_prefix(SCHEME) else {
            return fail("must start with `secret://`");
        };
        if rest.chars().any(char::is_control) {
            return fail("contains a control character");
        }
        let segments: Vec<&str> = rest.split('/').collect();
        if segments.iter().any(|s| s.is_empty()) {
            return fail("has an empty segment");
        }
        match segments.as_slice() {
            [key] => Ok(Self::Unscoped {
                key: SecretKey::new(key)?,
            }),
            [owner, key] => Ok(Self::Owner {
                owner: OwnerName::new(owner)?,
                key: SecretKey::new(key)?,
            }),
            [owner, repo, key] => Ok(Self::Project {
                owner: OwnerName::new(owner)?,
                repo: RepoName::new(repo)?,
                key: SecretKey::new(key)?,
            }),
            _ => fail("has more than three segments"),
        }
    }

    /// The key the reference names.
    pub fn key(&self) -> &SecretKey {
        match self {
            Self::Unscoped { key } | Self::Owner { key, .. } | Self::Project { key, .. } => key,
        }
    }

    /// The vault an explicit reference pins, or `None` for `secret://KEY`.
    ///
    /// Test: `api_reference_grammar_table`.
    pub fn pinned_vault(&self) -> Option<VaultName> {
        match self {
            Self::Unscoped { .. } => None,
            Self::Owner { owner, .. } => Some(VaultName::owner(owner)),
            Self::Project { owner, repo, .. } => Some(VaultName::project(owner, repo)),
        }
    }
}

impl fmt::Display for SecretRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unscoped { key } => write!(f, "{SCHEME}{key}"),
            Self::Owner { owner, key } => write!(f, "{SCHEME}{owner}/{key}"),
            Self::Project { owner, repo, key } => write!(f, "{SCHEME}{owner}/{repo}/{key}"),
        }
    }
}

impl FromStr for SecretRef {
    type Err = SecretsError;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        Self::parse(raw)
    }
}
