//! Config resolution: which backend, and which project vault (DOC-74 §6.1).
//!
//! Why: the machine names a `default_backend`; a project may name its own
//! `backend` and a `vault` override. A config that will not parse must fail
//! closed — reading it as "no config" would send writes to the default
//! backend or the derived vault, a different place than the operator chose.
//! What: the two `secrets:` section shapes, a loader that extracts the
//! top-level `secrets:` key from a YAML file, and [`resolve`].
//! - backend: project `secrets.backend`, else machine
//!   `secrets.default_backend`, else [`super::default_backend`] — `keychain`
//!   where a Keychain backend is compiled in, `file` elsewhere (#9326).
//! - vault: the machine `secrets.project_vaults` entry for the checkout's
//!   `<owner>/<repo>`; else project `secrets.vault`, which must sit under the
//!   remote's owner; otherwise the remote-derived project vault
//!   ([`super::ScopeSet::derive`]). #9328 (owner ruling 06 R2): the project
//!   file is tracked, so only the untracked machine file may pick a vault
//!   outside the owner.
//!
//! `copy` stays in-project (§13 Q6); its request type is
//! [`crate::api::methods::CopyRequest`] and the copy itself ships with S2.
//! Test: `config_backend_precedence_table`,
//! `config_corrupt_file_fails_closed`, `config_absent_section_is_none`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use super::platform::{self, Symlinks};
use crate::api::{BackendId, OwnerName, RepoName, SecretsError, VaultName};

/// Machine config file, relative to `$HOME` (DOC-74 §6.1).
pub const MACHINE_CONFIG_SUBPATH: &str = ".trusty-tools/trusty-common/config.yaml";

/// The machine-level `secrets:` section.
///
/// What: unknown keys are ignored so backend-specific settings (§6.2) can
/// sit beside it; a known key with a bad value fails the load.
// #9073: §6.2 adds backend settings; build it from `Default` and assign fields.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct MachineSecretsConfig {
    /// The backend projects use when they name none.
    #[serde(default)]
    pub default_backend: Option<BackendId>,
    /// Per-project vault overrides, keyed by the remote's `<owner>/<repo>`.
    // #9328: owner ruling 06 R2 — the only place a vault outside the
    // remote's owner may be chosen, because this file is not tracked.
    #[serde(default)]
    pub project_vaults: BTreeMap<String, VaultName>,
    /// `false` turns the credential access audit off on this machine.
    // #4567: DOC-45 C-7.10 — only this untracked file may suppress the audit.
    #[serde(default)]
    pub audit: Option<bool>,
    /// DOC-74 §6.2's 1Password section. Present, even as `{}`, it enables
    /// the `onepassword` backend on this machine (#7519).
    #[serde(default)]
    pub onepassword: Option<CliSettings>,
    /// DOC-74 §6.2's Keeper section. Present, even as `{}`, it enables the
    /// `keeper` backend on this machine (#7519 P3); it opens only when the
    /// section also pins `program` and `config_path`.
    #[serde(default)]
    pub keeper: Option<CliSettings>,
}

impl MachineSecretsConfig {
    /// Whether this machine enables the CLI-backed backend `id`.
    ///
    /// Why: #7519 P1 carry-over (a) — a delete sweeps every enabled CLI
    /// backend, and a CLI backend opens only when enabled, so no value is
    /// ever written where the sweep cannot reach it. Only this untracked
    /// file may enable one.
    /// What: `default_backend` names `id`, or `id`'s own section is present.
    /// A built-in backend (`keychain`, `file`) is never "enabled" here.
    /// Test: `onepassword_open_requires_machine_enablement`,
    /// `keeper_open_requires_machine_enablement`.
    pub fn enables(&self, id: &BackendId) -> bool {
        let section = match id.as_str() {
            BackendId::ONEPASSWORD => self.onepassword.is_some(),
            BackendId::KEEPER => self.keeper.is_some(),
            _ => return false,
        };
        section || self.default_backend.as_ref() == Some(id)
    }

    /// The `project_vaults` entry for `<owner>/<repo>`, if any.
    ///
    /// What: keys compare ASCII case-insensitively, as owner and repository
    /// names do; the first match in key order wins.
    /// Test: `scope_tracked_override_outside_the_owner_is_refused`.
    pub fn project_vault(&self, owner: &OwnerName, repo: &RepoName) -> Option<&VaultName> {
        let identity = format!("{owner}/{repo}");
        self.project_vaults
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(&identity))
            .map(|(_, vault)| vault)
    }
}

