//! Which `gh` config dir may stand in for an account-only pin (#8510).
//!
//! Why: a registry record can pin `gh_account` with no `github.config_dir`.
//! #8416 put the registry ahead of the static config, so every such record
//! refused merged-PR lookups even when a dir that selects the account exists.
//! A dir may stand in only when it provably selects the pinned account: the
//! `user:` line in `hosts.yml` is not proof, because `gh` falls back to the
//! keyring's unkeyed slot for a user with no token of its own (#8510 critic,
//! gh 2.98.0). #5851 is why the account name alone is never trusted.
//!
//! What: [`AccountDirSources`] lists the candidates, in order: the static
//! config's per-origin `github.config_dir`, then tm's own
//! `<state_root>/gh-accounts/<login>`. A candidate is borrowed only when
//! (1) its `hosts.yml` names the pin as the ACTIVE `user:` for the origin's
//! host, and (2) under that dir, with every token variable removed,
//! `gh auth token --hostname <host>` and `gh auth token --hostname <host> -u
//! <login>` both succeed and print the SAME token. Both checks are local: the
//! file read and the keyring lookup, never a network call. The repository
//! owner never selects the account. Every failure is a named reason that never
//! contains a token.
//!
//! Test: the `an_account_only_pin_*` arms in `gh_account_registry_tests`.

use std::path::{Path, PathBuf};

use crate::core::trusty_tools_config::TrustyToolsConfig;
use crate::project::record::repo_url_matches;

/// Directory under the tm state root holding one `gh` config dir per account
/// (#7166); the daemon's `account_config_dir` bootstrap names it from here.
pub(crate) const GH_ACCOUNTS_DIR_NAME: &str = "gh-accounts";

/// Reject a `login` that would escape or corrupt the
/// `<state_root>/gh-accounts/<login>` join (#7166 review follow-up MEDIUM).
///
/// Why: the CLI's `is_name_segment` restricts only the character set, so `.`
/// and `..` pass it; `..` would resolve to the state root itself.
/// What: refuses an empty `login`, exactly `.` or `..`, or one containing `/`
/// or `\`.
/// Test: `ensure_account_config_dir_refuses_dot`,
/// `ensure_account_config_dir_refuses_dotdot`,
/// `ensure_account_config_dir_refuses_empty`,
/// `ensure_account_config_dir_refuses_a_forward_slash`,
/// `ensure_account_config_dir_refuses_a_backslash`,
/// `an_account_only_pin_refuses_an_unsafe_login_segment`.
pub(crate) fn reject_unsafe_login_segment(login: &str) -> Result<(), String> {
    if login.is_empty()
        || login == "."
        || login == ".."
        || login.contains('/')
        || login.contains('\\')
    {
        return Err(format!(
            "'{login}' is not a valid account login for a config directory segment"
        ));
    }
    Ok(())
}

/// `true` when `path` exists and is ITSELF a symlink (#7166 review follow-up
/// LOW), checked with `symlink_metadata`, which does not follow the link.
fn is_symlink(path: &Path) -> bool {
    path.symlink_metadata()
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
}

/// tm's own `<state_root>/gh-accounts/<login>` dir, refused when unsafe.
///
/// Why: the daemon's bootstrap and the #8510 borrow must apply the same
/// refusals to the same path; a second copy would drift.
/// What: [`reject_unsafe_login_segment`], then the join, then a refusal when
/// the dir or its `hosts.yml` is a symlink. `is_file()` follows a link, so a
/// symlinked `hosts.yml` would otherwise pass as "already built".
/// Test: `ensure_account_config_dir_places_it_under_gh_accounts`,
/// `ensure_account_config_dir_refuses_a_symlinked_dir`,
/// `ensure_account_config_dir_refuses_a_symlinked_hosts_yml`,
/// `an_account_only_pin_refuses_a_symlinked_tm_account_dir`.
pub(crate) fn tm_account_dir(state_root: &Path, login: &str) -> Result<PathBuf, String> {
    reject_unsafe_login_segment(login)?;
    let dir = state_root.join(GH_ACCOUNTS_DIR_NAME).join(login);
    if is_symlink(&dir) {
        return Err(format!(
            "{} is a symlink — refusing to use it as a per-account gh config directory; \
             remove it and retry",
            dir.display()
        ));
    }
    let hosts_yml = dir.join("hosts.yml");
    if is_symlink(&hosts_yml) {
        return Err(format!(
            "{} is a symlink — refusing to use it as a per-account gh config file; remove it \
             and retry",
            hosts_yml.display()
        ));
    }
    Ok(dir)
}

