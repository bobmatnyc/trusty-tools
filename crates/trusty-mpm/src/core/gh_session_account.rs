//! Run a session's `gh` and HTTPS git as the account `--account` names (#8914).
//!
//! Why: `tm sessions new --account X` spawned a session whose `gh api user`
//! answered with the machine's active account. Two defects combined. The CLI
//! parsed `--account` and dropped it. And tm's own per-account dir
//! (`<state_root>/gh-accounts/<login>`, #7166) held no token: `gh` run under it
//! reads the keyring's active-account entry, which is machine-global, so
//! `GH_CONFIG_DIR` alone never selected the account.
//! What: [`prepare_session_account`] is the pre-spawn gate the CLI runs. It
//! builds or reuses the per-account dir, refuses a dir it cannot trust, proves
//! a token for the login with `GET /user` ([`crate::core::gh_account_proof`]),
//! and writes that token into the dir's 0600 `hosts.yml` itself. It never runs
//! or suggests `gh auth login`: gh's login activates the account in the
//! machine-wide keyring slot. [`session_spawn_env`] is the daemon-side env
//! builder: every config-dir pin gets a proven token, `GH_CONFIG_DIR` and a git
//! credential helper pin, or the nobody-token.
//! Test: `gh_session_account_tests.rs`, and
//! `a_tm_account_dir_pin_never_leaves_the_session_on_the_active_account`.

use std::path::{Path, PathBuf};

use crate::core::gh_account::{GH_USER_ENV_VAR, GhSpawnEnv, PinnedGhIdentity};
use crate::core::gh_account_dir::{
    AccountDirSources, GH_ACCOUNTS_DIR_NAME, ensure_config_version, tm_account_dir,
};
use crate::core::gh_account_proof::{
    CliTokenProbe, GhTokenProbe, GhUserCheck, HttpUserCheck, ProvenToken, api_base_url,
    identity_token_vars, origin_host, prove_account_token,
};
use crate::session_manager::ssh_host_alias::SshHostAliases;

/// The `gh` config dir variable.
const GH_CONFIG_DIR_ENV_VAR: &str = "GH_CONFIG_DIR";

/// The git credential helper a pinned session uses: `gh` itself, which returns
/// the session's `GH_TOKEN` to git.
pub(crate) const GH_CREDENTIAL_HELPER: &str = "!gh auth git-credential";

/// The one-time fix every refusal names (#8914 HIGH 1).
///
/// Why: `gh auth login`, even with `GH_CONFIG_DIR` and `--insecure-storage`,
/// activates the account in the machine-wide keyring slot, so tm never names
/// it. A token on stdin touches no keyring.
/// What: rerun with `--account-token-stdin`; tm proves the token with
/// `GET /user` and stores it in `<dir>/hosts.yml`.
/// Test: `no_refusal_names_a_gh_auth_login_command`.
pub(crate) fn setup_hint(login: &str, dir: &Path) -> String {
    format!(
        "rerun the tm command with `--account {login} --account-token-stdin` and a token that \
         authenticates as '{login}' on stdin (`... < token-file`); tm proves it with `GET /user` \
         and stores it in {}/hosts.yml, mode 0600",
        dir.display()
    )
}

/// The refusal text for `login`: the reason, the no-fallback statement, the fix.
fn refusal(login: &str, dir: &Path, reason: &str) -> String {
    format!(
        "cannot run this session as gh account '{login}': {reason}. tm does not fall back to \
         this machine's active gh account (#8914). One-time setup: {}.",
        setup_hint(login, dir)
    )
}