/// The project-level `secrets:` section.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ProjectSecretsConfig {
    /// This project's backend, overriding the machine default.
    #[serde(default)]
    pub backend: Option<BackendId>,
    /// An explicit project vault, overriding `trusty/<owner>/<repo>`. Must
    /// be `trusty/<owner>/<name>` under the remote's owner (#9328).
    #[serde(default)]
    pub vault: Option<VaultName>,
    /// Read only so the server can refuse `audit: false` here: a tracked file
    /// may never turn the credential access audit off (#4567).
    #[serde(default)]
    pub audit: Option<bool>,
    /// Read only so [`check_project_backend`] can refuse it here (#7519).
    #[serde(default)]
    pub account: Option<String>,
    /// Read only so [`check_project_backend`] can refuse it here (#7519).
    #[serde(default)]
    pub config_path: Option<PathBuf>,
    /// Read only so [`check_project_backend`] can refuse it here (#7519).
    #[serde(default)]
    pub program: Option<PathBuf>,
    /// DOC-74 §6.2's 1Password section; read only to refuse it (#7519).
    #[serde(default)]
    pub onepassword: Option<CliSettings>,
    /// DOC-74 §6.2's Keeper section; read only to refuse it (#7519).
    #[serde(default)]
    pub keeper: Option<CliSettings>,
}

/// The CLI settings DOC-74 §6.2 nests under a backend's own key.
///
/// Why: #7519 — a tracked project file must not set these in either the
/// top-level or the per-backend shape, so both are parsed to be refused.
/// What: parse-only; unknown keys beside them are ignored.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct CliSettings {
    /// The vendor CLI's account, e.g. `op --account`.
    #[serde(default)]
    pub account: Option<String>,
    /// The vendor CLI's config file or directory.
    #[serde(default)]
    pub config_path: Option<PathBuf>,
    /// The vendor CLI's executable, by absolute path (machine config only).
    // #7519: a pin skips the `PATH` search; a repository must not choose it.
    #[serde(default)]
    pub program: Option<PathBuf>,
}

/// The resolved backend and project-vault override for one invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ResolvedConfig {
    /// The backend to open.
    pub backend: BackendId,
    /// The tracked project-vault override, which [`super::ScopeSet::derive`]
    /// checks against the remote's owner.
    pub vault_override: Option<VaultName>,
}

/// `resolve(None, None)`: [`super::default_backend`] and no vault override.
/// Test: `config_backend_precedence_table`.
// #9328: `#[non_exhaustive]` blocks a literal, so other crates start here.
impl Default for ResolvedConfig {
    fn default() -> Self {
        resolve(None, None)
    }
}

/// Apply the DOC-74 §6.1 precedence.
///
/// What: an explicit backend always wins, whether or not this build can open
/// it; only the absence of any config falls to [`super::default_backend`].
/// Test: `config_backend_precedence_table`.
pub fn resolve(
    project: Option<&ProjectSecretsConfig>,
    machine: Option<&MachineSecretsConfig>,
) -> ResolvedConfig {
    let backend = project
        .and_then(|p| p.backend.clone())
        .or_else(|| machine.and_then(|m| m.default_backend.clone()))
        // #9326: the build's default — `file` only where no Keychain is compiled.
        .unwrap_or_else(super::default_backend);
    ResolvedConfig {
        backend,
        vault_override: project.and_then(|p| p.vault.clone()),
    }
}

/// Refuse a tracked project config that selects `file` on a Keychain build,
/// or that sets a vendor CLI's `account`, `config_path` or `program` on any
/// build.
///
/// Why: #9326, Architect ruling (basis ruling 06 R2, the #9328 class) — the
/// project file is tracked, so anyone who lands a change in the repository
/// could move every value to plaintext files. Where a Keychain is compiled
/// in, only the untracked machine config may select `file`. #7519, owner
/// ruling 2026-10-07: for the same reason it may not aim a vendor CLI at an
/// account or config directory of its choosing, nor choose the program run
/// as the CLI.
/// What: on a Keychain build, project `secrets.backend: file` is
/// [`SecretsError::TrackedBackendRefused`] naming `path` (the project file)
/// and the machine key to set, never the file's content. Any other project
/// backend, and every project backend on a build without a Keychain, passes.
/// Then, on every build, an `account`, `config_path` or `program` at the top
/// level or under `onepassword`/`keeper` is
/// [`SecretsError::TrackedCliSettingRefused`]
/// naming the key, never its value.
/// Test: `config_tracked_file_backend_is_refused_on_a_keychain_build`,
/// `server_tracked_file_backend_is_refused_on_a_keychain_build`,
/// `config_tracked_cli_settings_are_refused_on_every_build`,
/// `config_untracked_cli_settings_are_accepted`,
/// `server_tracked_cli_setting_is_refused_on_every_build`.
pub fn check_project_backend(
    project: Option<&ProjectSecretsConfig>,
    path: &Path,
) -> Result<(), SecretsError> {
    check_project_backend_for(project, path, super::backend::KEYCHAIN_COMPILED)
}

