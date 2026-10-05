//! `tm daemon --sandbox`: the isolation contract for a live-check daemon (#9121).
//!
//! Why: a qa live-check started a "sandbox" daemon with its own `$HOME` and
//! port. It inherited `TELEGRAM_BOT_TOKEN` and polled the real bot
//! (`TerminatedByOtherGetUpdates`). A fresh `$HOME` is not isolation: the
//! credential store and the Keychain belong to the OS user, not to `$HOME`.
//! What: [`enter`] refuses unless every environment variable is on the closed
//! [`ENV_ALLOWLIST`], `TRUSTY_DATA_DIR_OVERRIDE` is set, and `$HOME` is not the
//! account's home. On success it latches
//! `trusty_mpm::secret_source::enter_sandbox`, so no credential tier is read.
//! [`gate_channel_pollers`] keeps the Telegram bot down. Every check fails
//! closed: an answer that cannot be determined refuses.
//! Test: `daemon_sandbox_tests.rs`; end to end, `tests/sandbox_daemon_9121.rs`.

use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};

use trusty_common::DATA_DIR_OVERRIDE_ENV;

/// The only variables a sandbox daemon may inherit, compared exactly.
///
/// Why (#9121, Architect ruling): a denylist of secret-shaped suffixes misses
/// any credential spelled another way (`GITHUB_PAT`, `DATABASE_URL` with a
/// password). A closed allowlist refuses everything not named here. Each
/// entry, and why the daemon needs it:
/// - `HOME`: the sandbox's own home, checked against the account home below.
/// - `PATH`: the daemon spawns `tmux`, `git` and `claude` by name.
/// - `TRUSTY_DATA_DIR_OVERRIDE`: required; keeps data and sockets out of the
///   real OS data directory, which `$HOME` does not redirect on macOS.
/// - `TRUSTY_MPM_ADDR`: the sandbox's own listen address.
/// - `TMPDIR`: the per-user temp directory on macOS.
/// - `TERM`, `LANG`, [`LOCALE_CATEGORIES`]: terminal type and locale for the
///   tmux panes and git output the daemon reads back.
/// - `USER`, `LOGNAME`, `SHELL`: the account name and login shell tmux and git
///   read; names, not credentials.
/// - `RUST_LOG`: the log filter.
/// - `TRUSTY_SANDBOX`: set to `1` by `scripts/sandbox_daemon.sh`; tells
///   trusty-common to load no `.env.local` (#9178). A flag, not a credential.
///
/// Plus [`CF_TEXT_ENCODING`], which macOS sets inside the process itself.
const ENV_ALLOWLIST: &[&str] = &[
    "HOME",
    "PATH",
    DATA_DIR_OVERRIDE_ENV,
    "TRUSTY_MPM_ADDR",
    "TMPDIR",
    "TERM",
    "LANG",
    "USER",
    "LOGNAME",
    "SHELL",
    "RUST_LOG",
    // #9178: the sandbox script's `.env.local` opt-out.
    "TRUSTY_SANDBOX",
];

/// The locale categories allowed by name — the `LC_*` family, closed.
///
/// Why: an open `LC_` prefix would admit `LC_API_KEY`. These are the POSIX
/// categories plus glibc's six extra ones, and nothing else.
const LOCALE_CATEGORIES: &[&str] = &[
    "LC_ALL",
    "LC_COLLATE",
    "LC_CTYPE",
    "LC_MESSAGES",
    "LC_MONETARY",
    "LC_NUMERIC",
    "LC_TIME",
    "LC_ADDRESS",
    "LC_IDENTIFICATION",
    "LC_MEASUREMENT",
    "LC_NAME",
    "LC_PAPER",
    "LC_TELEPHONE",
];

/// macOS CoreFoundation's text-encoding hint.
///
/// Why: CoreFoundation sets it in every process it loads into, so the daemon
/// carries it even when started under `env -i`
/// (`an_allowlisted_environment_passes_the_allowlist_check` caught it). It is
/// allowed only in CF's own `0x<hex>:0x<hex>:0x<hex>` form, so it cannot carry
/// anything else past the allowlist.
const CF_TEXT_ENCODING: &str = "__CF_USER_TEXT_ENCODING";

