//! The launch-time login check for a pinned project (#9750, owner ruling D6e).
//!
//! Why: D6e takes the token out of a pinned session's environment. The
//! session's `gh` asks the keyring for it on every call, through the shim in
//! [`crate::core::gh_session_shim`]. Before the session starts, tm must still
//! prove that the keyring answers `--user <login>` with that login's own token:
//! on a keyring-backed host `gh auth token -u` was measured returning the
//! active account's token for another login (#5851).
//! What: [`verify_pinned_login`] asks each candidate config dir for
//! `gh auth token --hostname <host> --user <login>` and sends the answer to
//! `GET /user`, through #8510's seams ([`crate::core::gh_account_proof`]). The
//! first dir whose token authenticates as the login is returned; the token is
//! dropped. [`check_pin`] runs that check for a pin and words a refusal with
//! [`launch_refusal`], the one message a refused interactive launch prints.
//! [`session_pin_with`] finds a checkout's pin the way the daemon's spawn does.
//! Test: `gh_login_verify_tests.rs`.

use std::path::{Path, PathBuf};

use crate::core::gh_account::PinnedGhIdentity;
use crate::core::gh_account_dir::AccountDirSources;
use crate::core::gh_account_proof::{
    GhTokenProbe, GhUserCheck, api_base_url, origin_host, prove_one,
};
use crate::core::gh_org_accounts::{OrgAccounts, OrgAccountsError};
use crate::core::trusty_tools_config::TrustyToolsConfig;
use crate::session_manager::ssh_host_alias::SshHostAliases;

/// A pinned login the keyring was proven to answer for, and the config dir
/// whose `gh auth token --user` answered.
///
/// What: `host` is normalized ([`crate::core::gh_account_proof::normalize_gh_host`]);
/// `config_dir` is the dir the shim asks on every later call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VerifiedLogin {
    /// The pinned login, as the pin spells it.
    pub(crate) login: String,
    /// The normalized gh host the login was proven on.
    pub(crate) host: String,
    /// The config dir whose keyring lookup returned the login's own token.
    pub(crate) config_dir: PathBuf,
}

/// The gh host `origin` lives on, with an SSH host alias resolved.
///
/// What: [`crate::session_manager::worktree_reclaim_gh::proof_origin`] under
/// `aliases`, then [`origin_host`] — the host the daemon's spawn proves on.
/// Test: `a_pin_on_an_unresolved_ssh_alias_refuses_before_gh_runs`.
pub(crate) fn pinned_host(origin: &str, aliases: &SshHostAliases) -> Result<String, String> {
    crate::session_manager::worktree_reclaim_gh::proof_origin(origin, aliases)
        .and_then(|o| origin_host(&o))
}

/// Prove that one of `sources`' dirs yields `login`'s own token for `host`.
///
/// Why: the shim trusts the keyring per call, so the launch must prove the
/// keyring's answer once: a lookup that returns another account's token, or
/// nothing, must stop the launch (D6e: "launch verifies").
/// What: each candidate of [`AccountDirSources::candidates`] in order through
/// [`prove_one`] — the dir must exist and pass `refuse_unmigrated_config`
/// before `probe` runs, and `GET <api_base>/user` with the token must answer
/// `login` case-insensitively. The first dir that passes is returned and the
/// token is dropped unread. `Err` joins every candidate's reason; no reason
/// ever contains the token.
/// Test: `a_matching_login_is_verified_and_names_its_dir`,
/// `another_accounts_token_is_refused`, `an_empty_token_is_refused`,
/// `a_gh_timeout_is_refused`, `a_failed_user_check_is_refused`,
/// `no_refusal_ever_contains_the_token`.
pub(crate) fn verify_pinned_login(
    sources: &AccountDirSources,
    login: &str,
    host: &str,
    probe: &dyn GhTokenProbe,
    check: &dyn GhUserCheck,
) -> Result<VerifiedLogin, String> {
    let api_base = api_base_url(host);
    let mut reasons = Vec::new();
    for candidate in sources.candidates(login) {
        // #9750: the token proves the dir, then is dropped; only the dir is kept.
        let proven = candidate.and_then(|dir| {
            prove_one(&dir, login, host, &api_base, probe, check).map(|_token| dir)
        });
        match proven {
            Ok(config_dir) => {
                return Ok(VerifiedLogin {
                    login: login.to_string(),
                    host: host.to_string(),
                    config_dir,
                });
            }
            Err(reason) => reasons.push(reason),
        }
    }
    if reasons.is_empty() {
        reasons.push("tm has no gh config dir to ask".to_string());
    }
    Err(reasons.join("; "))
}

