//! `[accounts]` in `~/.trusty-mpm/config.toml`: GitHub org → `gh` account (#9091).
//!
//! Why: owner ruling 33. `tm duettoresearch/<repo>` must run as `bob-duetto`
//! without naming `--account` on every invocation, so the config maps an org to
//! the account that clones and spawns for it.
//! What: [`OrgAccounts`] is the validated table. [`OrgAccounts::load`] reads it
//! strictly: an unparseable file or a malformed table is an `Err`, never an
//! empty map, because an empty map would clone and spawn as the machine's
//! active account. [`resolve_gh_account`] is the one precedence rule — an
//! explicit selection, then the org map, then none — and
//! [`resolve_gh_account_default`] is its production entry point, called by every
//! clone and spawn path.
//! Test: `gh_org_accounts_tests.rs`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The `[accounts]` table: org → `gh` login, matched case-insensitively.
///
/// Why: GitHub org names are case-insensitive, so `DuettoResearch` and
/// `duettoresearch` must find the same entry, and a table naming both is
/// ambiguous rather than "last one wins".
/// What: keys keep the case the operator wrote; [`Self::account_for`] folds case.
/// Test: `org_lookup_ignores_case`, `orgs_differing_only_in_case_are_refused`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OrgAccounts(BTreeMap<String, String>);

/// Why the `[accounts]` table could not be read.
///
/// Why: every variant is a configuration error the operator must see; none of
/// them may be read as "no mapping".
/// Test: `a_malformed_accounts_table_is_an_error`,
/// `unparseable_toml_is_an_error_not_an_empty_map`.
#[derive(Debug, thiserror::Error)]
pub enum OrgAccountsError {
    /// The file exists but could not be read.
    #[error("cannot read {path}: {source}")]
    Read {
        /// The config file.
        path: PathBuf,
        /// The I/O failure.
        source: std::io::Error,
    },
    /// The file is not valid TOML, so the table's contents are unknown.
    #[error("{path} is not valid TOML, so its [accounts] table cannot be read: {message}")]
    Parse {
        /// The config file.
        path: PathBuf,
        /// The parser's message.
        message: String,
    },
    /// The file parses but the `[accounts]` table is malformed.
    #[error("{path}: [accounts] {detail}")]
    Invalid {
        /// The config file.
        path: PathBuf,
        /// What is wrong, naming the offending entry.
        detail: String,
    },
}

/// Where a resolved account came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountSource {
    /// `--account`/`--user`/`--u`, an embedded `<login>@` selector, or a
    /// registry pin a flag wrote.
    Explicit,
    /// The `[accounts]` org map.
    OrgMap,
}

/// The account a clone or spawn runs as, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedAccount {
    /// The `gh` login.
    pub login: String,
    /// Which layer supplied it.
    pub source: AccountSource,
}

impl OrgAccounts {
    /// Parse the `[accounts]` table out of a whole `config.toml` text.
    ///
    /// What: an absent table is an empty map. A text that does not parse, an
    /// `accounts` value that is not a table, a non-string or blank value, a key
    /// or login with characters outside `[A-Za-z0-9._-]`, and two orgs that
    /// differ only in case are each an `Err` naming the problem.
    /// Test: `accounts_table_parses`, `absent_accounts_table_is_empty`,
    /// `a_malformed_accounts_table_is_an_error`,
    /// `unparseable_toml_is_an_error_not_an_empty_map`.
    pub fn from_toml(raw: &str, path: &Path) -> Result<Self, OrgAccountsError> {
        let doc = toml::from_str::<toml::Table>(raw).map_err(|e| OrgAccountsError::Parse {
            path: path.to_path_buf(),
            message: e.message().to_string(),
        })?;
        let invalid = |detail: String| OrgAccountsError::Invalid {
            path: path.to_path_buf(),
            detail,
        };
        let Some(value) = doc.get("accounts") else {
            return Ok(Self::default());
        };
        let toml::Value::Table(table) = value else {
            return Err(invalid(
                "must be a table of `org = \"login\"` entries".to_string(),
            ));
        };
        let mut map = BTreeMap::new();
        for (org, login) in table {
            let Some(login) = login.as_str().map(str::trim) else {
                return Err(invalid(format!("{org}: the login must be a string")));
            };
            if !is_name(org) {
                return Err(invalid(format!("'{org}' is not a GitHub org name")));
            }
            if !is_name(login) {
                return Err(invalid(format!("{org}: '{login}' is not a gh login")));
            }
            if let Some(other) = map.keys().find(|k: &&String| k.eq_ignore_ascii_case(org)) {
                return Err(invalid(format!(
                    "names org '{org}' twice ('{other}' and '{org}'); org names ignore case"
                )));
            }
            map.insert(org.clone(), login.to_string());
        }
        Ok(Self(map))
    }

    /// Read `<root>/config.toml`'s `[accounts]` table.
    ///
    /// What: a missing file is an empty map; any other read failure, and every
    /// [`Self::from_toml`] failure, is an `Err`.
    /// Test: `load_reads_the_table_from_the_root`, `load_of_a_missing_file_is_empty`.
    pub fn load(root: &Path) -> Result<Self, OrgAccountsError> {
        let path = root.join("config.toml");
        match std::fs::read_to_string(&path) {
            Ok(raw) => Self::from_toml(&raw, &path),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(source) => Err(OrgAccountsError::Read { path, source }),
        }
    }