/// Production entry point: tm's state root, the operator's own `gh` config
/// dir, `~/.ssh/config`, the real `gh` and the real `GET /user`.
///
/// Why: the one call `tm sessions new --account`, `tm launch --account` and
/// `tm <url> --account` make before a session exists.
/// What: resolves `origin` through [`session_origin`]; stores `stdin_token`
/// first when given ([`store_supplied_token`]); then
/// [`prepare_session_account_with`]. Blocking (a `gh` subprocess and HTTPS
/// calls); run it off the async executor.
/// Test: none directly (real `gh` and network); the decisions are
/// [`session_origin`]'s, [`store_supplied_token`]'s and
/// [`prepare_session_account_with`]'s.
pub fn prepare_session_account(
    login: &str,
    origin: &str,
    stdin_token: Option<&str>,
) -> Result<PathBuf, String> {
    let state_root = crate::core::paths::FrameworkPaths::default().root;
    let operator_dir = crate::core::gh_account::gh_config_dir()
        .unwrap_or_else(|| PathBuf::from(".config").join("gh"));
    let origin = session_origin(login, origin, &SshHostAliases::for_current_user())?;
    if let Some(token) = stdin_token {
        store_supplied_token(&state_root, login, &origin, token, &HttpUserCheck)?;
    }
    prepare_session_account_with(
        &state_root,
        &operator_dir,
        login,
        &origin,
        &StoredTokenFirst(&CliTokenProbe),
        &HttpUserCheck,
    )
}

/// `origin` as the URL a proof runs against, with an SSH host alias resolved
/// through `aliases` (#8914 MEDIUM).
///
/// Why: `git@gh-work:acme/x` names the alias `gh-work`; proving on it would
/// send the token to `https://gh-work/api/v3`. The daemon's spawn proof
/// resolves the same alias ([`crate::core::gh_account::spawn_proof`]).
/// What: [`crate::session_manager::worktree_reclaim_gh::proof_origin`]; an
/// alias no `~/.ssh/config` entry renames is a refusal.
/// Test: `an_ssh_alias_origin_is_proven_on_its_real_host`,
/// `an_unresolved_ssh_alias_is_refused`.
pub(crate) fn session_origin(
    login: &str,
    origin: &str,
    aliases: &SshHostAliases,
) -> Result<String, String> {
    crate::session_manager::worktree_reclaim_gh::proof_origin(origin, aliases).map_err(|e| {
        format!(
            "cannot run this session as gh account '{login}': {e}. tm does not fall back to \
             this machine's active gh account (#8914)."
        )
    })
}

/// Build or reuse `login`'s per-account `gh` dir, prove a token for it, and
/// store that token in the dir's `hosts.yml`.
///
/// Why: a session started with `--account` must run as that account or not
/// start. Starting it with no identity is the #8914 defect.
/// What: refuses an existing dir that cannot be listed (never repaired), and a
/// login neither the dir nor the operator's `hosts.yml` names. Builds or reuses
/// the dir through
/// [`crate::daemon::managed_routes::inproject::ensure_account_config_dir`];
/// its `hosts.yml` must parse and name `login`. The dir is set to 0700 and its
/// `hosts.yml`/`config.yml` to 0600. A token for `login` from the dir or from
/// `operator_gh_config_dir` must pass `GET /user` on the origin's host; it is
/// then written as `users.<login>.oauth_token` ([`store_token`]). Returns the
/// dir. Every `Err` is [`refusal`] text.
/// Test: `a_logged_in_login_gets_a_private_proven_dir`,
/// `a_proven_token_is_stored_in_the_private_hosts_yml`,
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
    origin_host(origin).map_err(|e| format!("cannot pin gh account '{login}': {e}"))?;
    let dir = tm_account_dir(state_root, login)?;
    let refuse = |reason: String| refusal(login, &dir, &reason);
    refuse_unlistable(&dir).map_err(refuse)?;
    // #8914: refused here, before the dir builder's own text can name a
    // `gh auth login`/`switch` that moves the machine's active account.
    if !dir.join("hosts.yml").is_file() && !hosts_yml_names(operator_gh_config_dir, login) {
        return Err(refuse(format!(
            "'{login}' is not logged into gh in {}/hosts.yml, and no token for it was given on \
             stdin",
            operator_gh_config_dir.display()
        )));
    }
    crate::daemon::managed_routes::inproject::ensure_account_config_dir(
        state_root,
        operator_gh_config_dir,
        login,
    )
    .map_err(refuse)?;
    let canonical = require_hosts_yml_names(&dir, login).map_err(refuse)?;
    make_private(&dir).map_err(refuse)?;
    let sources = AccountDirSources {
        static_config_dir: None,
        state_root: Some(state_root.to_path_buf()),
        own_config_dir: Some(operator_gh_config_dir.to_path_buf()),
    };
    let proven = prove_account_token(&sources, login, origin, probe, check).map_err(|reasons| {
        refuse(format!(
            "no gh token is proven to be its own ({})",
            reasons.join("; ")
        ))
    })?;
    // #8914 HIGH 1: tm stores the proven token itself; no `gh auth login`.
    store_token(&dir, &proven.host, &canonical, proven.secret()).map_err(refuse)?;
    Ok(dir)
}

