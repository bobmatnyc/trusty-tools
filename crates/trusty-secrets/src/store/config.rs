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

use super::platform;
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
}

impl MachineSecretsConfig {
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

/// Refuse a tracked project config that selects `file` on a Keychain build.
///
/// Why: #9326, Architect ruling (basis ruling 06 R2, the #9328 class) — the
/// project file is tracked, so anyone who lands a change in the repository
/// could move every value to plaintext files. Where a Keychain is compiled
/// in, only the untracked machine config may select `file`.
/// What: on a Keychain build, project `secrets.backend: file` is
/// [`SecretsError::TrackedBackendRefused`] naming `path` (the project file)
/// and the machine key to set, never the file's content. Any other project
/// backend, and every project backend on a build without a Keychain, passes.
/// Test: `config_tracked_file_backend_is_refused_on_a_keychain_build`,
/// `server_tracked_file_backend_is_refused_on_a_keychain_build`.
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
    Ok(())
}

/// The machine config path under the real `$HOME`.
pub fn machine_config_path() -> Result<PathBuf, SecretsError> {
    Ok(platform::home_dir()?.join(MACHINE_CONFIG_SUBPATH))
}

/// Load the machine `secrets:` section from `path`.
///
/// Test: `config_corrupt_file_fails_closed`, `config_absent_section_is_none`.
pub fn load_machine_at(path: &Path) -> Result<Option<MachineSecretsConfig>, SecretsError> {
    load_section_at(path)
}

/// Load a project `secrets:` section from `path`.
///
/// Test: `config_corrupt_file_fails_closed`, `config_absent_section_is_none`.
pub fn load_project_at(path: &Path) -> Result<Option<ProjectSecretsConfig>, SecretsError> {
    load_section_at(path)
}

/// Read the top-level `secrets:` key of a YAML file.
///
/// What: a missing file, an empty file, no `secrets:` key, or `secrets: null`
/// is `Ok(None)`. A read failure, a YAML syntax error, a non-mapping top
/// level, or a section that does not decode is [`SecretsError::Config`] (or
/// [`SecretsError::Io`] for the read), reported by position only — a
/// serde message can quote the offending scalar.
/// Test: `config_corrupt_file_fails_closed`, `config_absent_section_is_none`.
fn load_section_at<T: DeserializeOwned>(path: &Path) -> Result<Option<T>, SecretsError> {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(SecretsError::Io {
                path: path.to_path_buf(),
                source,
            });
        }
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