/// The message a refused interactive launch prints and exits 1 with (#9750).
///
/// Why: the plan of record fixes this text, so an operator reads the account,
/// the host, gh's own reason and the two commands that repair the keyring.
/// What: one line; `reason` is the verifier's, which never carries a token.
/// Test: `the_launch_refusal_is_the_plans_exact_message`.
pub fn launch_refusal(login: &str, host: &str, reason: &str) -> String {
    format!(
        "tm: cannot start session: gh account '{login}' is not logged in as that account on \
         {host} ({reason}). Run: gh auth login --hostname {host} (as {login}) or gh auth \
         refresh -h {host} -u {login}. No token was injected."
    )
}

/// The candidate config dirs a pin's login is looked up in.
///
/// Why: the daemon's spawn proof (#8510) and the launch check must ask the
/// same dirs in the same order, or one passes where the other refuses.
/// What: [`AccountDirSources::for_origin`] over `config`, with a pin's own
/// `config_dir` first (#8914 HIGH 3).
/// Test: `a_config_dir_pin_is_asked_before_any_other_dir`.
pub(crate) fn pin_sources(
    pinned: &PinnedGhIdentity,
    origin: &str,
    state_root: &Path,
    config: &TrustyToolsConfig,
    own_config_dir: Option<PathBuf>,
) -> AccountDirSources {
    let mut sources =
        AccountDirSources::for_origin(config, origin, state_root.to_path_buf(), own_config_dir);
    if let Some(dir) = &pinned.config_dir {
        sources.static_config_dir = Some(dir.clone());
    }
    sources
}

/// The login a pin runs as on `host`: its account, else its config dir's
/// `hosts.yml` user, as the daemon's spawn reads it.
/// Test: `a_config_dir_pin_naming_no_login_refuses`.
pub(crate) fn pin_login(pinned: &PinnedGhIdentity, host: &str) -> Option<String> {
    pinned
        .account
        .as_deref()
        .map(str::trim)
        .filter(|a| !a.is_empty())
        .map(str::to_string)
        .or_else(|| {
            let dir = pinned.config_dir.as_deref()?;
            crate::core::gh_session_account::hosts_yml_user(dir, host)
        })
}

/// What the launch check needs besides the pin: the dirs to ask and how.
pub(crate) struct VerifySeams<'a> {
    /// tm's state root, for the per-account dir candidate.
    pub(crate) state_root: &'a Path,
    /// `~/.ssh/config` host aliases.
    pub(crate) aliases: &'a SshHostAliases,
    /// The static config whose `projects[].github.config_dir` is a candidate.
    pub(crate) config: &'a TrustyToolsConfig,
    /// The operator's own gh config dir, the last candidate.
    pub(crate) own_config_dir: Option<PathBuf>,
    /// Asks gh for a candidate's token.
    pub(crate) probe: &'a dyn GhTokenProbe,
    /// Asks GitHub whose token it is.
    pub(crate) check: &'a dyn GhUserCheck,
}

