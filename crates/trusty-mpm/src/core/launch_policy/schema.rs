//! The per-owner launch config file and its validation (#8988 PR 1a).
//!
//! Why: one owner's policy must not depend on each repository carrying a copy,
//! and a typo must fail loudly rather than no-op — so every table is strict.
//! What: [`OWNER_FILE`], the [`OwnerFile`] schema — the top-level `extends`
//! key, [`McpTable`] (PR 1a), and #8992's [`SkillsTable`] / [`ToolsTable`];
//! any other table, `[agents]` included, is an unknown key —
//! [`parse_owner_file`], and the [`OwnerPolicyError`] every bad-file arm
//! reports. The `[skills]`/`[tools]` value checks live in
//! [`super::validate_tables`].
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
/// What: `extends` names one parent owner; `mcp`, `skills` and `tools` are
/// the three tables.
/// Test: `an_empty_owner_file_is_a_valid_policy`, `a_pr2_table_is_an_unknown_key`,
/// `good_skills_and_tools_tables_parse`.
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
    /// The `[skills]` table (#8992).
    #[serde(default)]
    pub skills: Option<SkillsTable>,
    /// The `[tools]` table (#8992).
    #[serde(default)]
    pub tools: Option<ToolsTable>,
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

/// The `[skills]` table of an owner file (#8992).
///
/// Why: Architect ruling Q-A — an owner may only turn skills off, so the one
/// key is `off` and there is no `on`.
/// What: exact skill names, each written as `"off"` in the launch's
/// `skillOverrides`; absent or `[]` turns nothing off.
/// Test: `good_skills_and_tools_tables_parse`,
/// `a_wrong_type_or_unknown_key_in_the_new_tables_is_malformed`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillsTable {
    /// Skills this owner turns off.
    #[serde(default)]
    pub off: Option<Vec<String>>,
}

/// The `[tools]` table of an owner file (#8992).
///
/// Why: Architect ruling Q-C — bare tool names only; a parenthesised rule such
/// as `Agent(engineer)` or `Bash(rm:*)` is a bad file.
/// What: built-in tool names, `mcp__<server>` and `mcp__<server>__<tool>`,
/// written to the launch's `permissions.deny`; absent or `[]` denies nothing.
/// Test: `good_skills_and_tools_tables_parse`,
/// `a_forbidden_skills_or_tools_value_is_a_bad_file_naming_its_key`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolsTable {
    /// Tools this owner denies.
    #[serde(default)]
    pub deny: Option<Vec<String>>,
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
/// What: `Ok(None)` when the path has no directory entry (a dangling symlink
/// is `Unreadable`); `Err` for an unreadable file, bad
/// TOML, an unknown key, a wildcard or empty server name, a floor server in
/// `deny`, an `extends` value that is not an owner name, or a bad `[skills]` /
/// `[tools]` value ([`super::validate_tables`]). On success also
/// returns the raw bytes, for the last-good sha.
/// Test: every test in `schema_tests.rs`.
pub fn parse_owner_file(path: &Path) -> Result<Option<(OwnerFile, Vec<u8>)>, OwnerPolicyError> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        // #8988: `read` follows symlinks, so NotFound also covers a dangling
        // symlink. Only a path with no directory entry at all is "absent".
        Err(e)
            if e.kind() == std::io::ErrorKind::NotFound
                && std::fs::symlink_metadata(path)
                    .is_err_and(|m| m.kind() == std::io::ErrorKind::NotFound) =>
        {
            return Ok(None);
        }
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
    // #8992: the `[skills]`/`[tools]` values, each an `Invalid` naming its key.
    super::validate_tables::validate(file).map_err(|(key, reason)| invalid(key, reason))?;
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