    /// [`Self::load`] against `~/.trusty-mpm`, the root `MpmConfig::load_default`
    /// reads. No home directory is an empty map, as it is there.
    pub fn load_default() -> Result<Self, OrgAccountsError> {
        dirs::home_dir().map_or_else(
            || Ok(Self::default()),
            |home| Self::load(&home.join(".trusty-mpm")),
        )
    }

    /// The login mapped to `org`, compared without case.
    /// Test: `org_lookup_ignores_case`.
    pub fn account_for(&self, org: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(org.trim()))
            .map(|(_, v)| v.as_str())
    }
}

/// Resolve the account a clone or spawn for `owner`'s repository runs as.
///
/// Why: the flag, the registry pin and the org map were each read at a
/// different call site; one rule keeps "an explicit selection always wins" true
/// everywhere (#9091).
/// What: a non-blank `explicit` wins and the map is never read, so a malformed
/// config cannot block a flagged run. Otherwise, with an `owner`, `load` runs
/// and the mapped login is returned as [`AccountSource::OrgMap`]. No owner, or
/// an unmapped one, is `Ok(None)` — the ambient identity, as before. A `load`
/// failure is returned, never read as "unmapped".
/// Test: `flag_beats_map_beats_default`, `a_flag_never_reads_the_map`,
/// `a_load_failure_is_returned_not_read_as_unmapped`.
pub fn resolve_gh_account(
    explicit: Option<&str>,
    owner: Option<&str>,
    load: impl FnOnce() -> Result<OrgAccounts, OrgAccountsError>,
) -> Result<Option<ResolvedAccount>, OrgAccountsError> {
    if let Some(login) = explicit.map(str::trim).filter(|s| !s.is_empty()) {
        return Ok(Some(ResolvedAccount {
            login: login.to_string(),
            source: AccountSource::Explicit,
        }));
    }
    let Some(owner) = owner.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    Ok(load()?.account_for(owner).map(|login| ResolvedAccount {
        login: login.to_string(),
        source: AccountSource::OrgMap,
    }))
}

/// [`resolve_gh_account`] against `~/.trusty-mpm/config.toml`, taking the owner
/// from a git remote URL.
///
/// What: `origin_url` that names no `owner/repo` (a local path, a bare word)
/// has no owner, so it resolves to `explicit` or `None` without reading the map.
/// Test: `owner_of_reads_the_remote_owner`; the rule is [`resolve_gh_account`]'s.
pub fn resolve_gh_account_default(
    explicit: Option<&str>,
    origin_url: &str,
) -> Result<Option<ResolvedAccount>, OrgAccountsError> {
    resolve_gh_account(
        explicit,
        owner_of(origin_url).as_deref(),
        OrgAccounts::load_default,
    )
}

/// The spawn-time identity for an origin no registry record pins (#9091).
///
/// Why: a session spawned without `tm run`/`--account` (`tm launch`, `tm
/// sessions new`, a daemon-initiated spawn) never writes a pin, so the spawn
/// path asks the org map the same question the clone path does.
/// What: `Ok(Some)` is an account-only pin for the mapped login, proven at spawn
/// like any other; `Ok(None)` is the ambient identity, as before. A `load`
/// failure is `Err` with the fail-closed env — the nobody token plus a warning
/// naming the config error — so the session's gh never acts as the active
/// account while the table is broken.
/// Test: `an_unpinned_origin_takes_the_mapped_account`,
/// `a_broken_table_fails_the_spawn_closed`.
pub(crate) fn org_map_pin(
    origin: &str,
    load: impl FnOnce() -> Result<OrgAccounts, OrgAccountsError>,
) -> Result<Option<crate::core::gh_account::PinnedGhIdentity>, crate::core::gh_account::GhSpawnEnv>
{
    match resolve_gh_account(None, owner_of(origin).as_deref(), load) {
        Ok(resolved) => Ok(resolved.map(|r| crate::core::gh_account::PinnedGhIdentity {
            account: Some(r.login),
            config_dir: None,
        })),
        Err(e) => Err(crate::core::gh_account::GhSpawnEnv {
            vars: crate::core::gh_account_proof::identity_token_vars(None),
            warning: Some(format!(
                "{e}. The session's gh for {origin} is given a token that authenticates as \
                 nobody until the [accounts] table is fixed (#9091)."
            )),
        }),
    }
}

/// The owner segment of a git remote URL, verbatim, or `None`.
/// Test: `owner_of_reads_the_remote_owner`.
pub fn owner_of(origin_url: &str) -> Option<String> {
    trusty_common::github_path::parse_remote_url(origin_url)
        .ok()
        .map(|r| r.owner)
}

/// `[A-Za-z0-9._-]+` — the shape of a GitHub org name and a `gh` login.
fn is_name(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

#[cfg(test)]
#[path = "gh_org_accounts_tests.rs"]
mod tests;
