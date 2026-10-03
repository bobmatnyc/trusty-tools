//! `tm daemon --sandbox`: the isolation contract for a live-check daemon (#9121).
//!
//! Why: a qa live-check started a "sandbox" daemon with its own `$HOME` and
//! port. It inherited `TELEGRAM_BOT_TOKEN` and polled the real bot
//! (`TerminatedByOtherGetUpdates`). A fresh `$HOME` is not isolation: the
//! credential store and the Keychain belong to the OS user, not to `$HOME`.
//! What: [`enter`] refuses unless the environment carries no secret-shaped
//! variable, `TRUSTY_DATA_DIR_OVERRIDE` is set, and `$HOME` is not the
//! account's home. On success it latches
//! `trusty_mpm::secret_source::enter_sandbox`, so no credential tier is read.
//! [`gate_channel_pollers`] keeps the Telegram bot down. Every check fails
//! closed: an answer that cannot be determined refuses.
//! Test: `daemon_sandbox_tests.rs`; end to end, `tests/sandbox_daemon_9121.rs`.

use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};

use trusty_common::DATA_DIR_OVERRIDE_ENV;
use trusty_common::credential_registry::is_registered_credential_env_var;

/// Name suffixes that mark a variable as a secret, compared case-insensitively.
///
/// Why: `_TOKEN` and `_KEY` are the #9121 contract. `_TOKEN_FILE` and
/// `_KEY_FILE` name files the daemon reads credentials from
/// (`TRUSTY_BUGREPORT_TOKEN_FILE`, `TRUSTY_BUGREPORT_GH_APP_KEY_FILE`), and
/// `_SECRET` / `_PASSWORD` are the same thing spelled differently.
const SECRET_SUFFIXES: &[&str] = &[
    "_TOKEN",
    "_KEY",
    "_TOKEN_FILE",
    "_KEY_FILE",
    "_SECRET",
    "_PASSWORD",
];

/// The parts of the environment the sandbox contract reads. Holds no value of
/// any secret-shaped variable — only names.
#[derive(Debug, Clone, Default)]
pub(crate) struct SandboxEnv {
    /// Secret-shaped variable NAMES present, sorted.
    pub(crate) secret_names: Vec<String>,
    /// `TRUSTY_DATA_DIR_OVERRIDE`, when set.
    pub(crate) data_dir_override: Option<OsString>,
    /// `$HOME`, when set.
    pub(crate) home: Option<PathBuf>,
    /// The account's home from the password database, when it resolves.
    pub(crate) account_home: Option<PathBuf>,
}

impl SandboxEnv {
    /// Read this process's environment. Values of other variables are dropped
    /// at the iterator, before anything stores them.
    fn capture() -> Self {
        Self {
            secret_names: secret_shaped_names(std::env::vars_os().map(|(name, _)| name)),
            data_dir_override: std::env::var_os(DATA_DIR_OVERRIDE_ENV),
            home: std::env::var_os("HOME").map(PathBuf::from),
            account_home: account_home(),
        }
    }
}

/// One reason a sandbox daemon refuses to start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Refusal {
    /// Secret-shaped variables are present; carries their names only.
    SecretEnv(Vec<String>),
    /// `TRUSTY_DATA_DIR_OVERRIDE` is unset or empty.
    NoDataDirOverride,
    /// `$HOME` is unset or empty.
    NoHome,
    /// `$HOME` does not resolve to an existing directory.
    HomeUnresolvable(PathBuf),
    /// The password database names no home for this user.
    AccountHomeUnknown,
    /// `$HOME` resolves to the account's real home.
    HomeIsAccountHome(PathBuf),
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SecretEnv(names) => write!(
                f,
                "the environment carries secret-shaped variables (values not shown): {}",
                names.join(", ")
            ),
            Self::NoDataDirOverride => write!(f, "{DATA_DIR_OVERRIDE_ENV} is not set"),
            Self::NoHome => write!(f, "$HOME is not set"),
            Self::HomeUnresolvable(home) => {
                write!(f, "$HOME ({}) is not an existing directory", home.display())
            }
            Self::AccountHomeUnknown => write!(
                f,
                "the password database names no home for this user, so $HOME cannot be \
                 shown to differ from it"
            ),
            Self::HomeIsAccountHome(home) => write!(
                f,
                "$HOME ({}) is this user's real home; a sandbox needs its own",
                home.display()
            ),
        }
    }
}

/// Validate the environment and, only when it is isolated, enter sandbox mode.
///
/// Why: the refusal has to run before the daemon does anything else, and the
/// latch has to be set before anything can read a credential.
/// What: [`enter_with`] over this process's environment and the library latch.
/// Test: `enter_with`'s tests; end to end, `tests/sandbox_daemon_9121.rs`.
pub(crate) fn enter() -> anyhow::Result<()> {
    enter_with(
        &SandboxEnv::capture(),
        trusty_mpm::secret_source::enter_sandbox,
    )
}

