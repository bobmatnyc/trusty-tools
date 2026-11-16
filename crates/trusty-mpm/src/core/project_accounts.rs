//! `[accounts]` in `.trusty-mpm.toml`, and the prompt block it renders (#11390).
//!
//! Why: an ops agent acting through `gh`, `vercel`, `neonctl` or `aws` acts as
//! whatever account the CLI is logged into. Owner ask (2026-11-16): agents must
//! know the account NAMES a project uses, so no permission is ever requested of
//! the wrong account. The names are project facts, so they live in the
//! committed project config, never in the machine-global deployed agent files.
//! What: [`ProjectAccounts`] parses and validates the table — a name only, an
//! optional scope and a read-only verify command, every value single-line,
//! bounded, and refused when the credential detector would change it.
//! [`account_block`] renders the `## Project accounts` block the pm-guard hook
//! appends to an ops dispatch.
//! Test: `project_accounts_tests.rs`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::core::project_config::{PROJECT_CONFIG_FILE, load_or_report};

/// Heading that opens the block; its presence in a prompt makes the append a no-op.
pub const ACCOUNT_BLOCK_HEADING: &str = "## Project accounts";

/// The standing rule every rendered block ends with.
pub const ACCOUNT_RULE: &str = "Before any permission request or account-scoped action, name the \
     account and run its verify command; on a mismatch, stop and report to the PM, never log in \
     or switch accounts.";

/// Most services one block lists; more is a config error, not a longer prompt.
pub const MAX_ACCOUNTS: usize = 32;

/// Longest accepted service key or value, in bytes.
pub const MAX_VALUE_LEN: usize = 200;

/// One `[accounts.<service>]` table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccountEntry {
    /// The account name the CLI must be acting as. Never a password or token.
    pub account: String,
    /// Team, org or project the account acts within, when the CLI has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    /// A read-only command that prints the CLI's current account.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verify: Option<String>,
}

/// The validated `[accounts]` map, keyed by service name.
///
/// Why: the file is committed and its values reach agent prompts, so a pasted
/// token must fail at load rather than be injected (#11390).
/// What: deserializes through [`ProjectAccounts::try_from`]; any rejected
/// value fails the whole file, which `load_or_report` logs and then ignores.
/// Test: `secret_shaped_account_value_rejects_the_file`,
/// `unknown_account_key_rejects_the_file`, `multiline_value_is_rejected`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "BTreeMap<String, AccountEntry>")]
pub struct ProjectAccounts(BTreeMap<String, AccountEntry>);

impl ProjectAccounts {
    /// The entries, ordered by service name.
    pub fn entries(&self) -> &BTreeMap<String, AccountEntry> {
        &self.0
    }
}

impl TryFrom<BTreeMap<String, AccountEntry>> for ProjectAccounts {
    type Error = String;

    fn try_from(map: BTreeMap<String, AccountEntry>) -> Result<Self, Self::Error> {
        if map.len() > MAX_ACCOUNTS {
            return Err(format!(
                "[accounts] lists {} services; at most {MAX_ACCOUNTS} are allowed",
                map.len()
            ));
        }
        for (service, entry) in &map {
            check_value("accounts.<service> name", service)?;
            let fields = [
                ("account", Some(&entry.account)),
                ("scope", entry.scope.as_ref()),
                ("verify", entry.verify.as_ref()),
            ];
            for (field, value) in fields {
                if let Some(value) = value {
                    check_value(&format!("accounts.{service}.{field}"), value)?;
                }
            }
        }
        Ok(Self(map))
    }
}

/// Refuse a value that is empty, too long, multi-line or credential-shaped.
///
/// The error names the key, never the value: a rejected value may be a secret.
fn check_value(key: &str, value: &str) -> Result<(), String> {
    if value.trim().is_empty() {
        return Err(format!("{key} is empty"));
    }
    if value.len() > MAX_VALUE_LEN {
        return Err(format!("{key} is longer than {MAX_VALUE_LEN} bytes"));
    }
    if value.chars().any(char::is_control) {
        return Err(format!("{key} must be one line with no control characters"));
    }
    // #11390: the detector the delegation guard redacts prompts with.
    if trusty_delegation_guard::redact::redact(value).redacted {
        return Err(format!(
            "{key} looks like a credential; [accounts] holds account NAMES only (value withheld)"
        ));
    }
    Ok(())
}

/// The nearest `.trusty-mpm.toml` at or above `cwd`, by directory.
///
/// Why: a dispatch can be made from a subdirectory of the project.
/// What: the first ancestor of `cwd` (itself included) holding the file.
/// Test: `accounts_are_found_from_a_subdirectory`.
pub fn project_dir_for(cwd: &Path) -> Option<PathBuf> {
    cwd.ancestors()
        .find(|dir| dir.join(PROJECT_CONFIG_FILE).is_file())
        .map(Path::to_path_buf)
}

/// The validated account map for the project containing `cwd`, if any.
///
/// What: `None` when no config is found, the file fails to parse (logged by
/// `load_or_report`), or it has no `[accounts]` table.
/// Test: `accounts_are_found_from_a_subdirectory`.
pub fn accounts_for(cwd: &Path) -> Option<ProjectAccounts> {
    load_or_report(&project_dir_for(cwd)?)?.accounts
}

/// Render the `## Project accounts` block for an ops prompt.
///
/// Why (#11390): the agent reads the names as data at dispatch time, so the
/// deployed agent files stay project-free.
/// What: a separator, the heading, one line per service (account, scope,
/// verify command), or one line saying no map is configured, then
/// [`ACCOUNT_RULE`]. Bounded by [`MAX_ACCOUNTS`] and [`MAX_VALUE_LEN`].
/// Test: `block_lists_each_service_and_the_rule`, `block_without_a_map_says_so`.
pub fn account_block(accounts: Option<&ProjectAccounts>) -> String {
    let mut out = format!("\n\n---\n{ACCOUNT_BLOCK_HEADING}\n\n");
    match accounts.filter(|a| !a.0.is_empty()) {
        None => out.push_str(
            "No account map is configured for this project (`[accounts]` in \
             `.trusty-mpm.toml`); the rule below applies to each CLI's current account.\n",
        ),
        Some(accounts) => {
            for (service, entry) in &accounts.0 {
                out.push_str(&format!("- `{service}`: account `{}`", entry.account));
                if let Some(scope) = &entry.scope {
                    out.push_str(&format!(", scope `{scope}`"));
                }
                match &entry.verify {
                    Some(verify) => out.push_str(&format!("; verify: `{verify}`\n")),
                    None => out.push_str("; verify: the CLI's own whoami command\n"),
                }
            }
        }
    }
    out.push('\n');
    out.push_str(ACCOUNT_RULE);
    out
}
