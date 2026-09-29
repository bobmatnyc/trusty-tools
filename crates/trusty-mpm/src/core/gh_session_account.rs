//! Run a session's `gh` and HTTPS git as the account `--account` names (#8914).
//!
//! Why: `tm sessions new --account X` spawned a session whose `gh api user`
//! answered with the machine's active account. Two defects combined. The CLI
//! parsed `--account` and dropped it. And tm's own per-account dir
//! (`<state_root>/gh-accounts/<login>`, #7166) holds no token: `gh` run under it
//! reads the keyring's active-account entry, which is machine-global, so
//! `GH_CONFIG_DIR` alone never selected the account.
//! What: [`prepare_session_account`] is the pre-spawn gate the CLI runs. It
//! builds or reuses the per-account dir, refuses a dir it cannot trust, and
//! proves a token for the login with `GET /user`
//! ([`crate::core::gh_account_proof`]). Every refusal names the one-time setup
//! command and never falls back to the active account. [`session_spawn_env`] is
//! the daemon-side env builder: a registry pin to tm's own account dir injects
//! the proven token, `GH_CONFIG_DIR` pointing at that dir, and a git credential
//! helper pin, so HTTPS git asks `gh` and `gh` answers with that token.
//! Test: `gh_session_account_tests.rs`, and
//! `a_tm_account_dir_pin_never_leaves_the_session_on_the_active_account`.

use std::path::{Path, PathBuf};

use crate::core::gh_account::{GH_USER_ENV_VAR, GhSpawnEnv, PinnedGhIdentity};
use crate::core::gh_account_dir::{AccountDirSources, GH_ACCOUNTS_DIR_NAME, tm_account_dir};
use crate::core::gh_account_proof::{
    CliTokenProbe, GhTokenProbe, GhUserCheck, HttpUserCheck, ProvenToken, identity_token_vars,
    origin_host, prove_account_token,
};

/// The `gh` config dir variable.
const GH_CONFIG_DIR_ENV_VAR: &str = "GH_CONFIG_DIR";

/// The git credential helper a pinned session uses: `gh` itself, which returns
/// the session's `GH_TOKEN` to git.
pub(crate) const GH_CREDENTIAL_HELPER: &str = "!gh auth git-credential";

/// The one-time command that stores `dir`'s account credential inside `dir`.
///
/// Why: `--insecure-storage` keeps the token in `<dir>/hosts.yml` (mode 0600,
/// dir 0700). A keyring login also rewrites the keyring's active-account entry,
/// which every other `gh` process on the machine reads.
/// Test: `an_unknown_login_is_refused_with_the_setup_command`.
pub(crate) fn one_time_setup_command(dir: &Path, host: &str) -> String {
    format!(
        "GH_CONFIG_DIR={} gh auth login --hostname {host} --insecure-storage",
        dir.display()
    )
}

/// The refusal text for `login`: the reason, the no-fallback statement, the fix.
fn refusal(login: &str, dir: &Path, host: &str, reason: &str) -> String {
    format!(
        "cannot run this session as gh account '{login}': {reason}. tm does not fall back to \
         this machine's active gh account (#8914). One-time setup, in your own terminal: `{}`, \
         then retry.",
        one_time_setup_command(dir, host)
    )
}

/// Production entry point of [`prepare_session_account_with`]: tm's state root,
/// the operator's own `gh` config dir, the real `gh` and the real `GET /user`.
///
/// Why: the one call `tm sessions new --account`, `tm launch --account` and
/// `tm <url> --account` make before a session exists.
/// What: blocking (a `gh` subprocess and an HTTPS call); run it off the async
/// executor.
/// Test: none directly (real `gh` and network); the decisions are
/// [`prepare_session_account_with`]'s.
pub fn prepare_session_account(login: &str, origin: &str) -> Result<PathBuf, String> {
    let state_root = crate::core::paths::FrameworkPaths::default().root;
    let operator_dir = crate::core::gh_account::gh_config_dir()
        .unwrap_or_else(|| PathBuf::from(".config").join("gh"));
    prepare_session_account_with(
        &state_root,
        &operator_dir,
        login,
        origin,
        &CliTokenProbe,
        &HttpUserCheck,
    )
}