/// [`enter`] with the environment and the latch injected.
///
/// Why: the latch is process-wide, so a test that set it would change what
/// every parallel sibling's credential read returns.
/// What: every [`refusals`] entry becomes one clause of the error, and `latch`
/// is never called; with no refusal, `latch` runs once.
/// Test: `an_isolated_clean_environment_enters_and_latches`,
/// `a_refused_sandbox_never_latches`.
pub(crate) fn enter_with(env: &SandboxEnv, latch: impl FnOnce()) -> anyhow::Result<()> {
    let refused = refusals(env);
    if !refused.is_empty() {
        let reasons: Vec<String> = refused.iter().map(ToString::to_string).collect();
        anyhow::bail!(
            "refusing to start `tm daemon --sandbox` (#9121): {}. Start a sandbox daemon \
             with scripts/sandbox_daemon.sh, which runs it under `env -i` with an explicit \
             allowlist.",
            reasons.join("; ")
        );
    }
    latch();
    tracing::info!(
        "sandbox mode (#9121): no channel poller starts and no credential tier is consulted"
    );
    Ok(())
}

/// Every reason `env` is not an isolated sandbox. Empty means isolated.
///
/// Test: `a_token_or_key_env_refuses_and_names_only_the_variables`,
/// `refuses_without_a_data_dir_override`, `refuses_when_home_is_unset`,
/// `refuses_when_home_is_the_account_home`,
/// `refuses_when_home_symlinks_to_the_account_home`,
/// `refuses_when_the_account_home_is_unknown`,
/// `refuses_when_home_does_not_exist`.
pub(crate) fn refusals(env: &SandboxEnv) -> Vec<Refusal> {
    let mut out = Vec::new();
    if !env.secret_names.is_empty() {
        out.push(Refusal::SecretEnv(env.secret_names.clone()));
    }
    if env
        .data_dir_override
        .as_ref()
        .is_none_or(|value| value.is_empty())
    {
        out.push(Refusal::NoDataDirOverride);
    }
    if let Some(refusal) = home_refusal(env.home.as_deref(), env.account_home.as_deref()) {
        out.push(refusal);
    }
    out
}

/// Whether `home` is a home of its own, compared after resolving symlinks.
///
/// What: `None` only when `home` is set, resolves to an existing directory, the
/// account home is known, and the two differ. An account home that does not
/// resolve is compared as written: an existing `home` cannot equal it.
fn home_refusal(home: Option<&Path>, account: Option<&Path>) -> Option<Refusal> {
    let Some(home) = home.filter(|h| !h.as_os_str().is_empty()) else {
        return Some(Refusal::NoHome);
    };
    let Some(account) = account.filter(|a| !a.as_os_str().is_empty()) else {
        return Some(Refusal::AccountHomeUnknown);
    };
    let Ok(resolved_home) = std::fs::canonicalize(home) else {
        return Some(Refusal::HomeUnresolvable(home.to_path_buf()));
    };
    let resolved_account = std::fs::canonicalize(account).unwrap_or_else(|_| account.to_path_buf());
    (resolved_home == resolved_account).then(|| Refusal::HomeIsAccountHome(home.to_path_buf()))
}

/// The secret-shaped names among `names`, sorted and de-duplicated.
///
/// What: a name matches when, upper-cased, it ends in a [`SECRET_SUFFIXES`]
/// entry or is a variable the credential registry knows. A name that is not
/// UTF-8 is matched on its lossy form.
/// Test: `secret_shaped_names_matches_the_contract_suffixes`.
pub(crate) fn secret_shaped_names(names: impl Iterator<Item = OsString>) -> Vec<String> {
    let mut out: Vec<String> = names
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| {
            let upper = name.to_ascii_uppercase();
            SECRET_SUFFIXES.iter().any(|s| upper.ends_with(s))
                || is_registered_credential_env_var(&upper)
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Run `spawn` only outside sandbox mode.
///
/// Why: in sandbox mode no channel poller may start, whatever a credential
/// tier would have answered. Gating the spawn, not only the token, keeps the
/// bot down even if the credential gate regressed.
/// Test: `sandbox_never_spawns_the_telegram_bot`,
/// `outside_sandbox_the_bot_spawn_runs`.
pub(crate) fn gate_channel_pollers<T>(
    sandbox: bool,
    spawn: impl FnOnce() -> Option<T>,
) -> Option<T> {
    if sandbox {
        tracing::info!("sandbox mode (#9121): the Telegram bot is not started");
        return None;
    }
    spawn()
}

/// The account's home, from the password database rather than `$HOME`.
///
/// Why: `$HOME` is exactly what a sandbox reassigns, so it cannot be the
/// reference. `getpwuid(getuid())` is what the OS records for this user, and
/// the Keychain the incident reached is keyed to that same user.
/// What: `None` on a failed lookup or an empty home, which [`refusals`] treats
/// as indeterminate and refuses.
fn account_home() -> Option<PathBuf> {
    match nix::unistd::User::from_uid(nix::unistd::Uid::current()) {
        Ok(Some(user)) if !user.dir.as_os_str().is_empty() => Some(user.dir),
        _ => None,
    }
}

#[cfg(test)]
#[path = "daemon_sandbox_tests.rs"]
mod tests;
