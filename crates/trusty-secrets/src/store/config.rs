//! Config resolution: which backend, and which project vault (DOC-74 §6.1).
//!
//! Why: the machine names a `default_backend`; a project may name its own
//! `backend` and a `vault` override. A config that will not parse must fail
//! closed — reading it as "no config" would send writes to the default
//! backend or the derived vault, a different place than the operator chose.
//! What: the two `secrets:` section shapes, a loader that extracts the
//! top-level `secrets:` key from a YAML file, and [`resolve`].
//! - backend: project `secrets.backend`, else machine
//!   `secrets.default_backend`, else `keychain`.
//! - vault: project `secrets.vault` when set; otherwise the remote-derived
//!   project vault ([`super::ScopeSet::derive`]).
//!
//! `copy` stays in-project (§13 Q6); its request type is
//! [`crate::api::methods::CopyRequest`] and the copy itself ships with S2.
//! Test: `config_backend_precedence_table`,
//! `config_corrupt_file_fails_closed`, `config_absent_section_is_none`.

use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use super::platform;
use crate::api::{BackendId, SecretsError, VaultName};

/// Machine config file, relative to `$HOME` (DOC-74 §6.1).
pub const MACHINE_CONFIG_SUBPATH: &str = ".trusty-tools/trusty-common/config.yaml";

/// The machine-level `secrets:` section.
///
/// What: unknown keys are ignored so backend-specific settings (§6.2) can
/// sit beside it; a known key with a bad value fails the load.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MachineSecretsConfig {
    /// The backend projects use when they name none.
    #[serde(default)]
    pub default_backend: Option<BackendId>,
}

/// The project-level `secrets:` section.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectSecretsConfig {
    /// This project's backend, overriding the machine default.
    #[serde(default)]
    pub backend: Option<BackendId>,
    /// An explicit project vault, overriding `trusty/<owner>/<repo>`.
    #[serde(default)]
    pub vault: Option<VaultName>,
}

/// The resolved backend and project-vault override for one invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedConfig {
    /// The backend to open.
    pub backend: BackendId,
    /// The project-vault override, passed to [`super::ScopeSet::derive`].
    pub vault_override: Option<VaultName>,
}

/// Apply the DOC-74 §6.1 precedence.
///
/// Test: `config_backend_precedence_table`.
pub fn resolve(
    project: Option<&ProjectSecretsConfig>,
    machine: Option<&MachineSecretsConfig>,
) -> ResolvedConfig {
    let backend = project
        .and_then(|p| p.backend.clone())
        .or_else(|| machine.and_then(|m| m.default_backend.clone()))
        .unwrap_or_else(BackendId::keychain);
    ResolvedConfig {
        backend,
        vault_override: project.and_then(|p| p.vault.clone()),
    }
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
