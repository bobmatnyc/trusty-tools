//! [`KeeperSettings`]: how the Keeper backend invokes Keeper Commander.
//!
//! Why: #7519 P3 ruling 7 and ruling 74 — the program and the Commander
//! config file come only from the account's untracked machine config. The
//! config file holds the device's persistent-login token, so it is a
//! credential: an absolute path to a regular 0600 file the account owns.
//! Commander's own default search (the working directory, then `~/.keeper`)
//! is never used, so no planted `config.json` can be picked up.
//! What: [`KeeperSettings`] (program, leading args, config path, timeout),
//! built by [`KeeperSettings::from_machine`] from `secrets.keeper`.
//! Test: `keeper_machine_settings_are_checked_before_any_spawn`,
//! `keeper_program_must_be_an_absolute_executable_machine_pin`.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::not_installed;
use crate::api::SecretsError;
use crate::store::cli::is_executable_file;
use crate::store::config::MachineSecretsConfig;
use crate::store::file::{Kind, judge};

/// How long one `keeper` call may take, including its vault sync.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);

/// How the backend invokes `keeper`.
///
/// Why: see the module docs.
/// What: `program` plus `leading_args` is the command (the absolute
/// `keeper` and nothing in production; tests name `/bin/sh` and a shim
/// script). `config_path` becomes `--config <path>` on every call. A
/// backend refuses to spawn a `program` that is not absolute. Holds no
/// secret, so `Debug` is derived.
/// Test: `keeper_machine_settings_are_checked_before_any_spawn`.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct KeeperSettings {
    /// The program to run; absolute, or the backend spawns nothing.
    pub program: OsString,
    /// Arguments placed before every `keeper` argument.
    pub leading_args: Vec<OsString>,
    /// Commander's config file, passed as `--config`.
    pub config_path: PathBuf,
    /// How long one `keeper` call may take.
    pub timeout: Duration,
}

impl KeeperSettings {
    /// `program` with `config_path`, no leading args, the default timeout.
    pub fn new(program: impl Into<OsString>, config_path: PathBuf) -> Self {
        Self {
            program: program.into(),
            leading_args: Vec::new(),
            config_path,
            timeout: DEFAULT_TIMEOUT,
        }
    }

    /// Settings from the machine config's `secrets.keeper` section.
    ///
    /// What: in order, each failing before anything is spawned:
    /// - an `account` is [`SecretsError::Config`]: Commander's account is
    ///   the one its config file logs in to, so a second source is refused;
    /// - `config_path` must be set and absolute ([`SecretsError::Config`]),
    ///   exist ([`SecretsError::Config`]), and be a regular file, not a
    ///   symlink, with no bit beyond 0600, owned by this user
    ///   ([`SecretsError::StorageRefused`]);
    /// - `program` must be set ([`SecretsError::CliNotInstalled`]), absolute
    ///   ([`SecretsError::Config`]), and a regular executable file
    ///   ([`SecretsError::CliNotInstalled`]). No `PATH` search.
    ///
    /// Errors name `machine_path` and the key, never a value.
    /// Test: `keeper_machine_settings_are_checked_before_any_spawn`,
    /// `keeper_program_must_be_an_absolute_executable_machine_pin`.
    pub fn from_machine(
        machine: &MachineSecretsConfig,
        machine_path: &Path,
    ) -> Result<Self, SecretsError> {
        let invalid = |reason: &str| SecretsError::Config {
            path: machine_path.to_path_buf(),
            reason: reason.to_string(),
        };
        // `default_backend: keeper` with no section enables it, with nothing set.
        let section = machine.keeper.clone().unwrap_or_default();
        if section.account.is_some() {
            return Err(invalid(
                "`secrets.keeper.account` is not used: Keeper's account is the one `config_path` logs in to",
            ));
        }
        // #7519: ruling 7 — the config file is a credential; machine config only.
        let Some(config_path) = section.config_path else {
            return Err(invalid(
                "`secrets.keeper.config_path` must name Keeper Commander's config file",
            ));
        };
        if !config_path.is_absolute() {
            return Err(invalid(
                "`secrets.keeper.config_path` must be an absolute path",
            ));
        }
        match std::fs::symlink_metadata(&config_path) {
            // #7519: ruling 7 — mode 0600, owned by this user, not a link.
            Ok(meta) => judge(&config_path, &meta, Kind::File)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(invalid(
                    "`secrets.keeper.config_path` names no file; log in with Keeper Commander first",
                ));
            }
            Err(source) => {
                return Err(SecretsError::Io {
                    path: config_path,
                    source,
                });
            }
        }
        // #7519: ruling 74 — the program is a machine pin, never a `PATH` hit.
        let Some(program) = section.program else {
            return Err(not_installed(OsStr::new(super::SPEC.program)));
        };
        if !program.is_absolute() {
            return Err(invalid("`secrets.keeper.program` must be an absolute path"));
        }
        if !is_executable_file(&program) {
            return Err(not_installed(program.as_os_str()));
        }
        Ok(Self::new(program, config_path))
    }
}
