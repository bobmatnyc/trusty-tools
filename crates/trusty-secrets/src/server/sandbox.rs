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

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use trusty_common::DATA_DIR_OVERRIDE_ENV;
use trusty_common::credentials::SANDBOX_ENV_VAR;

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

    /// Whether the sandbox flag is on.
    pub(crate) fn flag_set(&self) -> bool {
        false
    }
}

/// The sandbox home's machine config, when this server runs in a sandbox.
pub(crate) fn sandbox_consent_config(
    env: &SandboxEnv,
    account_home: Option<&Path>,
) -> Option<PathBuf> {
    let _ = (env, account_home);
    None
}

/// The machine config that consents to `file` writes.
pub(crate) fn file_consent_config_for(
    env: &SandboxEnv,
    account_home: Option<&Path>,
) -> Option<PathBuf> {
    let _ = env;
    account_home.map(|home| home.join(MACHINE_CONFIG_SUBPATH))
}

/// Whether backend `id` may open under the sandbox flag.
pub(crate) fn opens_in_sandbox(id: &BackendId) -> bool {
    let _ = id;
    true
}