/// Asks `gh` which token it would use under a config dir (#8510).
///
/// Why: whether a dir selects an account is decided by `gh`'s keyring lookup,
/// which a test cannot run hermetically. The trait is the seam: production
/// runs `gh`, tests script the answers.
/// What: `token` returns the token `gh auth token --hostname <host>` prints
/// with `GH_CONFIG_DIR=<dir>` and every token variable removed, plus
/// `-u <login>` when `login` is set. An `Err` must never contain a token.
/// Test: `an_account_only_pin_refuses_a_dir_whose_login_has_no_token`.
pub(crate) trait GhTokenProbe {
    /// The token `gh` resolves under `dir` for `host`, optionally for `login`.
    fn token(&self, dir: &Path, host: &str, login: Option<&str>) -> Result<String, String>;
}

/// The production [`GhTokenProbe`]: runs the real `gh auth token`.
///
/// Why: `gh auth token` reads the config dir and the keyring only; it makes no
/// network call. It is bounded all the same, because a wedged `securityd`
/// hangs it (#6867).
/// What: [`crate::session_manager::worktree_reclaim_gh::gh_command`] rooted at
/// `dir` with `GH_CONFIG_DIR=dir` and every inherited identity variable
/// removed, bounded by [`crate::core::gh_account::GH_ENFORCE_TIMEOUT`]. The
/// failure reason carries `gh`'s exit code and stderr, never its stdout.
/// Test: none directly (a real `gh` and keyring); the decisions it feeds are
/// covered through scripted probes in `gh_account_registry_tests`.
pub(crate) struct CliTokenProbe;

impl GhTokenProbe for CliTokenProbe {
    fn token(&self, dir: &Path, host: &str, login: Option<&str>) -> Result<String, String> {
        use crate::session_manager::worktree_reclaim_gh::{gh_command, run_with_timeout};
        let cfg = crate::core::trusty_tools_config::GithubConfig {
            config_dir: Some(dir.to_path_buf()),
            ..Default::default()
        };
        let env =
            crate::core::gh_identity::resolve_gh_env(Some(&cfg)).map_err(|e| e.to_string())?;
        let mut cmd = gh_command(dir, &env);
        cmd.args(["auth", "token", "--hostname", host]);
        if let Some(login) = login {
            cmd.args(["-u", login]);
        }
        let token = run_with_timeout(cmd, crate::core::gh_account::GH_ENFORCE_TIMEOUT)
            .map_err(|failure| failure.to_string())?;
        let token = token.trim();
        if token.is_empty() {
            return Err("printed no token".to_string());
        }
        Ok(token.to_string())
    }
}

/// The config dirs an account-only pin may borrow (#8510).
///
/// Why: see the module docs. Each candidate is verified before it is used.
/// What: the static config's per-project `config_dir` for this origin (the
/// global binding is not "for this origin"), then tm's `<state_root>/
/// gh-accounts/<login>`. `Default` names neither: the pre-#8510 refusal.
/// Test: `account_dir_sources_take_only_this_origins_static_binding`.
#[derive(Debug, Default, Clone)]
pub(crate) struct AccountDirSources {
    /// The static `projects[].github.config_dir` bound to this origin.
    pub(crate) static_config_dir: Option<PathBuf>,
    /// tm's state root, whose `gh-accounts/<login>` is the second candidate.
    pub(crate) state_root: Option<PathBuf>,
}

impl AccountDirSources {
    /// The candidates for `origin`: its static binding and tm's own account dirs.
    /// Test: `account_dir_sources_take_only_this_origins_static_binding`.
    pub(crate) fn for_origin(
        config: &TrustyToolsConfig,
        origin: &str,
        state_root: PathBuf,
    ) -> Self {
        let static_config_dir = config
            .projects
            .iter()
            .find(|p| repo_url_matches(&p.repo_url, origin))
            .and_then(|p| p.github.as_ref())
            .and_then(crate::core::gh_account_registry::selected_config_dir);
        Self {
            static_config_dir,
            state_root: Some(state_root),
        }
    }

    /// The first candidate that provably selects `login` on `origin`'s host,
    /// else every candidate's reason for refusal.
    ///
    /// What: the host comes from `origin`; an origin with no parsable host
    /// refuses. A candidate must pass [`verify_active_user`] and then
    /// [`verify_token_selects`].
    /// Test: `an_account_only_pin_borrows_a_static_dir_whose_active_user_matches`,
    /// `an_account_only_pin_falls_back_to_tms_own_account_dir`,
    /// `an_account_only_pin_refuses_an_origin_with_no_host`.
    pub(crate) fn verified_dir(
        &self,
        login: &str,
        origin: &str,
        probe: &dyn GhTokenProbe,
    ) -> Result<PathBuf, Vec<String>> {
        let host = trusty_common::github_path::parse_remote_url(origin)
            .map(|remote| remote.host.to_ascii_lowercase())
            .map_err(|e| vec![format!("cannot tell which gh host serves it ({e})")])?;
        let mut candidates: Vec<Result<PathBuf, String>> =
            self.static_config_dir.iter().cloned().map(Ok).collect();
        if let Some(root) = &self.state_root {
            candidates.push(tm_account_dir(root, login));
        }
        let mut reasons = Vec::new();
        for candidate in candidates {
            let verified = candidate.and_then(|dir| {
                verify_active_user(&dir, login, &host)?;
                verify_token_selects(&dir, login, &host, probe)?;
                Ok(dir)
            });
            match verified {
                Ok(dir) => return Ok(dir),
                Err(reason) => reasons.push(reason),
            }
        }
        Err(reasons)
    }
}