/// Prove a token the operator piped on stdin and store it in `login`'s
/// per-account dir (#8914 HIGH 1).
///
/// Why: when the operator's own gh cannot yield `login`'s token (the #5851
/// keyring shape), stdin is the setup path that touches no keyring.
/// What: the token must be non-empty and `GET /user` on the origin's host must
/// answer `login`, BEFORE anything is written. Then the dir is created 0700,
/// its `config.yml` declares `version: "1"`, and the token is written with
/// [`store_token`]. Every `Err` is [`refusal`] text; a refused token writes
/// nothing. Returns the dir.
/// Test: `a_stdin_token_is_proven_then_stored`,
/// `a_stdin_token_for_another_account_is_refused_and_not_stored`,
/// `a_stdin_token_whose_proof_fails_is_refused_and_not_stored`,
/// `an_empty_stdin_token_is_refused`.
pub(crate) fn store_supplied_token(
    state_root: &Path,
    login: &str,
    origin: &str,
    token: &str,
    check: &dyn GhUserCheck,
) -> Result<PathBuf, String> {
    let host = origin_host(origin).map_err(|e| format!("cannot pin gh account '{login}': {e}"))?;
    let dir = tm_account_dir(state_root, login)?;
    let refuse = |reason: String| refusal(login, &dir, &reason);
    let token = token.trim();
    if token.is_empty() {
        return Err(refuse("the token on stdin is empty".to_string()));
    }
    let actual = check
        .login(&api_base_url(&host), token)
        .map_err(|e| refuse(format!("the token on stdin could not be proven ({e})")))?;
    if !actual.eq_ignore_ascii_case(login) {
        return Err(refuse(format!(
            "the token on stdin authenticates as '{actual}'"
        )));
    }
    refuse_unlistable(&dir).map_err(refuse)?;
    std::fs::create_dir_all(&dir)
        .map_err(|e| refuse(format!("cannot create {} ({e})", dir.display())))?;
    make_private(&dir).map_err(refuse)?;
    ensure_config_version(&dir).map_err(refuse)?;
    store_token(&dir, &host, &actual, token).map_err(refuse)?;
    make_private(&dir).map_err(refuse)?;
    Ok(dir)
}

/// `Err` when `dir` exists but cannot be listed; a missing dir is `Ok`.
fn refuse_unlistable(dir: &Path) -> Result<(), String> {
    if dir.symlink_metadata().is_ok() {
        std::fs::read_dir(dir).map_err(|e| format!("{} is unreadable ({e})", dir.display()))?;
    }
    Ok(())
}

/// `true` when `<dir>/hosts.yml` reads, parses, and names `login`.
fn hosts_yml_names(dir: &Path, login: &str) -> bool {
    require_hosts_yml_names(dir, login).is_ok()
}

/// The login `<dir>/hosts.yml` names, in its written case, or why not.
fn require_hosts_yml_names(dir: &Path, login: &str) -> Result<String, String> {
    let path = dir.join("hosts.yml");
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("{} is unreadable ({e})", path.display()))?;
    let status = crate::core::gh_account::parse_gh_account_status_from_hosts_yml(&text)
        .ok_or_else(|| format!("{} names no github.com account", path.display()))?;
    status
        .canonical_logged_in_login(login)
        .ok_or_else(|| format!("{} does not name '{login}'", path.display()))
}