/// Build or reuse `login`'s per-account `gh` dir and prove a token for it.
///
/// Why: a session started with `--account` must run as that account or not
/// start. Starting it with no identity is the #8914 defect.
/// What: refuses an existing dir that cannot be listed (never repaired), then
/// builds or reuses the dir through
/// [`crate::daemon::managed_routes::inproject::ensure_account_config_dir`].
/// The dir's `hosts.yml` must parse and name `login`. The dir is set to 0700
/// and its `hosts.yml`/`config.yml` to 0600. Last, a token for `login` from
/// the dir or from `operator_gh_config_dir` must pass `GET /user` on the
/// origin's host. Returns the dir. Every `Err` is [`refusal`] text.
/// Test: `a_logged_in_login_gets_a_private_proven_dir`,
/// `an_unknown_login_is_refused_with_the_setup_command`,
/// `a_token_for_another_account_is_refused_not_used`,
/// `an_unreadable_account_dir_is_refused`,
/// `a_malformed_hosts_yml_is_refused`,
/// `a_missing_hosts_yml_for_an_unknown_login_is_refused`,
/// `a_malformed_config_yml_is_refused`,
/// `a_reused_dir_is_made_private`.
pub(crate) fn prepare_session_account_with(
    state_root: &Path,
    operator_gh_config_dir: &Path,
    login: &str,
    origin: &str,
    probe: &dyn GhTokenProbe,
    check: &dyn GhUserCheck,
) -> Result<PathBuf, String> {
    let host = origin_host(origin).map_err(|e| format!("cannot pin gh account '{login}': {e}"))?;
    let dir = tm_account_dir(state_root, login)?;
    let refuse = |reason: String| refusal(login, &dir, &host, &reason);
    // #8914: an unlistable dir is refused, never chmod-ed back into use.
    if dir.symlink_metadata().is_ok() {
        std::fs::read_dir(&dir)
            .map_err(|e| refuse(format!("{} is unreadable ({e})", dir.display())))?;
    }
    crate::daemon::managed_routes::inproject::ensure_account_config_dir(
        state_root,
        operator_gh_config_dir,
        login,
    )
    .map_err(refuse)?;
    require_hosts_yml_names(&dir, login).map_err(refuse)?;
    make_private(&dir).map_err(refuse)?;
    let sources = AccountDirSources {
        static_config_dir: None,
        state_root: Some(state_root.to_path_buf()),
        own_config_dir: Some(operator_gh_config_dir.to_path_buf()),
    };
    prove_account_token(&sources, login, origin, probe, check).map_err(|reasons| {
        refuse(format!(
            "no gh token is proven to be its own ({})",
            reasons.join("; ")
        ))
    })?;
    Ok(dir)
}

/// `Ok` when `<dir>/hosts.yml` reads, parses, and names `login`.
fn require_hosts_yml_names(dir: &Path, login: &str) -> Result<(), String> {
    let path = dir.join("hosts.yml");
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("{} is unreadable ({e})", path.display()))?;
    let status = crate::core::gh_account::parse_gh_account_status_from_hosts_yml(&text)
        .ok_or_else(|| format!("{} names no github.com account", path.display()))?;
    status
        .canonical_logged_in_login(login)
        .map(|_| ())
        .ok_or_else(|| format!("{} does not name '{login}'", path.display()))
}

