//! `[accounts]` in `~/.trusty-mpm/config.toml`: GitHub org → `gh` account (#9091).
//!
//! Why: owner ruling 33. `tm duettoresearch/<repo>` must run as `bob-duetto`
//! without naming `--account` on every invocation, so the config maps an org to
//! the account that clones and spawns for it.
//! What: [`OrgAccounts`] is the validated table. [`OrgAccounts::inspect`] reads
//! it strictly: a malformed table is an `Err`, never an empty map, because an
//! empty map would clone and spawn as the machine's active account. The one
//! exception is a TOML syntax error in a file where no line names `accounts`,
//! which is an empty map plus a warning, as `MpmConfig::load` treats it.
//! [`resolve_gh_account_with`] is the one precedence rule — an explicit
//! selection, then the registry pin, then the org map, then none — and
//! [`resolve_gh_account`] is its production entry point, called by every clone
//! path and by the CLI launch preflight. Only github.com origins are mapped.
//! Test: `gh_org_accounts_tests.rs`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::core::gh_account_registry::RegistryPin;
use crate::session_manager::ssh_host_alias::SshHostAliases;

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
/// `a_syntax_error_with_an_accounts_header_is_an_error`.
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
    /// The file is not valid TOML and holds an `[accounts]` header, so the
    /// table's contents are unknown.
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
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AccountSource {
    /// `--account`/`--user`/`--u` or an embedded `<login>@` selector.
    Explicit,
    /// The project registry record a flag or `tm projects register` wrote.
    #[default]
    RegistryPin,
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

/// An `[accounts]` read: the table, plus the warning for a syntax error the
/// table was read past (#9091).
pub type Inspected = (OrgAccounts, Option<String>);

impl OrgAccounts {
    /// Parse the `[accounts]` table out of a whole `config.toml` text.
    ///
    /// What: [`Self::parse`], with the syntax-error warning logged.
    /// Test: `accounts_table_parses`, `absent_accounts_table_is_empty`,
    /// `a_malformed_accounts_table_is_an_error`.
    pub fn from_toml(raw: &str, path: &Path) -> Result<Self, OrgAccountsError> {
        Self::parse(raw, path).map(log_warning)
    }

    /// The table and any warning, from a whole `config.toml` text.
    ///
    /// What: an absent table is an empty map. #9091: a text that does not
    /// parse is an empty map and a warning when no line names the `accounts`
    /// table (see [`has_accounts_header`]), and [`OrgAccountsError::Parse`]
    /// when one does. An `accounts`
    /// value that is not a table, a non-string or blank value, a key or login
    /// with characters outside `[A-Za-z0-9._-]`, and two orgs that differ only
    /// in case are each an `Err` naming the problem.
    /// Test: `a_syntax_error_without_an_accounts_header_is_an_empty_map`,
    /// `a_syntax_error_with_an_accounts_header_is_an_error`.
    fn parse(raw: &str, path: &Path) -> Result<Inspected, OrgAccountsError> {
        let doc = match toml::from_str::<toml::Table>(raw) {
            Ok(doc) => doc,
            // #9091: the MpmConfig::load rule, unless the table is in the file.
            Err(e) if !has_accounts_header(raw) => {
                let warning = format!(
                    "{} is not valid TOML ({}); no line names an [accounts] table, so no org is \
                     mapped to a gh account",
                    path.display(),
                    e.message()
                );
                return Ok((Self::default(), Some(warning)));
            }
            Err(e) => {
                return Err(OrgAccountsError::Parse {
                    path: path.to_path_buf(),
                    message: e.message().to_string(),
                });
            }
        };
        let invalid = |detail: String| OrgAccountsError::Invalid {
            path: path.to_path_buf(),
            detail,
        };
        let Some(value) = doc.get("accounts") else {
            return Ok((Self::default(), None));
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
        Ok((Self(map), None))
    }

    /// Read `<root>/config.toml`'s `[accounts]` table and any warning.
    ///
    /// What: a missing file is an empty map; any other read failure, and every
    /// [`Self::parse`] failure, is an `Err`. `tm doctor` reports all three.
    /// Test: `load_reads_the_table_from_the_root`, `load_of_a_missing_file_is_empty`.
    pub fn inspect(root: &Path) -> Result<Inspected, OrgAccountsError> {
        let path = root.join("config.toml");
        match std::fs::read_to_string(&path) {
            Ok(raw) => Self::parse(&raw, &path),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok((Self::default(), None)),
            Err(source) => Err(OrgAccountsError::Read { path, source }),
        }
    }

    /// [`Self::inspect`] with the warning logged.
    pub fn load(root: &Path) -> Result<Self, OrgAccountsError> {
        Self::inspect(root).map(log_warning)
    }

    /// [`Self::load`] against `~/.trusty-mpm`, the root `MpmConfig::load_default`
    /// reads. No home directory is an empty map, as it is there.
    pub fn load_default() -> Result<Self, OrgAccountsError> {
        dirs::home_dir().map_or_else(
            || Ok(Self::default()),
            |home| Self::load(&home.join(".trusty-mpm")),
        )
    }

    /// The number of mapped orgs.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// True when no org is mapped.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
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

/// Log an [`Inspected`] warning and keep the table.
fn log_warning((accounts, warning): Inspected) -> OrgAccounts {
    if let Some(warning) = warning {
        tracing::warn!("{warning}");
    }
    accounts
}

/// Does any line of `raw` name the `accounts` table?
///
/// What: a line-level match after dropping a `#` comment, every whitespace
/// character and every quote. A header counts when it opens `accounts` or a
/// table under it — `[accounts]`, `["accounts"]`, `[[accounts]]`,
/// `[accounts.x]` — since the parser reads each of those as a malformed or
/// valid table. So does a key line `accounts = …` or `accounts.org = …`. A key
/// line under another table also counts, which errs toward refusing (#9091 r2).
/// Test: `a_syntax_error_with_an_accounts_header_is_an_error`,
/// `a_syntax_error_without_an_accounts_header_is_an_empty_map`.
fn has_accounts_header(raw: &str) -> bool {
    raw.lines().any(|line| {
        let code: String = line
            .split('#')
            .next()
            .unwrap_or_default()
            .chars()
            .filter(|c| !c.is_whitespace() && !matches!(c, '"' | '\''))
            .collect();
        let name = code.trim_start_matches('[');
        if name.len() < code.len() {
            return name.starts_with("accounts]") || name.starts_with("accounts.");
        }
        code.starts_with("accounts=") || code.starts_with("accounts.")
    })
}

/// Resolve the account a clone, spawn or launch for `origin_url` runs as.
///
/// Why: the flag, the registry pin and the org map were each read at a
/// different call site; one rule keeps clone and spawn on the same account
/// (#9091, config-convention.md "registry pin wins").
/// What: a non-blank `explicit` wins and nothing is read, so a malformed config
/// cannot block a flagged run. An origin with no github.com owner (see
/// [`owner_of`]) is `Ok(None)` — the ambient identity, as before. Otherwise a
/// registry pin wins: its login, or `Ok(None)` for a pin that names a config
/// dir but no login. With no pin, the `[accounts]` login for the owner, or
/// `Ok(None)` when unmapped. A pin or table read failure is an `Err`, never
/// read as "unmapped".
/// Test: `flag_beats_pin_beats_map_beats_default`, `a_flag_never_reads_the_map`,
/// `a_load_failure_is_returned_not_read_as_unmapped`.
pub(crate) fn resolve_gh_account_with(
    explicit: Option<&str>,
    origin_url: &str,
    aliases: &SshHostAliases,
    pin: impl FnOnce() -> Result<Option<RegistryPin>, String>,
    load: impl FnOnce() -> Result<OrgAccounts, OrgAccountsError>,
) -> Result<Option<ResolvedAccount>, String> {
    let resolved = |login: &str, source| ResolvedAccount {
        login: login.to_string(),
        source,
    };
    if let Some(login) = explicit.map(str::trim).filter(|s| !s.is_empty()) {
        return Ok(Some(resolved(login, AccountSource::Explicit)));
    }
    let Some(owner) = owner_of(origin_url, aliases) else {
        return Ok(None);
    };
    if let Some(pin) = pin()? {
        return Ok(pin
            .login()
            .map(|login| resolved(login, AccountSource::RegistryPin)));
    }
    let accounts = load().map_err(|e| e.to_string())?;
    Ok(accounts
        .account_for(&owner)
        .map(|login| resolved(login, AccountSource::OrgMap)))
}

/// [`resolve_gh_account_with`] against the operator's `~/.ssh/config`, the
/// project registry and `~/.trusty-mpm/config.toml`.
/// Test: the rule is [`resolve_gh_account_with`]'s.
pub fn resolve_gh_account(
    explicit: Option<&str>,
    origin_url: &str,
) -> Result<Option<ResolvedAccount>, String> {
    resolve_gh_account_with(
        explicit,
        origin_url,
        &SshHostAliases::for_current_user(),
        || {
            crate::core::gh_account_registry::read_pin(
                &crate::project::registry_data_dir(),
                origin_url,
            )
        },
        OrgAccounts::load_default,
    )
}

/// The spawn-time identity for an origin no registry record pins (#9091).
///
/// Why: a session spawned without `tm run`/`--account` (`tm launch`, `tm
/// sessions new`, a daemon-initiated spawn) never writes a pin, so the spawn
/// path asks the org map the same question the clone path does.
/// What: [`resolve_gh_account_with`] with no flag and no pin (the caller
/// already found none). `Ok(Some)` is an account-only pin for the mapped login,
/// tagged [`AccountSource::OrgMap`] and proven at spawn like any other;
/// `Ok(None)` is the ambient identity, as before. A `load` failure is `Err`
/// with the fail-closed env — the nobody token plus a warning naming the config
/// error — so the session's gh never acts as the active account.
/// Test: `an_unpinned_origin_takes_the_mapped_account`,
/// `a_broken_table_fails_the_spawn_closed`.
pub(crate) fn org_map_pin(
    origin: &str,
    aliases: &SshHostAliases,
    load: impl FnOnce() -> Result<OrgAccounts, OrgAccountsError>,
) -> Result<Option<crate::core::gh_account::PinnedGhIdentity>, crate::core::gh_account::GhSpawnEnv>
{
    match resolve_gh_account_with(None, origin, aliases, || Ok(None), load) {
        Ok(resolved) => Ok(resolved.map(|r| crate::core::gh_account::PinnedGhIdentity {
            account: Some(r.login),
            config_dir: None,
            source: r.source,
        })),
        Err(e) => Err(crate::core::gh_account::GhSpawnEnv {
            vars: crate::core::gh_account_proof::identity_token_vars(None),
            warning: Some(format!(
                "{e}. The session's gh for {} is given a token that authenticates as \
                 nobody until the [accounts] table is fixed (#9091).",
                // #9124: the origin may embed `user:token@`.
                crate::core::remote_url_redact::redact_stored_url(origin)
            )),
        }),
    }
}

/// The spawn warning for an `[accounts]` login no token is proven for (#9091).
///
/// Why: the registry-pin wording names `tm projects register --gh-account`,
/// which is the wrong fix when the account came from the config table.
/// Test: `an_org_map_refusal_names_the_table_not_the_registry`.
pub(crate) fn org_map_refusal(who: &str, reason: &str) -> String {
    format!(
        "the [accounts] table in ~/.trusty-mpm/config.toml maps this repository's org to gh \
         account '{who}', and no gh token is proven by `GET /user` to be its own ({reason}). \
         The session's gh is given a token that authenticates as nobody, so it fails instead \
         of acting as the machine's global account (#8510, #9091). Fix: `gh auth login` as \
         '{who}', or edit the [accounts] table."
    )
}

/// The owner of a github.com remote URL, verbatim, or `None`.
///
/// What: the remote is read the way `gh --repo` reads it — an SSH host alias
/// resolved through `aliases` (#9089), so `git@github-bob:org/x` maps when
/// `github-bob` names github.com. Any other host — gitlab.com, Bitbucket,
/// GitHub Enterprise Server — and a URL naming no repository are `None`.
/// Test: `owner_of_reads_the_remote_owner`.
pub(crate) fn owner_of(origin_url: &str, aliases: &SshHostAliases) -> Option<String> {
    // #9091: a slug that keeps a host is not github.com's.
    let slug =
        crate::session_manager::worktree_repo_slug::parse_repo_slug(origin_url, aliases).ok()?;
    let (owner, repo) = slug.split_once('/')?;
    (!repo.contains('/')).then(|| owner.to_string())
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