/// [`check_project_backend`] for a build that does or does not link a Keychain.
pub(crate) fn check_project_backend_for(
    project: Option<&ProjectSecretsConfig>,
    path: &Path,
    keychain_compiled: bool,
) -> Result<(), SecretsError> {
    let names_file = project
        .and_then(|p| p.backend.as_ref())
        .is_some_and(|b| b.as_str() == BackendId::FILE);
    if keychain_compiled && names_file {
        return Err(SecretsError::TrackedBackendRefused {
            path: path.to_path_buf(),
        });
    }
    // #7519: owner ruling 2026-10-07 — not Keychain-gated.
    match project.and_then(tracked_cli_setting) {
        Some(key) => Err(SecretsError::TrackedCliSettingRefused {
            path: path.to_path_buf(),
            key,
        }),
        None => Ok(()),
    }
}

/// Refuse a value write into `file` on a Keychain build unless the account's
/// own machine config selected `file`.
///
/// Why: #7524 H1, owner ruling item 74 — the Keychain ACL is a boundary
/// against same-user callers. Without this, any same-uid process could ask
/// the server to move Keychain values into 0600 plaintext files. Architect
/// ruling: a config path the spawner chooses — `--machine-config`, or one
/// under a redirected `$HOME` — is not that machine config.
/// What: with `keychain_compiled`, a `target` of `file` passes only when
/// `consent_config` is `Some` and loads (by [`load_machine_at`]) with
/// `default_backend: file`. No path, a missing or unreadable file, a parse
/// failure, no `secrets:` section, or another `default_backend` is
/// [`SecretsError::FileBackendNotSelected`]. Every other target, and every
/// target on a build without a Keychain, passes without reading the file.
/// Reads and deletes never call this.
/// Test: `config_value_write_into_file_needs_the_consent_config`,
/// `server_copy_to_file_is_refused_when_only_a_spawner_chosen_config_selects_it`,
/// `server_set_into_file_is_refused_when_only_a_spawner_chosen_config_selects_it`,
/// `server_copy_to_file_is_allowed_when_the_machine_config_selects_file`,
/// `server_copy_to_file_is_allowed_on_a_build_without_a_keychain`.
// #7524: only the server writes values on a caller's behalf.
#[cfg(feature = "server")]
pub(crate) fn check_value_write_for(
    target: &BackendId,
    consent_config: Option<&Path>,
    keychain_compiled: bool,
) -> Result<(), SecretsError> {
    let is_file = |id: &BackendId| id.as_str() == BackendId::FILE;
    if !keychain_compiled || !is_file(target) {
        return Ok(());
    }
    // #7524 H1: fail closed — any failure to read a `file` selection refuses.
    let consented = consent_config
        .and_then(|path| load_machine_at(path).ok().flatten())
        .and_then(|machine| machine.default_backend)
        .is_some_and(|id| is_file(&id));
    if consented {
        Ok(())
    } else {
        Err(SecretsError::FileBackendNotSelected)
    }
}

/// The first CLI setting `project` sets, as the key the refusal names.
fn tracked_cli_setting(project: &ProjectSecretsConfig) -> Option<&'static str> {
    // #7519: `program` too — a tracked pin would choose what runs as the CLI.
    let flags = |s: Option<&CliSettings>| {
        s.map_or([false; 3], |s| {
            [
                s.account.is_some(),
                s.config_path.is_some(),
                s.program.is_some(),
            ]
        })
    };
    let top = [
        project.account.is_some(),
        project.config_path.is_some(),
        project.program.is_some(),
    ];
    let sections = [
        (top, ["account", "config_path", "program"]),
        (
            flags(project.onepassword.as_ref()),
            [
                "onepassword.account",
                "onepassword.config_path",
                "onepassword.program",
            ],
        ),
        (
            flags(project.keeper.as_ref()),
            ["keeper.account", "keeper.config_path", "keeper.program"],
        ),
    ];
    sections
        .into_iter()
        .flat_map(|(set, keys)| set.into_iter().zip(keys))
        .find_map(|(set, key)| set.then_some(key))
}

