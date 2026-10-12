//! The sandbox consent override and backend refusal (#9793).
//!
//! Why: `scripts/sandbox_secrets_console.sh` runs this server with `$HOME`
//! moved into a throwaway directory, so `file` values land under the sandbox
//! home. Consent to `file` writes is read from the password database's home
//! (#7524 H1, ruling 74), which a sandbox must not touch, so every `set` into
//! `file` was refused there, and the default backend was the login Keychain.
//! What: [`SandboxEnv`] (the three variables read once at start),
//! [`sandbox_consent_config`] (the sandbox home's machine config, only when
//! every signal agrees), [`file_consent_config_for`] (that override, else the
//! account's own file), and [`opens_in_sandbox`] (only `file` opens under the
//! flag). Outside the sandbox, consent is unchanged.
//! Test: `sandbox_tests.rs`.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use trusty_common::DATA_DIR_OVERRIDE_ENV;
use trusty_common::credentials::{SANDBOX_ENV_VAR, sandbox_flag_set};

use crate::api::BackendId;
use crate::store::config::MACHINE_CONFIG_SUBPATH;

/// The variables the sandbox decision reads, captured once at start.
///
/// Why: a decision read per request could change under a running server;
/// one read at start keeps consent, enablement and doctor in agreement.
/// What: `TRUSTY_SANDBOX`, `TRUSTY_DATA_DIR_OVERRIDE` and `HOME`, raw.
/// Test: `sandbox_consent_is_none_without_the_flag`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SandboxEnv {
    /// `TRUSTY_SANDBOX`, as set.
    pub(crate) flag: Option<OsString>,
    /// `TRUSTY_DATA_DIR_OVERRIDE`, as set.
    pub(crate) data_dir_override: Option<OsString>,
    /// `HOME`, as set.
    pub(crate) home: Option<OsString>,
}

impl SandboxEnv {
    /// This process's sandbox variables.
    pub(crate) fn read() -> Self {
        Self {
            flag: std::env::var_os(SANDBOX_ENV_VAR),
            data_dir_override: std::env::var_os(DATA_DIR_OVERRIDE_ENV),
            home: std::env::var_os("HOME"),
        }
    }

    /// Whether the sandbox flag is on: `TRUSTY_SANDBOX` is exactly `1`.
    // #9793: trusty-common's reader, so every tool agrees on the rule (#9178).
    pub(crate) fn flag_set(&self) -> bool {
        sandbox_flag_set(self.flag.as_deref())
    }
}

/// The sandbox home's machine config, when this server runs in a sandbox.
///
/// Why: #9793, ruling Q1 — the flag alone is any spawner's to set, so the
/// override needs every signal the launcher pins, and fails closed on doubt.
/// What: `Some(<canonical HOME>/.trusty-tools/trusty-common/config.yaml)`
/// only when all hold: the flag is exactly `1`; `TRUSTY_DATA_DIR_OVERRIDE`
/// is an absolute, existing directory; `HOME` is absolute and exists; the
/// account home is known; and the canonical `HOME` differs from the
/// canonical account home (Q3). Anything else is `None`.
/// Test: `sandbox_consent_is_none_without_the_flag`,
/// `sandbox_consent_is_none_without_a_data_dir_override`,
/// `sandbox_consent_is_none_when_home_is_the_account_home`.
pub(crate) fn sandbox_consent_config(
    env: &SandboxEnv,
    account_home: Option<&Path>,
) -> Option<PathBuf> {
    if !env.flag_set() {
        return None;
    }
    let data_dir = absolute(env.data_dir_override.as_deref())?;
    if !data_dir.is_dir() {
        return None;
    }
    let home = absolute(env.home.as_deref())?.canonicalize().ok()?;
    let account = account_home?.canonicalize().ok()?;
    // #9793 (Q3): the real home is never a sandbox; consent stays ruling 74's.
    if home == account {
        return None;
    }
    Some(home.join(MACHINE_CONFIG_SUBPATH))
}

/// `value` as a path, when it is set and absolute.
fn absolute(value: Option<&OsStr>) -> Option<PathBuf> {
    let path = PathBuf::from(value?);
    path.is_absolute().then_some(path)
}

/// The machine config that consents to `file` writes.
///
/// Why: #7524 H1 and ruling 74 keep consent on the account's own file;
/// #9793 lets a sandbox home's file stand in for it, and nothing else.
/// What: [`sandbox_consent_config`], else `MACHINE_CONFIG_SUBPATH` under
/// `account_home`, else `None`.
/// Test: `sandbox_consent_is_none_when_home_is_the_account_home`,
/// `server_spawner_chosen_configs_are_not_consent_under_the_sandbox_flag`.
pub(crate) fn file_consent_config_for(
    env: &SandboxEnv,
    account_home: Option<&Path>,
) -> Option<PathBuf> {
    sandbox_consent_config(env, account_home)
        .or_else(|| account_home.map(|home| home.join(MACHINE_CONFIG_SUBPATH)))
}

/// Whether backend `id` may open under the sandbox flag.
///
/// Why: #9793, rulings Q2 and Q6 — a sandbox never touches the login
/// Keychain or a CLI backend's account.
/// What: `file` only, so a backend added later is off in a sandbox too.
/// Test: `server_keychain_is_never_opened_under_the_sandbox_flag`,
/// `server_sandbox_flag_does_not_enable_cli_backends`.
pub(crate) fn opens_in_sandbox(id: &BackendId) -> bool {
    id.as_str() == BackendId::FILE
}