/// Whether `(name, value)` is CoreFoundation's own [`CF_TEXT_ENCODING`].
///
/// Test: `only_a_well_formed_cf_text_encoding_is_allowed`.
pub(crate) fn is_cf_text_encoding(name: &OsString, value: &OsString) -> bool {
    let field = |f: &str| {
        let hex = f.strip_prefix("0x").unwrap_or(f);
        (1..=8).contains(&hex.len()) && hex.bytes().all(|b| b.is_ascii_hexdigit())
    };
    name == CF_TEXT_ENCODING
        && value.to_str().is_some_and(|v| {
            let fields: Vec<&str> = v.split(':').collect();
            fields.len() == 3 && fields.iter().all(|f| field(f))
        })
}

/// The parts of the environment the sandbox contract reads. Holds no value of
/// any variable outside the allowlist — only names.
#[derive(Debug, Clone, Default)]
pub(crate) struct SandboxEnv {
    /// Variable NAMES present but not on the allowlist, sorted.
    pub(crate) disallowed_names: Vec<String>,
    /// `TRUSTY_DATA_DIR_OVERRIDE`, when set.
    pub(crate) data_dir_override: Option<OsString>,
    /// `$HOME`, when set.
    pub(crate) home: Option<PathBuf>,
    /// The account's home from the password database, when it resolves.
    pub(crate) account_home: Option<PathBuf>,
}

impl SandboxEnv {
    /// Read this process's environment. Values are dropped at the iterator,
    /// before anything stores them; only [`CF_TEXT_ENCODING`]'s is inspected.
    fn capture() -> Self {
        let names = std::env::vars_os()
            .filter(|(name, value)| !is_cf_text_encoding(name, value))
            .map(|(name, _)| name);
        Self {
            disallowed_names: disallowed_names(names),
            data_dir_override: std::env::var_os(DATA_DIR_OVERRIDE_ENV),
            home: std::env::var_os("HOME").map(PathBuf::from),
            account_home: account_home(),
        }
    }
}

/// One reason a sandbox daemon refuses to start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Refusal {
    /// Variables outside [`ENV_ALLOWLIST`] are present; carries names only.
    DisallowedEnv(Vec<String>),
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
            Self::DisallowedEnv(names) => write!(
                f,
                "the environment carries variables outside the sandbox allowlist \
                 (values not shown): {}",
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
        // #9121 (#7247): name the launcher by role — this binary ships to
        // projects that have no such script path.
        anyhow::bail!(
            "refusing to start `tm daemon --sandbox` (#9121): {}. Start a sandbox daemon \
             with the project's sandbox launcher (`env -i` + allowlist).",
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
/// Test: `a_variable_outside_the_allowlist_refuses_and_names_only_the_variables`,
/// `refuses_without_a_data_dir_override`, `refuses_when_home_is_unset`,
/// `refuses_when_home_is_the_account_home`,
/// `refuses_when_home_symlinks_to_the_account_home`,
/// `refuses_when_the_account_home_is_unknown`,
/// `refuses_when_home_does_not_exist`.
pub(crate) fn refusals(env: &SandboxEnv) -> Vec<Refusal> {
    let mut out = Vec::new();
    if !env.disallowed_names.is_empty() {
        out.push(Refusal::DisallowedEnv(env.disallowed_names.clone()));
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

/// The names among `names` that are not on the allowlist, sorted and
/// de-duplicated.
///
/// What: a name is allowed only when it equals an [`ENV_ALLOWLIST`] or
/// [`LOCALE_CATEGORIES`] entry exactly — case included, as the OS compares
/// them. A name that is not UTF-8 is reported by its lossy form and is never
/// allowed.
/// Test: `disallowed_names_admits_only_the_closed_allowlist`.
pub(crate) fn disallowed_names(names: impl Iterator<Item = OsString>) -> Vec<String> {
    let mut out: Vec<String> = names
        .filter(|name| {
            name.to_str().is_none_or(|name| {
                !ENV_ALLOWLIST.contains(&name) && !LOCALE_CATEGORIES.contains(&name)
            })
        })
        .map(|name| name.to_string_lossy().into_owned())
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