/// Write `token` into `<dir>/hosts.yml` as gh's insecure-storage layout for
/// `host`: `users.<login>.oauth_token`, plus `user` and `oauth_token` (#8914).
///
/// Why: tm's per-account dir must hold its account's own credential, or gh
/// under it reads the machine-wide keyring slot.
/// What: every other key is kept. A file already holding exactly this is not
/// rewritten. The write is atomic ([`trusty_common::atomic_file::write_atomic`])
/// and the file ends 0600. A file that is not a YAML mapping is refused, never
/// replaced.
/// Test: `a_proven_token_is_stored_in_the_private_hosts_yml`,
/// `a_hosts_yml_that_is_not_a_mapping_is_refused_and_kept`.
fn store_token(dir: &Path, host: &str, login: &str, token: &str) -> Result<(), String> {
    use serde_yaml::{Mapping, Value};
    let path = dir.join("hosts.yml");
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("{} is unreadable ({e})", path.display())),
    };
    let original: Value = if text.trim().is_empty() {
        Value::Mapping(Mapping::new())
    } else {
        serde_yaml::from_str(&text).map_err(|_| format!("{} is not YAML", path.display()))?
    };
    let mut doc = original.clone();
    let not_a_map = || format!("{} is not a gh hosts.yml mapping", path.display());
    let host_map = mapping_at(&mut doc, host).ok_or_else(not_a_map)?;
    let users = mapping_at_value(host_map, "users").ok_or_else(not_a_map)?;
    mapping_at_value(users, login)
        .ok_or_else(not_a_map)?
        .insert("oauth_token".into(), token.into());
    host_map.insert("user".into(), login.into());
    host_map.insert("oauth_token".into(), token.into());
    if doc != original {
        let rendered = serde_yaml::to_string(&doc)
            .map_err(|e| format!("cannot render {} ({e})", path.display()))?;
        trusty_common::atomic_file::write_atomic(&path, rendered.as_bytes())
            .map_err(|e| format!("cannot write {} ({e})", path.display()))?;
    }
    make_private(dir)
}

/// The mapping at top-level key `key` of `doc`, created when absent or null.
fn mapping_at<'a>(
    doc: &'a mut serde_yaml::Value,
    key: &str,
) -> Option<&'a mut serde_yaml::Mapping> {
    mapping_at_value(doc.as_mapping_mut()?, key)
}

/// The mapping under `key` in `map`, created when absent or null.
fn mapping_at_value<'a>(
    map: &'a mut serde_yaml::Mapping,
    key: &str,
) -> Option<&'a mut serde_yaml::Mapping> {
    let slot = map.entry(key.into()).or_insert(serde_yaml::Value::Null);
    if slot.is_null() {
        *slot = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
    }
    slot.as_mapping_mut()
}

/// Set `dir` to 0700 and its `hosts.yml`/`config.yml`, when present, to 0600.
///
/// Why: the dir holds a token (#8914), and a dir the operator's own
/// `gh auth login` created keeps gh's modes.
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

/// The token `<dir>/hosts.yml` stores for `login` on `host`, if any (#8914).
///
/// Why: `gh auth token -u <login>` reads the keyring BEFORE `hosts.yml`, so a
/// stale keyring slot would shadow the token tm stored and proved.
/// What: `<host>.users.<login>.oauth_token` (login matched case-insensitively),
/// non-empty; `None` for a missing, unreadable or unparsable file.
/// Test: `a_stored_token_is_read_before_gh_is_asked`.
fn stored_token(dir: &Path, host: &str, login: &str) -> Option<String> {
    let text = std::fs::read_to_string(dir.join("hosts.yml")).ok()?;
    let doc: serde_yaml::Value = serde_yaml::from_str(&text).ok()?;
    let users = doc.get(host)?.get("users")?.as_mapping()?;
    let token = users
        .iter()
        .find(|(k, _)| k.as_str().is_some_and(|k| k.eq_ignore_ascii_case(login)))?
        .1
        .get("oauth_token")?
        .as_str()?
        .trim();
    (!token.is_empty()).then(|| token.to_string())
}

/// A [`GhTokenProbe`] that answers from a dir's stored token first and asks
/// `inner` only when there is none (#8914).
///
/// Why: see [`stored_token`]. Every answer is still proven by `GET /user`.
/// Test: `a_stored_token_is_read_before_gh_is_asked`.
pub(crate) struct StoredTokenFirst<'a>(pub(crate) &'a dyn GhTokenProbe);

impl GhTokenProbe for StoredTokenFirst<'_> {
    fn token(&self, dir: &Path, host: &str, login: &str) -> Result<String, String> {
        match stored_token(dir, host, login) {
            Some(token) => Ok(token),
            None => self.0.token(dir, host, login),
        }
    }
}