/// Does `dir`'s local `hosts.yml` name `login` as the ACTIVE user for `host`?
///
/// Why: only the active `user:` is who `gh` acts as under `GH_CONFIG_DIR=dir`,
/// and only for the host the repository lives on (#7057 carries
/// non-github.com hosts). A dir that merely lists the account would probe as
/// someone else.
/// What: `Ok(())` on a case-insensitive match (GitHub logins are); otherwise
/// `Err` naming the dir and the failure — missing dir, unreadable or
/// unparsable `hosts.yml`, no active user for `host`, or a different one.
/// Test: `an_account_only_pin_refuses_a_static_dir_active_as_another_account`,
/// `an_account_only_pin_refuses_a_dir_listing_it_but_active_as_another`,
/// `an_account_only_pin_refuses_a_missing_candidate_dir`,
/// `an_account_only_pin_refuses_an_unreadable_hosts_yml`,
/// `an_account_only_pin_refuses_a_malformed_hosts_yml`,
/// `an_account_only_pin_refuses_a_hosts_yml_with_no_active_user`,
/// `an_account_only_pin_checks_the_origins_host_not_github_com`,
/// `an_account_only_pin_never_resolves_from_the_repository_owner`.
fn verify_active_user(dir: &Path, login: &str, host: &str) -> Result<(), String> {
    if !dir.is_dir() {
        return Err(format!("{} does not exist", dir.display()));
    }
    let hosts = dir.join("hosts.yml");
    let text = std::fs::read_to_string(&hosts)
        .map_err(|e| format!("{} could not be read ({e})", hosts.display()))?;
    let doc: serde_yaml::Value = serde_yaml::from_str(&text)
        .map_err(|e| format!("{} did not parse ({e})", hosts.display()))?;
    let entry = doc.get(host);
    let active = entry
        .and_then(|h| h.get("user"))
        .and_then(serde_yaml::Value::as_str)
        .map(str::trim)
        .filter(|u| !u.is_empty());
    match active {
        Some(user) if user.eq_ignore_ascii_case(login) => Ok(()),
        Some(user) => {
            let lists = entry
                .and_then(|h| h.get("users"))
                .and_then(serde_yaml::Value::as_mapping)
                .is_some_and(|users| {
                    users
                        .keys()
                        .filter_map(serde_yaml::Value::as_str)
                        .any(|k| k.trim().eq_ignore_ascii_case(login))
                });
            let how = if lists {
                format!("lists '{login}' but is active on {host} as")
            } else {
                format!("is active on {host} as")
            };
            Err(format!("{} {how} '{user}'", hosts.display()))
        }
        None => Err(format!("{} names no active {host} user", hosts.display())),
    }
}

/// Does `gh` under `dir` actually use `login`'s own keyring token for `host`?
///
/// Why: the #8510 HIGH. `gh` falls back to the keyring's unkeyed slot when the
/// active user has no token of its own, so a correct `user:` line still probes
/// as whichever account owns that slot. `-u <login>` exits non-zero for a login
/// with no token, and a match between the two answers proves the active
/// credential IS the login's.
/// What: `Ok(())` when both lookups succeed and return the same token. The two
/// tokens are compared in memory and dropped; no reason ever includes one.
/// Test: `an_account_only_pin_refuses_a_dir_whose_login_has_no_token`,
/// `an_account_only_pin_refuses_a_dir_whose_tokens_differ`,
/// `an_account_only_pin_refuses_a_dir_with_no_active_token`.
fn verify_token_selects(
    dir: &Path,
    login: &str,
    host: &str,
    probe: &dyn GhTokenProbe,
) -> Result<(), String> {
    let shown = dir.display();
    let active = probe
        .token(dir, host, None)
        .map_err(|e| format!("{shown}: `gh auth token --hostname {host}` failed ({e})"))?;
    let own = probe.token(dir, host, Some(login)).map_err(|e| {
        format!("{shown}: `gh auth token --hostname {host} -u {login}` failed ({e})")
    })?;
    if active != own {
        return Err(format!(
            "{shown}: gh's active {host} token is not '{login}''s own keyring token (it falls \
             back to another account's credential)"
        ));
    }
    Ok(())
}