/// Set `dir` to 0700 and its `hosts.yml`/`config.yml`, when present, to 0600.
///
/// Why: a dir the operator's own `gh auth login` created keeps gh's modes; the
/// dir holds a token after an `--insecure-storage` login.
#[cfg(unix)]
fn make_private(dir: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let set = |path: &Path, mode: u32| {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
            .map_err(|e| format!("cannot set permissions on {} ({e})", path.display()))
    };
    set(dir, 0o700)?;
    for file in ["hosts.yml", "config.yml"] {
        let path = dir.join(file);
        if path.is_file() {
            set(&path, 0o600)?;
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn make_private(_dir: &Path) -> Result<(), String> {
    Ok(())
}

/// `true` when `dir` is one of tm's own `<state_root>/gh-accounts/<login>` dirs.
fn is_tm_account_dir(dir: &Path, state_root: &Path) -> bool {
    dir.parent() == Some(state_root.join(GH_ACCOUNTS_DIR_NAME).as_path())
}

/// Git config, by environment, that makes `gh` the only credential helper for
/// `host` (#8914).
///
/// Why: git asks helpers in config order, so an earlier helper (osxkeychain)
/// could answer with another account's stored password. An empty value clears
/// the list; `GIT_CONFIG_*` entries are read after every config file.
/// What: `GIT_CONFIG_COUNT=2`, then `credential.https://<host>.helper` set to
/// empty and then to [`GH_CREDENTIAL_HELPER`].
/// Test: `a_proven_tm_account_dir_pin_isolates_gh_and_git`.
pub(crate) fn git_credential_pin(host: &str) -> Vec<(String, String)> {
    let key = format!("credential.https://{host}.helper");
    [
        ("GIT_CONFIG_COUNT", "2".to_string()),
        ("GIT_CONFIG_KEY_0", key.clone()),
        ("GIT_CONFIG_VALUE_0", String::new()),
        ("GIT_CONFIG_KEY_1", key),
        ("GIT_CONFIG_VALUE_1", GH_CREDENTIAL_HELPER.to_string()),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect()
}

/// The spawn env for a registry pin, with tm's own account dirs proven (#8914).
///
/// Why: tm's per-account dir carries no token, so a spawn that set only
/// `GH_CONFIG_DIR` to it ran as the keyring's active account.
/// What: a pin whose `config_dir` is tm's own dir and that names an account
/// gets the token `prove` returns in the token variable for its host class,
/// `GH_USER`, `GH_CONFIG_DIR=<dir>`, and [`git_credential_pin`]. When no token
/// is proven it gets the nobody-token in both token variables and a warning
/// naming [`one_time_setup_command`], so its `gh` and HTTPS git fail rather
/// than act as the active account. Every other pin is
/// [`crate::core::gh_account::pinned_spawn_env`], unchanged.
/// Test: `a_proven_tm_account_dir_pin_isolates_gh_and_git`,
/// `an_unproven_tm_account_dir_pin_fails_closed`,
/// `an_operator_config_dir_pin_is_unchanged`.
pub(crate) fn session_spawn_env(
    pinned: &PinnedGhIdentity,
    origin: &str,
    state_root: &Path,
    prove: impl FnOnce(&str) -> Result<ProvenToken, String>,
) -> Option<Result<GhSpawnEnv, String>> {
    let login = pinned
        .account
        .as_deref()
        .map(str::trim)
        .filter(|a| !a.is_empty());
    let (Some(dir), Some(login)) = (pinned.config_dir.as_deref(), login) else {
        return crate::core::gh_account::pinned_spawn_env(pinned, origin, prove);
    };
    if !is_tm_account_dir(dir, state_root) {
        return crate::core::gh_account::pinned_spawn_env(pinned, origin, prove);
    }
    let host = origin_host(origin).unwrap_or_else(|_| "github.com".to_string());
    let (mut vars, warning) = match prove(login) {
        Ok(proven) => (proven.identity_vars(), None),
        Err(reason) => (
            identity_token_vars(None),
            Some(refusal(login, dir, &host, &reason)),
        ),
    };
    vars.push((GH_USER_ENV_VAR.to_string(), login.to_string()));
    vars.push((GH_CONFIG_DIR_ENV_VAR.to_string(), dir.display().to_string()));
    vars.extend(git_credential_pin(&host));
    Some(Ok(GhSpawnEnv { vars, warning }))
}

#[cfg(test)]
#[path = "gh_session_account_tests.rs"]
mod gh_session_account_tests;
