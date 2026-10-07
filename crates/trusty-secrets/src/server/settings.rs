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
//! #4567: only flags move the audit log — `--audit-log`, or `--index-dir`,
//! which puts it beside the index ([`audit_log_beside`]). No environment
//! variable does, not even [`INDEX_DIR_ENV`]: the on-demand client passes the
//! caller's environment through, and a tracked `.envrc` or an agent's
//! environment must not move the audit trail into a project tree (DOC-45
//! C-7.12).
//! #7524: for the same reason [`INDEX_DIR_ENV`] moves the index only for a
//! server off the default socket. The first spawner's environment would
//! otherwise move the names index for every client of the shared server; a
//! test or sandbox on its own socket keeps the override, and `--index-dir`
//! works on any socket.
//! Test: `settings_flags_beat_env_beat_defaults`,
//! `settings_audit_log_defaults_beside_the_index`,
//! `settings_ignore_an_audit_log_environment_variable`,
//! `settings_idle_env_falls_back_on_garbage_and_zero`,
//! `settings_reject_unknown_and_incomplete_flags`,
//! `settings_index_env_is_ignored_on_the_default_socket`,
//! `settings_index_override_survives_off_the_default_socket`.

use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use crate::api::SecretsError;
use crate::store::INDEX_SUBDIR;
use crate::store::config::MACHINE_CONFIG_SUBPATH;

/// Socket path under `$HOME` (owner ruling 31).
pub const SOCKET_SUBPATH: &str = ".trusty-tools/trusty-secrets/secrets.sock";

/// Overrides the socket path.
pub const SOCKET_ENV: &str = "TRUSTY_SECRETS_SOCKET";

/// Overrides the names-only index directory, but only for a server whose
/// socket is not the default one (#7524).
pub const INDEX_DIR_ENV: &str = "TRUSTY_SECRETS_INDEX_DIR";

/// The credential access audit log under `$HOME` (#4567, DOC-45 C-7.9).
pub const AUDIT_LOG_SUBPATH: &str = ".trusty-tools/trusty-secrets/audit/audit.jsonl";

/// Size at which the audit log is rotated to `<log>.1` when next opened.
///
/// Why: the server is on demand (ruling 28), so nothing is resident to rotate
/// on a timer; the cap is checked each time the log is opened (#4567).
pub const DEFAULT_AUDIT_MAX_BYTES: u64 = 8 * 1024 * 1024;

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
        "usage: trusty-secrets serve [--socket PATH] [--index-dir PATH] [--machine-config PATH] [--audit-log PATH] [--idle-timeout-secs N]"
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
/// What: plain fields; build one with [`ServerSettings::new`], or
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
    /// The append-only credential access audit log (#4567).
    pub audit_log: PathBuf,
    /// Size at which [`Self::audit_log`] is rotated when next opened.
    pub audit_max_bytes: u64,
}

impl ServerSettings {
    /// Settings from explicit values, for a caller that does not parse argv.
    ///
    /// What: the audit log is [`audit_log_beside`] the index, capped at
    /// [`DEFAULT_AUDIT_MAX_BYTES`]; change either with the `with_` methods.
    pub fn new(
        socket: PathBuf,
        index_root: PathBuf,
        machine_config: PathBuf,
        idle_timeout: Duration,
    ) -> Self {
        Self {
            audit_log: audit_log_beside(&index_root),
            audit_max_bytes: DEFAULT_AUDIT_MAX_BYTES,
            socket,
            index_root,
            machine_config,
            idle_timeout,
        }
    }

    /// These settings with the audit log at `path`.
    pub fn with_audit_log(mut self, path: PathBuf) -> Self {
        self.audit_log = path;
        self
    }

    /// These settings with the audit log rotated at `bytes`.
    pub fn with_audit_max_bytes(mut self, bytes: u64) -> Self {
        self.audit_max_bytes = bytes;
        self
    }

