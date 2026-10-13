//! The per-owner launch config file and its validation (#8988 PR 1a).
//!
//! Why: one owner's policy must not depend on each repository carrying a copy,
//! and a typo must fail loudly rather than no-op — so every table is strict.
//! What: [`OWNER_FILE`], the [`OwnerFile`] / [`McpTable`] schema (PR 1a: the
//! top-level `extends` key and `[mcp]` only; any other table, including PR 2's
//! `[tools]`/`[skills]`/`[agents]`, is an unknown key), [`parse_owner_file`],
//! and the [`OwnerPolicyError`] every bad-file arm reports.
//! Test: `schema_tests.rs`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::floor::is_floor;

/// Basename of the owner file, at `<repos_root>/<owner>/`.
///
/// Test: `an_empty_owner_file_is_a_valid_policy`.
pub const OWNER_FILE: &str = ".trusty-mpm-owner.toml";

/// The longest `extends` chain, counting the owner's own file.
///
/// Test: `a_chain_longer_than_four_files_is_a_bad_file`.
pub const MAX_CHAIN: usize = 4;

/// One owner file, as written.
///
/// Why: every key is optional, so an empty file is a valid "decline to decide".
/// What: `extends` names one parent owner; `mcp` is the MCP table.
/// Test: `an_empty_owner_file_is_a_valid_policy`, `a_pr2_table_is_an_unknown_key`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
// #8988: a misspelled key or an early PR 2 table is a bad file (Q7).
#[serde(deny_unknown_fields)]
pub struct OwnerFile {
    /// The parent owner whose file this one extends.
    #[serde(default)]
    pub extends: Option<String>,
    /// The `[mcp]` table.
    #[serde(default)]
    pub mcp: Option<McpTable>,
}

/// The `[mcp]` table of an owner file.
///
/// What: `allow` lists the user-scope servers that load (absent = all; `[]` =
/// none but the floor, Q5); `deny` lists servers that never load, applied after
/// `allow` (Q4); `claude_ai_connectors = false` asks for the claude.ai
/// connectors off.
/// Test: `allow_and_deny_may_share_the_table`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpTable {
    /// User-scope servers that load; `None` = all.
    #[serde(default)]
    pub allow: Option<Vec<String>>,
    /// Servers that never load under this owner.
    #[serde(default)]
    pub deny: Option<Vec<String>>,
    /// `Some(false)` turns the claude.ai connectors off.
    #[serde(default)]
    pub claude_ai_connectors: Option<bool>,
}

/// Why an owner file, or its `extends` chain, is a bad file.
///
/// Why: the doctor row and the fallback notice must name the file and the
/// reason, so the operator can fix it without guessing.
/// Test: `schema_tests.rs`, `compose_tests.rs`.
#[derive(Debug, thiserror::Error)]
pub enum OwnerPolicyError {
    /// The file exists but could not be read (EACCES, non-UTF-8).
    #[error("{} could not be read: {source}", .path.display())]
    Unreadable {
        /// The file.
        path: PathBuf,
        /// The I/O failure.
        #[source]
        source: std::io::Error,
    },
    /// Not valid TOML, or a key the schema does not define.
    #[error("{} is not a valid owner config: {source}", .path.display())]
    Malformed {
        /// The file.
        path: PathBuf,
        /// serde's message, naming the key.
        #[source]
        source: toml::de::Error,
    },
    /// A value the schema accepts but the policy forbids.
    #[error("{} is not a valid owner config ({key}: {reason})", .path.display())]
    Invalid {
        /// The file.
        path: PathBuf,
        /// The offending key.
        key: &'static str,
        /// What is wrong with it.
        reason: String,
    },
    /// `extends` returns to an owner already in the chain.
    #[error("extends cycle: {}", .chain.join(" → "))]
    Cycle {
        /// The chain, ending at the repeated owner.
        chain: Vec<String>,
    },
    /// `extends` names an owner with no owner file.
    #[error("extends names `{parent}`, but {} does not exist", .path.display())]
    MissingParent {
        /// The parent owner.
        parent: String,
        /// Where its file was looked for.
        path: PathBuf,
    },
    /// The chain is longer than [`MAX_CHAIN`] files.
    #[error("extends chain is longer than {MAX_CHAIN} files: {}", .chain.join(" → "))]
    TooDeep {
        /// The chain as far as it was read.
        chain: Vec<String>,
    },
    /// A parent in the chain is itself a bad file.
    #[error("extends parent `{parent}` is invalid: {source}")]
    InvalidParent {
        /// The parent owner.
        parent: String,
        /// Why the parent is bad.
        #[source]
        source: Box<OwnerPolicyError>,
    },
}

/// Whether `name` is usable as an owner directory name.
///
/// Why: an owner names a directory and a last-good file, so `/`, `..` and the
/// empty string must never reach a path join.
/// What: non-empty, ASCII alphanumerics plus `-`, `_`, `.`, not starting with `.`.
/// Test: `an_extends_value_that_is_not_an_owner_name_is_a_bad_file`.
pub fn is_owner_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// Read and validate one owner file.
///
/// Why: the one place a file becomes an [`OwnerFile`], so every caller gets the
/// same strictness.
/// What: `Ok(None)` when the file is absent; `Err` for an unreadable file, bad
/// TOML, an unknown key, a wildcard or empty server name, a floor server in
/// `deny`, or an `extends` value that is not an owner name. On success also
/// returns the raw bytes, for the last-good sha.
/// Test: every test in `schema_tests.rs`.
pub fn parse_owner_file(path: &Path) -> Result<Option<(OwnerFile, Vec<u8>)>, OwnerPolicyError> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(OwnerPolicyError::Unreadable {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    let text = std::str::from_utf8(&bytes).map_err(|e| OwnerPolicyError::Unreadable {
        path: path.to_path_buf(),
        source: std::io::Error::new(std::io::ErrorKind::InvalidData, e),
    })?;
    let file: OwnerFile = toml::from_str(text).map_err(|source| OwnerPolicyError::Malformed {
        path: path.to_path_buf(),
        source,
    })?;
    validate(path, &file)?;
    Ok(Some((file, bytes)))
}

/// Reject the values `deny_unknown_fields` cannot.
fn validate(path: &Path, file: &OwnerFile) -> Result<(), OwnerPolicyError> {
    let invalid = |key: &'static str, reason: String| OwnerPolicyError::Invalid {
        path: path.to_path_buf(),
        key,
        reason,
    };
    if let Some(parent) = &file.extends
        && !is_owner_name(parent)
    {
        return Err(invalid(
            "extends",
            format!("`{parent}` is not an owner name"),
        ));
    }
    let Some(mcp) = &file.mcp else {
        return Ok(());
    };
    for (key, names) in [("mcp.allow", &mcp.allow), ("mcp.deny", &mcp.deny)] {
        for name in names.iter().flatten() {
            if name.trim().is_empty() {
                return Err(invalid(key, "an empty server name".to_string()));
            }
            if name.contains(['*', '?']) {
                return Err(invalid(key, format!("wildcard `{name}` is not allowed")));
            }
        }
    }
    // #8988 Q2: the floor is never deniable.
    if let Some(name) = mcp.deny.iter().flatten().find(|n| is_floor(n)) {
        return Err(invalid(
            "mcp.deny",
            format!("`{name}` is in the never-deny floor"),
        ));
    }
    Ok(())
}

#[cfg(test)]
#[path = "schema_tests.rs"]
mod tests;
