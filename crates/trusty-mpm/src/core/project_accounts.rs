//! `[accounts]` in `.trusty-mpm.toml`, and the prompt block it renders (#11390).
//!
//! Why: an ops agent acting through `gh`, `vercel`, `neonctl` or `aws` acts as
//! whatever account the CLI is logged into. Owner ask (2026-11-16): agents must
//! know the account NAMES a project uses, so no permission is ever requested of
//! the wrong account. The names are project facts, so they live in the
//! committed project config, never in the machine-global deployed agent files.
//! What: the config keeps the table as raw TOML ([`RawAccounts`](crate::core::project_accounts::RawAccounts)), so a bad
//! entry never fails the rest of the file. [`accounts_for`](crate::core::project_accounts::accounts_for) validates it into
//! an [`AccountMap`](crate::core::project_accounts::AccountMap) — every value allowlisted by character, bounded, and
//! refused when it is shaped like a credential — and [`account_block`](crate::core::project_accounts::account_block)
//! renders the `## Project accounts` block the pm-guard hook appends to an ops
//! dispatch. No error or log line ever carries a value.
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

/// Most services one block lists; more rejects the map.
pub const MAX_ACCOUNTS: usize = 16;

/// Longest accepted service, account or scope, in bytes.
pub const MAX_NAME_LEN: usize = 100;

/// Longest accepted verify command, in bytes.
pub const MAX_VERIFY_LEN: usize = 200;

/// Prefixes of credential formats a name or verify word must not start with.
const TOKEN_PREFIXES: &[&str] = &["ghp_", "gho_", "github_pat_", "sk-", "xox", "AKIA", "eyJ"];

/// An alphanumeric run this long reads as a key, not a name.
const MAX_ALNUM_RUN: usize = 32;

/// The raw `[accounts]` value as parsed from TOML, validated later.
///
/// Why: a strict parse would fail the whole `.trusty-mpm.toml` on one bad
/// account value, silently reverting the worktree, session, style and model
/// settings too (#11390 review).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RawAccounts(pub toml::Value);

// A config value holds no float NaN in practice; `ProjectLevelConfig` is `Eq`.
impl Eq for RawAccounts {}

/// One validated `[accounts.<service>]` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountEntry {
    /// The account name the CLI must be acting as. Never a password or token.
    pub account: String,
    /// Team, org or project the account acts within, when the CLI has one.
    pub scope: Option<String>,
    /// A read-only command that prints the CLI's current account.
    pub verify: Option<String>,
}

/// What the project says about its accounts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccountMap {
    /// No config, or no `[accounts]` table.
    Absent,
    /// The table failed validation; the reason names keys, never values.
    Rejected(String),
    /// The validated entries, ordered by service name.
    Ok(BTreeMap<String, AccountEntry>),
}

impl RawAccounts {
    /// Validate the table.
    ///
    /// What: `Err(reason)` for a non-table, more than [`MAX_ACCOUNTS`]
    /// services, a key other than `account`/`scope`/`verify`, a missing
    /// `account`, a non-string value, or a value failing [`check_name`] /
    /// [`check_verify`]. The reason quotes a service name only after it
    /// passed [`check_name`].
    /// Test: `secret_shaped_values_are_rejected_without_echo`,
    /// `unknown_account_key_is_rejected`, `shell_metacharacters_in_verify_are_rejected`.
    pub fn validate(&self) -> Result<BTreeMap<String, AccountEntry>, String> {
        let table = self.0.as_table().ok_or("[accounts] must be a table")?;
        if table.len() > MAX_ACCOUNTS {
            return Err(format!(
                "[accounts] lists {} services; at most {MAX_ACCOUNTS} are allowed",
                table.len()
            ));
        }
        let mut out = BTreeMap::new();
        for (service, value) in table {
            check_name(service).map_err(|why| format!("a service name {why}"))?;
            let at = format!("accounts.{service}");
            let entry = value
                .as_table()
                .ok_or_else(|| format!("{at} must be a table"))?;
            if let Some(_unknown) = entry
                .keys()
                .find(|k| !matches!(k.as_str(), "account" | "scope" | "verify"))
            {
                return Err(format!(
                    "{at} has a key other than account, scope and verify"
                ));
            }
            let field = |name: &str| -> Result<Option<String>, String> {
                match entry.get(name) {
                    None => Ok(None),
                    Some(toml::Value::String(s)) => Ok(Some(s.clone())),
                    Some(_) => Err(format!("{at}.{name} must be a string")),
                }
            };
            let account = field("account")?.ok_or_else(|| format!("{at}.account is missing"))?;
            check_name(&account).map_err(|why| format!("{at}.account {why}"))?;
            let scope = field("scope")?;
            if let Some(scope) = &scope {
                check_name(scope).map_err(|why| format!("{at}.scope {why}"))?;
            }
            let verify = field("verify")?;
            if let Some(verify) = &verify {
                check_verify(verify).map_err(|why| format!("{at}.verify {why}"))?;
            }
            out.insert(
                service.clone(),
                AccountEntry {
                    account,
                    scope,
                    verify,
                },
            );
        }
        Ok(out)
    }
}