/// Verify `pinned`'s login for a launch in a checkout whose origin is `origin`.
///
/// Why: Q1 of the #9750 plan — an interactive launch of a pinned project is
/// refused when the keyring cannot prove the login, instead of starting a
/// session whose every `gh` call fails.
/// What: the host from [`pinned_host`], the login from [`pin_login`], then
/// [`verify_pinned_login`] over [`pin_sources`]. `Ok(None)` for a pin that
/// names neither a login nor a config dir (nothing to verify, as the daemon
/// injects nothing for it). Every `Err` is a full message that ends "No token
/// was injected."; a failed verification is [`launch_refusal`].
/// Test: `a_verified_pin_passes_the_launch_check`,
/// `an_unverified_pin_is_refused_with_the_exact_message`,
/// `a_config_dir_pin_naming_no_login_refuses`,
/// `a_pin_on_an_unresolved_ssh_alias_refuses_before_gh_runs`.
pub(crate) fn check_pin(
    pinned: &PinnedGhIdentity,
    origin: &str,
    seams: &VerifySeams<'_>,
) -> Result<Option<VerifiedLogin>, String> {
    if pinned.account.is_none() && pinned.config_dir.is_none() {
        return Ok(None);
    }
    let host = pinned_host(origin, seams.aliases)
        .map_err(|e| format!("tm: cannot start session: {e}. No token was injected."))?;
    let Some(login) = pin_login(pinned, &host) else {
        return Err(no_login_refusal(pinned, Ok(&host)));
    };
    let sources = pin_sources(
        pinned,
        origin,
        seams.state_root,
        seams.config,
        seams.own_config_dir.clone(),
    );
    verify_pinned_login(&sources, &login, &host, seams.probe, seams.check)
        .map(Some)
        .map_err(|reason| launch_refusal(&login, &host, &reason))
}

/// The refusal for a pin that names no login tm can verify on `host` (#9750).
///
/// Why: the launch check ([`check_pin`]) and the in-place relaunch must refuse
/// the same pin with the same words.
/// What: `host`'s own error when it does not resolve; otherwise the pin's
/// config dir (or `<none>`) names no account for `host`.
/// Test: `a_config_dir_pin_naming_no_login_refuses`,
/// `a_blank_account_pin_refuses_the_relaunch_as_check_pin_does`.
pub(crate) fn no_login_refusal(pinned: &PinnedGhIdentity, host: Result<&str, &str>) -> String {
    match host {
        Err(e) => format!("tm: cannot start session: {e}. No token was injected."),
        Ok(host) => {
            let dir = pinned.config_dir.as_deref().unwrap_or(Path::new("<none>"));
            format!(
                "tm: cannot start session: gh config dir {} names no account for {host}, so \
                 tm cannot prove who the session runs as. No token was injected.",
                dir.display()
            )
        }
    }
}

/// A checkout's gh pin, found the way the daemon's spawn finds it.
///
/// Why: an interactive launch and an in-place relaunch run in the CLI, which
/// must decide "pinned or not" exactly as the daemon will, or one of them
/// verifies a login the other never uses.
/// What: the registry record for `origin` ([`crate::core::gh_account_registry::read_pin`]),
/// else the `[accounts]` org map ([`crate::core::gh_org_accounts::org_map_pin`]).
/// `Ok(None)` when neither pins a login or a config dir. An unreadable
/// registry, disagreeing records or a broken org map are an `Err`: the daemon
/// fails those closed, so a launch must not start on them.
/// Test: `an_unregistered_origin_is_unpinned`,
/// `a_registry_pin_is_found_for_its_origin`,
/// `an_org_mapped_origin_is_pinned_to_the_mapped_login`,
/// `a_token_env_only_record_is_unpinned`,
/// `an_unreadable_registry_or_broken_org_map_refuses_the_launch`.
pub(crate) fn session_pin_with(
    origin: &str,
    registry_dir: &Path,
    aliases: &SshHostAliases,
    load: impl FnOnce() -> Result<OrgAccounts, OrgAccountsError>,
) -> Result<Option<PinnedGhIdentity>, String> {
    // #9750: both `Err` arms below refuse; neither reads as unpinned.
    let pinned = match crate::core::gh_account_registry::read_pin(registry_dir, origin)? {
        Some(pin) => Some(crate::core::gh_account_registry::pinned_identity(&pin)),
        None => {
            crate::core::gh_org_accounts::org_map_pin(origin, aliases, load).map_err(|env| {
                env.warning
                    .unwrap_or_else(|| "the [accounts] table could not be read".to_string())
            })?
        }
    };
    Ok(pinned.filter(|p| p.account.is_some() || p.config_dir.is_some()))
}

