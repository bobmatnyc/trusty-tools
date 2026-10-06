//! Where the server binds, where its index lives, and when it exits.
//!
//! Why: owner ruling 31 fixes the socket at
//! `~/.trusty-tools/trusty-secrets/secrets.sock` and the idle exit at 60 s
//! with an environment override. Tests must redirect every path so no test
//! touches `~/.trusty-tools` (#9065 brief, item 8), so each location also
//! takes a flag and an environment variable.
//! What: [`ServerSettings`] and [`ServerSettings::from_args`], which reads
//! `serve` flags first, then the environment, then the defaults. The
//! environment is passed in as a function, so a test never calls `set_var`.
//! Test: `settings_flags_beat_env_beat_defaults`,
//! `settings_idle_env_falls_back_on_garbage_and_zero`,
//! `settings_reject_unknown_and_incomplete_flags`.

use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;

use crate::api::SecretsError;
use crate::store::INDEX_SUBDIR;
use crate::store::config::MACHINE_CONFIG_SUBPATH;

/// Socket path under `$HOME` (owner ruling 31).
pub const SOCKET_SUBPATH: &str = ".trusty-tools/trusty-secrets/secrets.sock";

/// Overrides the socket path.
pub const SOCKET_ENV: &str = "TRUSTY_SECRETS_SOCKET";

/// Overrides the names-only index directory.
pub const INDEX_DIR_ENV: &str = "TRUSTY_SECRETS_INDEX_DIR";

/// Overrides the idle window, in whole seconds (owner ruling 31).
///
/// What: a positive integer. Unset, unparseable, or `0` means the default —
/// there is no "never exit" value, because a resident socket is a daemon
/// (owner ruling 28).
pub const IDLE_TIMEOUT_ENV: &str = "TRUSTY_SECRETS_IDLE_TIMEOUT_SECS";

/// Idle window before the server exits (owner ruling 31).
pub const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(60);

/// The subcommand that serves the socket.
pub const SERVE_SUBCOMMAND: &str = "serve";

/// A `serve` command line that could not be read.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SettingsError {
    /// The first argument is not `serve`.
    #[error(
        "usage: trusty-secrets serve [--socket PATH] [--index-dir PATH] [--machine-config PATH] [--idle-timeout-secs N]"
    )]
    Usage,
    /// A flag this binary does not define. The flag itself is not echoed.
    #[error("unknown argument to `trusty-secrets serve`")]
    UnknownArgument,
    /// A flag with no value after it.
    #[error("{flag} needs a value")]
    MissingValue {
        /// The flag.
        flag: &'static str,
    },
    /// `--idle-timeout-secs` is not a positive integer.
    #[error("--idle-timeout-secs must be a positive whole number of seconds")]
    InvalidIdleTimeout,
    /// A default location needs `$HOME`.
    #[error(transparent)]
    Home(#[from] SecretsError),
}

/// Everything `serve` needs to know before it binds.
///
/// Why: see the module docs.
/// What: four plain fields; build one with [`ServerSettings::new`], or
/// through [`ServerSettings::from_args`] in the binary.
/// Test: `settings_flags_beat_env_beat_defaults`.
// #9328: `#[non_exhaustive]` so a later setting is not a breaking change.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ServerSettings {
    /// The socket to bind.
    pub socket: PathBuf,
    /// The names-only index directory.
    pub index_root: PathBuf,
    /// The machine config file holding `secrets.default_backend`.
    pub machine_config: PathBuf,
    /// How long the server stays up with no answered request.
    pub idle_timeout: Duration,
}

impl ServerSettings {
    /// Settings from explicit values, for a caller that does not parse argv.
    pub fn new(
        socket: PathBuf,
        index_root: PathBuf,
        machine_config: PathBuf,
        idle_timeout: Duration,
    ) -> Self {
        Self {
            socket,
            index_root,
            machine_config,
            idle_timeout,
        }
    }