/// Is `c` allowed in a service, account or scope name?
fn name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '@' | ':' | '/' | ' ' | '-')
}

/// Accept a name of 1..=[`MAX_NAME_LEN`] bytes in `[A-Za-z0-9._@:/ -]`, not
/// shaped like a credential. The error never includes the value.
fn check_name(value: &str) -> Result<(), String> {
    check_shape(value, MAX_NAME_LEN, name_char)
}

/// Accept a verify command: the name characters plus `=`. This rejects every
/// shell metacharacter (`` ` `` `|` `;` `&` `$` `<` `>`), quotes and any
/// non-ASCII character, U+202E and U+200B included.
fn check_verify(value: &str) -> Result<(), String> {
    check_shape(value, MAX_VERIFY_LEN, |c| name_char(c) || c == '=')
}

fn check_shape(value: &str, max: usize, allowed: impl Fn(char) -> bool) -> Result<(), String> {
    if value.trim().is_empty() {
        return Err("is empty".into());
    }
    if value.len() > max {
        return Err(format!("is longer than {max} bytes"));
    }
    if !value.chars().all(allowed) {
        return Err("contains a character outside the allowed set".into());
    }
    if looks_like_credential(value) {
        return Err("looks like a credential; [accounts] holds account NAMES only".into());
    }
    Ok(())
}

/// A word starting with a known token prefix, or an alphanumeric run of at
/// least [`MAX_ALNUM_RUN`] characters.
fn looks_like_credential(value: &str) -> bool {
    let prefixed = value
        .split([' ', '='])
        .any(|word| TOKEN_PREFIXES.iter().any(|p| word.starts_with(p)));
    let longest_run = value
        .split(|c: char| !c.is_ascii_alphanumeric())
        .map(str::len)
        .max()
        .unwrap_or(0);
    prefixed || longest_run >= MAX_ALNUM_RUN
}

/// The nearest directory at or above `cwd` holding `.trusty-mpm.toml`.
///
/// Test: `accounts_are_found_from_a_subdirectory`.
pub fn project_dir_for(cwd: &Path) -> Option<PathBuf> {
    cwd.ancestors()
        .find(|dir| dir.join(PROJECT_CONFIG_FILE).is_file())
        .map(Path::to_path_buf)
}

/// The account map for the project containing `cwd`.
///
/// What: [`AccountMap::Absent`] with no config or no table (a file that fails
/// to parse for another reason is logged by `load_or_report`);
/// [`AccountMap::Rejected`], logged at `error`, when validation fails;
/// otherwise [`AccountMap::Ok`].
/// Test: `accounts_are_found_from_a_subdirectory`,
/// `bad_accounts_keep_the_rest_of_the_config`.
pub fn accounts_for(cwd: &Path) -> AccountMap {
    let Some(raw) = project_dir_for(cwd)
        .and_then(|dir| load_or_report(&dir))
        .and_then(|cfg| cfg.accounts)
    else {
        return AccountMap::Absent;
    };
    match raw.validate() {
        Ok(map) => AccountMap::Ok(map),
        Err(reason) => {
            tracing::error!("project account map rejected: {reason}");
            AccountMap::Rejected(reason)
        }
    }
}

/// Render the `## Project accounts` block for an ops prompt.
///
/// Why (#11390): the agent reads the names as data at dispatch time, so the
/// deployed agent files stay project-free.
/// What: a separator, the heading, one line per service, or one line saying
/// the map is absent or rejected, then [`ACCOUNT_RULE`].
/// Test: `block_lists_each_service_and_the_rule`, `block_without_a_map_says_so`,
/// `rejected_map_is_reported_as_rejected`.
pub fn account_block(accounts: &AccountMap) -> String {
    let mut out = format!("\n\n---\n{ACCOUNT_BLOCK_HEADING}\n\n");
    match accounts {
        AccountMap::Ok(map) if !map.is_empty() => {
            for (service, entry) in map {
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
        AccountMap::Rejected(reason) => out.push_str(&format!(
            "The project account map was rejected ({reason}); ask the PM before any \
             account-scoped action.\n"
        )),
        AccountMap::Ok(_) | AccountMap::Absent => out.push_str(
            "No account map is configured for this project (`[accounts]` in \
             `.trusty-mpm.toml`); the rule below applies to each CLI's current account.\n",
        ),
    }
    out.push('\n');
    out.push_str(ACCOUNT_RULE);
    out
}