/// The machine config path under the real `$HOME`.
pub fn machine_config_path() -> Result<PathBuf, SecretsError> {
    Ok(platform::home_dir()?.join(MACHINE_CONFIG_SUBPATH))
}

/// Largest project or machine config file the loaders read, in bytes.
///
/// What: 64 KiB, far above any real `secrets:` config. A larger file is
/// refused before it is read (#7524).
pub const MAX_CONFIG_BYTES: u64 = 64 * 1024;

/// Load the machine `secrets:` section from `path`.
///
/// What: the file may be a symlink — it is untracked, and dotfile managers
/// link it — but its target must be a regular file within
/// [`MAX_CONFIG_BYTES`].
/// Test: `config_corrupt_file_fails_closed`, `config_absent_section_is_none`,
/// `config_non_regular_and_linked_files_are_refused`.
pub fn load_machine_at(path: &Path) -> Result<Option<MachineSecretsConfig>, SecretsError> {
    load_section_at(path, Symlinks::Follow)
}

/// Load a project `secrets:` section from `path`.
///
/// What: the file must be a regular file within [`MAX_CONFIG_BYTES`], and
/// not a symlink, even to a file in the same checkout (#7524): the file is
/// tracked, so a symlink's target is the repository's choice, and one
/// project config has no use for a link.
/// Test: `config_corrupt_file_fails_closed`, `config_absent_section_is_none`,
/// `config_symlink_to_dev_zero_is_refused_promptly`,
/// `config_fifo_is_refused_without_blocking`,
/// `config_non_regular_and_linked_files_are_refused`.
pub fn load_project_at(path: &Path) -> Result<Option<ProjectSecretsConfig>, SecretsError> {
    load_section_at(path, Symlinks::Refuse)
}

/// Read the top-level `secrets:` key of a YAML file.
///
/// What: a missing file, an empty file, no `secrets:` key, or `secrets: null`
/// is `Ok(None)`. A file refused by [`platform::read_config`] (wrong type,
/// over [`MAX_CONFIG_BYTES`], not UTF-8, a refused symlink), a YAML syntax
/// error, a non-mapping top level, or a section that does not decode is
/// [`SecretsError::Config`]; another read failure is [`SecretsError::Io`].
/// Both report by position or rule only — a serde message can quote the
/// offending scalar.
/// Test: `config_corrupt_file_fails_closed`, `config_absent_section_is_none`,
/// `config_oversized_file_is_refused_and_a_normal_one_loads`.
fn load_section_at<T: DeserializeOwned>(
    path: &Path,
    symlinks: Symlinks,
) -> Result<Option<T>, SecretsError> {
    // #7524: bounded, type-checked read; a tracked symlink to /dev/zero hung here.
    let Some(raw) = platform::read_config(path, MAX_CONFIG_BYTES, symlinks)? else {
        return Ok(None);
    };
    if raw.trim().is_empty() {
        return Ok(None);
    }
    let invalid = |e: serde_yaml::Error, what: &str| SecretsError::Config {
        path: path.to_path_buf(),
        reason: match e.location() {
            Some(at) => format!("{what} at line {} column {}", at.line(), at.column()),
            None => what.to_string(),
        },
    };
    let doc: serde_yaml::Value =
        serde_yaml::from_str(&raw).map_err(|e| invalid(e, "the file does not parse as YAML"))?;
    if !matches!(doc, serde_yaml::Value::Mapping(_) | serde_yaml::Value::Null) {
        return Err(SecretsError::Config {
            path: path.to_path_buf(),
            reason: "the top level is not a mapping".to_string(),
        });
    }
    let section = match doc.get("secrets") {
        None | Some(serde_yaml::Value::Null) => return Ok(None),
        Some(section) => section.clone(),
    };
    serde_yaml::from_value(section)
        .map(Some)
        .map_err(|e| invalid(e, "the `secrets:` section does not decode"))
}