/// The origin of `checkout`, when git reads one.
///
/// What: `Ok(None)` for a checkout with no `origin` or a non-repository —
/// neither is a pinned launch. #9750: a git failure (an unreadable `.git`, a
/// `safe.directory` refusal) is an `Err` refusal: tm cannot tell whether the
/// checkout is pinned, so it must not launch it as unpinned, just as an
/// unreadable registry refuses.
/// Test: `a_relaunch_whose_origin_git_cannot_read_is_refused`,
/// `a_launch_check_whose_origin_git_cannot_read_is_refused`.
pub(crate) fn checkout_origin(checkout: &Path) -> Result<Option<String>, String> {
    use crate::core::remote_mode::RemoteMode;
    match crate::core::remote_mode::remote_mode(checkout) {
        Ok(RemoteMode::Origin(origin)) => Ok(Some(origin)),
        Ok(RemoteMode::LocalOnly { .. } | RemoteMode::NotARepository) => Ok(None),
        Err(e) => Err(format!(
            "tm: cannot start session: git cannot read the origin of {} ({e}), so tm cannot \
             tell whether its gh account is pinned. No token was injected.",
            checkout.display()
        )),
    }
}

/// Production entry point: refuse an interactive launch in `checkout` whose
/// pinned gh login the keyring cannot prove (#9750, plan Q1).
///
/// Why: `tm sessions new` / `tm sessions start` are interactive; the daemon's
/// spawn never blocks (#3025), so the refusal must happen in the CLI first.
/// What: `account` (the `--account` flag) is an account-only pin; otherwise
/// [`session_pin_with`] over the host's registry and `[accounts]` table. Then
/// [`check_pin`] with the real `gh` and `GET /user`. `Ok(())` for an unpinned
/// checkout; an `Err` when git cannot read the origin ([`checkout_origin`]).
/// Blocking; run it off the async executor.
/// Test: `a_launch_check_whose_origin_git_cannot_read_is_refused`; the other
/// decisions are [`session_pin_with`]'s and [`check_pin`]'s.
pub fn check_launch_login(checkout: &Path, account: Option<&str>) -> Result<(), String> {
    use crate::core::gh_account_proof::{CliTokenProbe, HttpUserCheck};
    // #9750: a git error reading `origin` refuses the launch.
    let Some(origin) = checkout_origin(checkout)? else {
        return Ok(());
    };
    let aliases = SshHostAliases::for_current_user();
    let pinned = match account.map(str::trim).filter(|a| !a.is_empty()) {
        Some(login) => Some(PinnedGhIdentity {
            account: Some(login.to_string()),
            ..Default::default()
        }),
        None => session_pin_with(
            &origin,
            &crate::project::registry_data_dir(),
            &aliases,
            OrgAccounts::load_default,
        )
        .map_err(|e| format!("tm: cannot start session: {e}. No token was injected."))?,
    };
    let Some(pinned) = pinned else {
        return Ok(());
    };
    let state_root = crate::core::paths::FrameworkPaths::default().root;
    let config = TrustyToolsConfig::load();
    let seams = VerifySeams {
        state_root: &state_root,
        aliases: &aliases,
        config: &config,
        own_config_dir: crate::core::gh_account::gh_config_dir(),
        probe: &CliTokenProbe,
        check: &HttpUserCheck,
    };
    check_pin(&pinned, &origin, &seams).map(|_| ())
}

#[cfg(test)]
#[path = "gh_login_verify_tests.rs"]
mod gh_login_verify_tests;