    /// Settings from a `serve` command line and an environment lookup.
    ///
    /// What: `args` excludes the program name and starts with `serve`. Each
    /// location is its flag, else its environment variable, else the
    /// default under `$HOME` ([`SOCKET_SUBPATH`], [`INDEX_SUBDIR`],
    /// [`MACHINE_CONFIG_SUBPATH`]). `$HOME` is resolved only when a default
    /// is actually needed.
    ///
    /// # Errors
    ///
    /// [`SettingsError`] for a malformed command line or an unknown `$HOME`.
    ///
    /// Test: `settings_flags_beat_env_beat_defaults`,
    /// `settings_reject_unknown_and_incomplete_flags`.
    pub fn from_args(
        args: impl IntoIterator<Item = OsString>,
        env: impl Fn(&str) -> Option<String>,
    ) -> Result<Self, SettingsError> {
        let mut args = args.into_iter();
        if args.next().as_deref() != Some(SERVE_SUBCOMMAND.as_ref()) {
            return Err(SettingsError::Usage);
        }
        let (mut socket, mut index_root, mut machine_config, mut idle) = (None, None, None, None);
        while let Some(flag) = args.next() {
            let flag: &'static str = match flag.to_str() {
                Some("--socket") => "--socket",
                Some("--index-dir") => "--index-dir",
                Some("--machine-config") => "--machine-config",
                Some("--idle-timeout-secs") => "--idle-timeout-secs",
                _ => return Err(SettingsError::UnknownArgument),
            };
            let value = args.next().ok_or(SettingsError::MissingValue { flag })?;
            match flag {
                "--socket" => socket = Some(PathBuf::from(value)),
                "--index-dir" => index_root = Some(PathBuf::from(value)),
                "--machine-config" => machine_config = Some(PathBuf::from(value)),
                _ => idle = Some(parse_idle_flag(&value)?),
            }
        }

        let under_home = |sub: &str| -> Result<PathBuf, SettingsError> {
            Ok(dirs::home_dir()
                .ok_or(SecretsError::HomeUnavailable)?
                .join(sub))
        };
        let pick = |flag: Option<PathBuf>, var: &str, sub: &str| match flag
            .or_else(|| env(var).filter(|v| !v.is_empty()).map(PathBuf::from))
        {
            Some(path) => Ok(path),
            None => under_home(sub),
        };
        Ok(Self {
            socket: pick(socket, SOCKET_ENV, SOCKET_SUBPATH)?,
            index_root: pick(index_root, INDEX_DIR_ENV, INDEX_SUBDIR)?,
            machine_config: match machine_config {
                Some(path) => path,
                None => under_home(MACHINE_CONFIG_SUBPATH)?,
            },
            idle_timeout: idle.unwrap_or_else(|| idle_from_env(env(IDLE_TIMEOUT_ENV).as_deref())),
        })
    }
}

/// A `--idle-timeout-secs` value: strict, because a flag is typed on purpose.
fn parse_idle_flag(value: &OsString) -> Result<Duration, SettingsError> {
    match value.to_str().map(str::trim).map(str::parse::<u64>) {
        Some(Ok(secs)) if secs > 0 => Ok(Duration::from_secs(secs)),
        _ => Err(SettingsError::InvalidIdleTimeout),
    }
}

/// The idle window from [`IDLE_TIMEOUT_ENV`]'s raw value.
///
/// What: a positive integer is that many seconds; anything else — unset,
/// empty, `0`, garbage — is [`DEFAULT_IDLE_TIMEOUT`]. Lenient where the flag
/// is strict: a typo in an inherited variable must not stop every on-demand
/// spawn.
/// Test: `settings_idle_env_falls_back_on_garbage_and_zero`.
pub fn idle_from_env(raw: Option<&str>) -> Duration {
    match raw.map(str::trim).map(str::parse::<u64>) {
        Some(Ok(secs)) if secs > 0 => Duration::from_secs(secs),
        _ => DEFAULT_IDLE_TIMEOUT,
    }
}
