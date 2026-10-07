//! [`OnePasswordSettings`]: how the 1Password backend invokes `op`, and
//! which of `op`'s environment variables the server keeps.
//!
//! Why: #7519 A4/A10 and owner ruling 2026-10-07 — `account`,
//! `config_path` and a `program` pin come only from the untracked machine
//! config, and the service-account token reaches `op` only through the
//! runner's environment overlay. A caller's inherited `OP_*` variables must
//! not choose the account, the config directory, or a Connect server for it.
//! What: [`OnePasswordSettings`] (program, flags, token, template
//! directory, timeout), built from the machine config by
//! [`OnePasswordSettings::from_machine`]; [`inherited_op_vars`], the
//! variables the server removes from its own environment at start; and
//! [`token_from`].
//! Test: `onepassword_machine_settings_reach_argv`,
//! `onepassword_inherited_op_vars_keep_only_sessions`.

use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::api::{SecretValue, SecretsError};
use crate::store::config::MachineSecretsConfig;

/// The variable `op` reads a service-account token from.
pub const SERVICE_ACCOUNT_TOKEN_ENV: &str = "OP_SERVICE_ACCOUNT_TOKEN";

/// How long one `op` call may take, including a desktop-app unlock prompt.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);

/// The prefix of every variable `op` reads.
const OP_PREFIX: &str = "OP_";

/// The prefix of an `op signin` session variable, which the server keeps.
const SESSION_PREFIX: &str = "OP_SESSION_";

/// How the backend invokes `op`.
///
/// Why: see the module docs.
/// What: `program` plus `leading_args` is the command (the absolute `op`
/// and nothing in production; tests name `/bin/sh` and a shim script by
/// absolute path). A backend refuses to spawn a `program` that is not
/// absolute (#7519).
/// `account` and `config_dir` become `--account` and `--config` in argv;
/// `token` becomes [`SERVICE_ACCOUNT_TOKEN_ENV`] in the child's environment
/// only. Template files for `op item edit` go under `template_root`.
/// `Debug` shows whether a token is set, never the token.
/// Test: `onepassword_headless_without_a_token_fails_closed`.
#[derive(Clone)]
#[non_exhaustive]
pub struct OnePasswordSettings {
    /// The program to run; absolute by the time a backend spawns it.
    pub program: OsString,
    /// Arguments placed before every `op` argument.
    pub leading_args: Vec<OsString>,
    /// `op --account`, from the machine config only.
    pub account: Option<String>,
    /// `op --config`, from the machine config only; absolute.
    pub config_dir: Option<PathBuf>,
    /// The service-account token, passed in the child's environment only.
    pub token: Option<SecretValue>,
    /// The 0700 directory template files are written under.
    pub template_root: PathBuf,
    /// How long one `op` call may take.
    pub timeout: Duration,
}

impl fmt::Debug for OnePasswordSettings {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OnePasswordSettings")
            .field("program", &self.program)
            .field("leading_args", &self.leading_args)
            .field("account", &self.account)
            .field("config_dir", &self.config_dir)
            .field("token_set", &self.token.is_some())
            .field("template_root", &self.template_root)
            .field("timeout", &self.timeout)
            .finish()
    }
}

impl OnePasswordSettings {
    /// Bare `op`, no account, no config directory, no token.
    ///
    /// What: a backend refuses to spawn bare `op`; `open` resolves it with
    /// `resolve_program`, and a test sets `program` by absolute path.
    pub fn new(template_root: PathBuf) -> Self {
        Self {
            program: OsString::from("op"),
            leading_args: Vec::new(),
            account: None,
            config_dir: None,
            token: None,
            template_root,
            timeout: DEFAULT_TIMEOUT,
        }
    }

    /// Settings from the machine config's `secrets.onepassword` section.
    ///
    /// What: [`Self::new`] with `token`, plus the section's `account`,
    /// `config_path` and `program`. An account outside `[A-Za-z0-9._@:-]`,
    /// or starting with `-`, and a relative config path or program are
    /// [`SecretsError::Config`] naming `machine_path`, never the value. A
    /// `program` that is not a regular executable file is
    /// [`SecretsError::CliNotInstalled`]. The project file is never read
    /// here; P0 refuses these keys in it.
    /// Test: `onepassword_machine_settings_reach_argv`,
    /// `onepassword_machine_program_pin_is_used_and_must_be_absolute`.
    pub fn from_machine(
        machine: &MachineSecretsConfig,
        machine_path: &Path,
        template_root: PathBuf,
        token: Option<SecretValue>,
    ) -> Result<Self, SecretsError> {
        let mut settings = Self::new(template_root);
        settings.token = token.filter(|t| !t.is_empty());
        let invalid = |reason: &str| SecretsError::Config {
            path: machine_path.to_path_buf(),
            reason: reason.to_string(),
        };
        if let Some(section) = &machine.onepassword {
            if let Some(account) = &section.account {
                if !valid_account(account) {
                    return Err(invalid(
                        "`secrets.onepassword.account` may hold only [A-Za-z0-9._@:-] and may not start with `-`",
                    ));
                }
                settings.account = Some(account.clone());
            }
            if let Some(dir) = &section.config_path {
                if !dir.is_absolute() {
                    return Err(invalid(
                        "`secrets.onepassword.config_path` must be an absolute path",
                    ));
                }
                settings.config_dir = Some(dir.clone());
            }
            // #7519: a pin runs as given, so only an absolute path; no
            // working directory or `PATH` entry can choose it.
            if let Some(program) = &section.program {
                if !program.is_absolute() {
                    return Err(invalid(
                        "`secrets.onepassword.program` must be an absolute path",
                    ));
                }
                if !super::program::is_executable_file(program) {
                    return Err(super::program::not_installed(program.as_os_str()));
                }
                settings.program = program.clone().into_os_string();
            }
        }
        Ok(settings)
    }
}

/// Whether `account` is safe as one `op --account` argv word.
fn valid_account(account: &str) -> bool {
    !account.is_empty()
        && account.len() <= 255
        && !account.starts_with('-')
        && account
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '@' | ':' | '-'))
}

/// The variables, of `names`, the server removes from its own environment.
///
/// Why: the server keeps its first spawner's environment for life. An
/// inherited `OP_ACCOUNT` or `OP_CONFIG_DIR` would pick the account or
/// config the machine config did not; `OP_CONNECT_HOST` would send item
/// templates, values inside, to another server; and an inherited token
/// would reach `op` around the overlay.
/// What: every name starting `OP_` except `OP_SESSION_*`, the session an
/// operator's own `op signin` exported. The token is read with
/// [`token_from`] before it is removed.
/// Test: `onepassword_inherited_op_vars_keep_only_sessions`.
pub fn inherited_op_vars(names: impl IntoIterator<Item = OsString>) -> Vec<OsString> {
    names
        .into_iter()
        .filter(|name| {
            let name = name.as_encoded_bytes();
            name.starts_with(OP_PREFIX.as_bytes()) && !name.starts_with(SESSION_PREFIX.as_bytes())
        })
        .collect()
}

/// The service-account token from an environment lookup; empty is none.
///
/// Test: `onepassword_inherited_op_vars_keep_only_sessions`.
pub fn token_from(lookup: impl Fn(&str) -> Option<String>) -> Option<SecretValue> {
    lookup(SERVICE_ACCOUNT_TOKEN_ENV)
        .filter(|t| !t.is_empty())
        .map(SecretValue::new)
}