/// The login `<dir>/hosts.yml` names as `host`'s active user, if any.
fn hosts_yml_user(dir: &Path, host: &str) -> Option<String> {
    let text = std::fs::read_to_string(dir.join("hosts.yml")).ok()?;
    let doc: serde_yaml::Value = serde_yaml::from_str(&text).ok()?;
    let user = doc.get(host)?.get("user")?.as_str()?.trim();
    (!user.is_empty()).then(|| user.to_string())
}

/// The spawn env for a registry pin, with every config-dir pin proven (#8914).
///
/// Why: a config dir with no token of its own makes gh read the machine-wide
/// keyring slot, so a spawn that set only `GH_CONFIG_DIR` could run as the
/// active account. That held for tm's own account dirs and for an operator's
/// `--gh-config-dir` alike (#8914 HIGH 3).
/// What: a pin with no `config_dir` is
/// [`crate::core::gh_account::pinned_spawn_env`]'s. A config-dir pin's login
/// is its `account`, else the dir's `hosts.yml` user for the origin's host
/// (resolved through `aliases`). A login `prove` proves gets its token in the
/// token variable for its host class; every failure — no host, no login, no
/// proven token — gets the nobody-token in both token variables and a warning
/// naming [`setup_hint`]. Every config-dir pin also gets `GH_USER` (when a
/// login is known), `GH_CONFIG_DIR=<dir>`, and, when the host is known,
/// [`git_credential_pin`].
/// Test: `a_proven_tm_account_dir_pin_isolates_gh_and_git`,
/// `an_unproven_tm_account_dir_pin_fails_closed`,
/// `an_operator_config_dir_pin_is_proven_and_injected`,
/// `an_operator_config_dir_pin_without_a_proven_token_fails_closed`,
/// `a_config_dir_pin_naming_no_login_fails_closed`,
/// `a_config_dir_pin_whose_origin_names_no_host_fails_closed`.
pub(crate) fn session_spawn_env(
    pinned: &PinnedGhIdentity,
    origin: &str,
    state_root: &Path,
    aliases: &SshHostAliases,
    prove: impl FnOnce(&str) -> Result<ProvenToken, String>,
) -> Option<Result<GhSpawnEnv, String>> {
    let Some(dir) = pinned.config_dir.as_deref() else {
        return crate::core::gh_account::pinned_spawn_env(pinned, origin, prove);
    };
    // #8914 LOW: no host is a refusal, never a guessed `github.com`.
    let host = crate::session_manager::worktree_reclaim_gh::proof_origin(origin, aliases)
        .and_then(|o| origin_host(&o));
    let login = pinned
        .account
        .as_deref()
        .map(str::trim)
        .filter(|a| !a.is_empty())
        .map(str::to_string)
        .or_else(|| host.as_ref().ok().and_then(|h| hosts_yml_user(dir, h)));
    let proven = match (&host, &login) {
        (Err(e), _) => Err(e.clone()),
        (Ok(h), None) => Err(format!(
            "gh config dir {} names no account for {h}, so tm cannot prove who it is",
            dir.display()
        )),
        (Ok(_), Some(login)) => prove(login),
    };
    let (mut vars, warning) = match proven {
        Ok(proven) => (proven.identity_vars(), None),
        Err(reason) => {
            let who = login.as_deref().unwrap_or("<login>");
            let store = tm_account_dir(state_root, who)
                .unwrap_or_else(|_| state_root.join(GH_ACCOUNTS_DIR_NAME).join("<login>"));
            (
                identity_token_vars(None),
                Some(refusal(who, &store, &reason)),
            )
        }
    };
    if let Some(login) = &login {
        vars.push((GH_USER_ENV_VAR.to_string(), login.clone()));
    }
    vars.push((GH_CONFIG_DIR_ENV_VAR.to_string(), dir.display().to_string()));
    if let Ok(host) = &host {
        vars.extend(git_credential_pin(host));
    }
    Some(Ok(GhSpawnEnv { vars, warning }))
}

#[cfg(test)]
#[path = "gh_session_account_tests.rs"]
mod gh_session_account_tests;