    /// Settings from a `serve` command line and an environment lookup.
    ///
    /// What: `args` excludes the program name and starts with `serve`. Each
    /// location is its flag, else its environment variable, else the
    /// default under `$HOME` ([`SOCKET_SUBPATH`], [`INDEX_SUBDIR`],
    /// [`MACHINE_CONFIG_SUBPATH`]). `$HOME` is resolved only when a default
    /// is actually needed. The audit log is `--audit-log`, else
    /// [`audit_log_beside`] an index named by the `--index-dir` flag, else
    /// [`AUDIT_LOG_SUBPATH`] under `$HOME` — also when the index comes from
    /// [`INDEX_DIR_ENV`]. No environment variable moves it. #7524:
    /// [`INDEX_DIR_ENV`] is read only when the socket is not the default
    /// socket (`is_default_socket`); on it, the index is the `--index-dir`
    /// flag or the default.
    ///
    /// # Errors
    ///
    /// [`SettingsError`] for a malformed command line or an unknown `$HOME`.
    ///
    /// Test: `settings_flags_beat_env_beat_defaults`,
    /// `settings_reject_unknown_and_incomplete_flags`,
    /// `settings_index_env_is_ignored_on_the_default_socket`.
    pub fn from_args(
        args: impl IntoIterator<Item = OsString>,
        env: impl Fn(&str) -> Option<String>,
    ) -> Result<Self, SettingsError> {
        let mut args = args.into_iter();
        if args.next().as_deref() != Some(SERVE_SUBCOMMAND.as_ref()) {
            return Err(SettingsError::Usage);
        }
        let (mut socket, mut index_root, mut machine_config, mut idle) = (None, None, None, None);
        let mut audit_log = None;
        while let Some(flag) = args.next() {
            let flag: &'static str = match flag.to_str() {
                Some("--socket") => "--socket",
                Some("--index-dir") => "--index-dir",
                Some("--machine-config") => "--machine-config",
                Some("--idle-timeout-secs") => "--idle-timeout-secs",
                Some("--audit-log") => "--audit-log",
                _ => return Err(SettingsError::UnknownArgument),
            };
            let value = args.next().ok_or(SettingsError::MissingValue { flag })?;
            match flag {
                "--socket" => socket = Some(PathBuf::from(value)),
                "--index-dir" => index_root = Some(PathBuf::from(value)),
                "--machine-config" => machine_config = Some(PathBuf::from(value)),
                "--audit-log" => audit_log = Some(PathBuf::from(value)),
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
        // #4567: only a flag moves the audit log, never the environment.
        let audit_log = match (audit_log, &index_root) {
            (Some(path), _) => path,
            (None, Some(index_flag)) => audit_log_beside(index_flag),
            (None, None) => under_home(AUDIT_LOG_SUBPATH)?,
        };
        let socket = pick(socket, SOCKET_ENV, SOCKET_SUBPATH)?;
        let index_root = match index_root {
            Some(path) => path,
            // #7524: a caller's environment never moves the shared server's index.
            None if is_default_socket(&socket) => under_home(INDEX_SUBDIR)?,
            None => pick(None, INDEX_DIR_ENV, INDEX_SUBDIR)?,
        };
        Ok(Self {
            socket,
            machine_config: match machine_config {
                Some(path) => path,
                None => under_home(MACHINE_CONFIG_SUBPATH)?,
            },
            idle_timeout: idle.unwrap_or_else(|| idle_from_env(env(IDLE_TIMEOUT_ENV).as_deref())),
            audit_log,
            audit_max_bytes: DEFAULT_AUDIT_MAX_BYTES,
            index_root,
        })
    }
}

/// The default audit log for an index at `index_root`: `audit/audit.jsonl`
/// in the index's parent directory.
///
/// Why: for the default index this is [`AUDIT_LOG_SUBPATH`]; for an index
/// redirected by the `--index-dir` flag (a test, a sandbox) the audit follows
/// it out of `$HOME` (#4567).
/// Test: `settings_audit_log_defaults_beside_the_index`.
pub fn audit_log_beside(index_root: &Path) -> PathBuf {
    index_root
        .parent()
        .unwrap_or(index_root)
        .join("audit")
        .join("audit.jsonl")
}

/// Whether `socket` is the shared default socket under `$HOME`.
///
/// Why: #7524 M3 — [`INDEX_DIR_ENV`] must not reach the server every client
/// shares, so another spelling of the default path counts as the default.
/// What: `true` when `$HOME` is unknown (fail closed), when `socket` equals
/// the default after lexical normalisation (`.`, `..`, repeated `/`), or
/// when both share a file name and their parent directories canonicalise
/// to one path (a symlinked directory). A lexical `..` match counts even if
/// a symlink would resolve it elsewhere; that only ignores the variable.
/// Test: `settings_index_env_is_ignored_on_the_default_socket`,
/// `settings_socket_alias_through_a_symlinked_dir_is_the_same_socket`.
pub(crate) fn is_default_socket(socket: &Path) -> bool {
    dirs::home_dir().is_none_or(|home| same_socket(socket, &home.join(SOCKET_SUBPATH)))
}

/// Whether `a` and `b` name one socket; see [`is_default_socket`].
pub(crate) fn same_socket(a: &Path, b: &Path) -> bool {
    if lexical(a) == lexical(b) {
        return true;
    }
    let parent = |p: &Path| p.parent().and_then(|dir| dir.canonicalize().ok());
    a.file_name() == b.file_name() && parent(a).is_some_and(|dir| parent(b) == Some(dir))
}

/// `path` with `.` dropped and each `..` removing the component before it.
fn lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
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

#[cfg(test)]
#[path = "settings_tests.rs"]
mod tests;
