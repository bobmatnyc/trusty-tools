//! [`GitHubActionsSettings`]: how the GitHub Actions sync target runs `gh`
//! (#9069).
//!
//! Why: D7 (owner ruling D63) — a push uses `gh`'s own stored login, and
//! ruling 74 — only the account's untracked machine config may name the
//! program and the `gh` config directory. An inherited `GH_*` variable or
//! token would choose the identity or the host of a push, so every one is
//! removed from the child's environment.
//! What: [`GitHubActionsSettings`], built by
//! [`GitHubActionsSettings::from_machine`] from `secrets.github_actions`,
//! and [`steering_vars`], the inherited names a run removes.
//! Test: `github_machine_settings_are_checked_before_any_spawn`,
//! `github_child_env_carries_no_inherited_token_or_repo`.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::api::SecretsError;
use crate::store::config::MachineSecretsConfig;
use crate::store::program::{find_in_dirs, is_executable_file};

/// How long one `gh` call may take; the request deadline also bounds it.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);

/// The variable a configured `config_path` reaches `gh` through.
pub(super) const CONFIG_DIR_ENV: &str = "GH_CONFIG_DIR";

/// The names G2 lists, removed from every `gh` run whether inherited or not.
const STEERING: [&str; 7] = [
    "GH_TOKEN",
    "GITHUB_TOKEN",
    "GH_ENTERPRISE_TOKEN",
    "GITHUB_ENTERPRISE_TOKEN",
    "GH_HOST",
    "GH_REPO",
    "GH_CONFIG_DIR",
];

/// How the backend invokes `gh`.
///
/// Why: see the module docs.
/// What: `program` is an absolute `gh`; `config_dir`, when set, becomes the
/// child's `GH_CONFIG_DIR` and nothing else does. Holds no secret, so
/// `Debug` is derived.
/// Test: `github_machine_settings_are_checked_before_any_spawn`.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct GitHubActionsSettings {
    /// The program to run; absolute, or the backend spawns nothing.
    pub program: OsString,
    /// `gh`'s config directory, passed as `GH_CONFIG_DIR`.
    pub config_dir: Option<PathBuf>,
    /// How long one `gh` call may take.
    pub timeout: Duration,
}

impl GitHubActionsSettings {
    /// `program` with no config directory and the default timeout.
    pub fn new(program: impl Into<OsString>) -> Self {
        Self {
            program: program.into(),
            config_dir: None,
            timeout: DEFAULT_TIMEOUT,
        }
    }

    /// Settings from the machine config's `secrets.github_actions` section.
    ///
    /// What: in order, each failing before anything is spawned:
    /// - an `account` is [`SecretsError::Config`]: the account is the one
    ///   `gh`'s login selects, so a second source is refused;
    /// - a `config_path` must be absolute and an existing directory
    ///   ([`SecretsError::Config`]);
    /// - a `program` pin must be absolute ([`SecretsError::Config`]) and a
    ///   regular executable file ([`SecretsError::CliNotInstalled`]); with no
    ///   pin, the first executable `gh` in `dirs`, never a `PATH`, else
    ///   [`SecretsError::CliNotInstalled`].
    ///
    /// Errors name `machine_path` and the key, never a value.
    /// Test: `github_machine_settings_are_checked_before_any_spawn`.
    pub fn from_machine(
        machine: &MachineSecretsConfig,
        machine_path: &Path,
        dirs: &[PathBuf],
    ) -> Result<Self, SecretsError> {
        let invalid = |reason: &str| SecretsError::Config {
            path: machine_path.to_path_buf(),
            reason: reason.to_string(),
        };
        let section = machine.github_actions.clone().unwrap_or_default();
        if section.account.is_some() {
            return Err(invalid(
                "`secrets.github_actions.account` is not used: the account is the one `gh`'s login selects",
            ));
        }
        if let Some(dir) = &section.config_path
            && !(dir.is_absolute() && dir.is_dir())
        {
            return Err(invalid(
                "`secrets.github_actions.config_path` must be the absolute path of `gh`'s config directory",
            ));
        }
        // #9069: ruling 74 — a machine pin or a fixed system directory, never `PATH`.
        let program = match section.program {
            Some(pin) if !pin.is_absolute() => {
                return Err(invalid(
                    "`secrets.github_actions.program` must be an absolute path",
                ));
            }
            Some(pin) if is_executable_file(&pin) => pin,
            Some(pin) => return Err(super::not_installed(pin.as_os_str())),
            None => find_in_dirs(super::SPEC.program, dirs)
                .ok_or_else(|| super::not_installed(OsStr::new(super::SPEC.program)))?,
        };
        Ok(Self {
            program: program.into_os_string(),
            config_dir: section.config_path,
            timeout: DEFAULT_TIMEOUT,
        })
    }
}

/// The inherited variable names a `gh` run removes.
///
/// Why: D7 — `GH_TOKEN`, `GITHUB_TOKEN` and their enterprise forms replace
/// `gh`'s stored login; `GH_HOST`, `GH_REPO` and `GH_CONFIG_DIR` move the
/// host, the repository or the login it reads (G2).
/// What: [`STEERING`] plus every name in `inherited` that starts with `GH_`,
/// so a variable `gh` adds later is covered too. `GH_CONFIG_DIR` returns as
/// an overlay only from the machine config.
/// Test: `github_child_env_carries_no_inherited_token_or_repo`.
pub(super) fn steering_vars(inherited: impl IntoIterator<Item = OsString>) -> Vec<OsString> {
    let mut names: Vec<OsString> = STEERING.iter().map(OsString::from).collect();
    names.extend(
        inherited
            .into_iter()
            .filter(|name| name.as_encoded_bytes().starts_with(b"GH_")),
    );
    names
}
