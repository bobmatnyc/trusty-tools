//! The `secrets:` section of `~/.trusty-tools/trusty-mpm/config.yaml` (#7521,
//! DOC-74 §6.1), split into its own module for the same SLOC reason as
//! `untracked_sync`.
//!
//! Why: `tm secrets` needs two settings that outlive a single invocation —
//! which backend a project uses, and which group (vault namespace) its keys
//! live under. Owner ruling for slice 1: both live in the machine-level
//! `TrustyToolsConfig` file, keyed by an explicit group, rather than in a new
//! per-repo tracked file that this codebase has no precedent for. The
//! per-project override DOC-74 §6.1 sketches lands with #7519, when a second
//! backend exists to switch between.
//!
//! What: [`SecretsConfig`] (backend + group, both optional), a read/merge/write
//! pair over `trusty_common::crate_config`, and the two resolution rules —
//! backend defaults to `keychain`, group defaults to the git-remote-derived
//! `<owner>/<repo>` of the working directory. Neither this struct nor this
//! file ever holds a secret **value**; a group and a backend name are not
//! secret (DOC-74 §4 T-2).
//!
//! Test: `secrets_configure_writes_group_and_backend_to_config`,
//! `secrets_config_defaults_to_keychain`,
//! `secrets_group_resolution_prefers_the_configured_group`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{CRATE_NAME, TrustyToolsConfig};

/// The only backend slice 1 implements (#7519 adds `onepassword`/`keeper`).
pub const KEYCHAIN_BACKEND: &str = "keychain";

/// Failures reading, writing, or resolving the `secrets:` section.
///
/// Why: `tm secrets` must fail closed with an actionable message — never
/// guess a group, never fall back to another project's vault.
/// What: one variant per failure class. No variant can carry a secret value;
/// the only data any of them holds is a path, a group, or a backend name.
/// Test: `secrets_group_resolution_fails_without_a_remote`.
#[derive(Debug, thiserror::Error)]
pub enum SecretsConfigError {
    /// The config file could not be read or written.
    #[error("secrets config I/O error: {0}")]
    Config(#[from] trusty_common::crate_config::ConfigError),

    /// `$HOME` is unavailable, so the config path cannot be resolved.
    #[error("secrets config unavailable: no home directory")]
    HomeUnavailable,

    /// No group was configured and none could be derived from the git remote.
    #[error(
        "cannot determine the secrets group for {dir}: {reason}. Run `tm secrets configure \
         --provider keychain --group <owner>/<repo>` to set one explicitly."
    )]
    GroupUndetermined {
        /// The directory whose remote was probed.
        dir: PathBuf,
        /// Why the derivation failed (never a secret).
        reason: String,
    },

    /// The configured or supplied group is not a legal vault name.
    #[error("{0}")]
    InvalidGroup(String),
}

/// The `secrets:` block of trusty-mpm's config file.
///
/// Why: see the module docs — the two settings `tm secrets` needs between
/// invocations.
/// What: both fields are optional so an absent block resolves through
/// [`resolve_backend`] / [`resolve_group`] rather than forcing a write on
/// first use. `#[non_exhaustive]` matches [`TrustyToolsConfig`]'s own
/// semver posture (#5204); build one with [`SecretsConfig::new`].
/// Test: `secrets_configure_writes_group_and_backend_to_config`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct SecretsConfig {
    /// Backend id — `keychain` in slice 1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend: Option<String>,
    /// Vault namespace, conventionally `<owner>/<repo>` (DOC-74 §6.3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
}

impl SecretsConfig {
    /// Build a section from a backend and an optional group.
    pub fn new(backend: impl Into<String>, group: Option<String>) -> Self {
        Self {
            backend: Some(backend.into()),
            group,
        }
    }
}

/// The backend a vault uses: the configured one, else `keychain`.
///
/// Test: `secrets_config_defaults_to_keychain`.
pub fn resolve_backend(cfg: Option<&SecretsConfig>) -> String {
    cfg.and_then(|c| c.backend.clone())
        .unwrap_or_else(|| KEYCHAIN_BACKEND.to_string())
}

/// The group a vault uses: the explicit override, else the configured group,
/// else the git-remote-derived `<owner>/<repo>` of `dir`.
///
/// Why: a wrong group silently reads or writes another project's vault, so a
/// group that cannot be determined is an error naming the fix, never a guess.
/// What: validates whichever group wins through
/// `trusty_common::credentials::validate_group`, so an illegal name is
/// refused before it reaches a keychain service name or an index path.
/// Test: `secrets_group_resolution_prefers_the_configured_group`,
/// `secrets_group_resolution_fails_without_a_remote`.
pub fn resolve_group(
    explicit: Option<&str>,
    cfg: Option<&SecretsConfig>,
    dir: &Path,
) -> Result<String, SecretsConfigError> {
    let group = match explicit {
        Some(g) => g.to_string(),
        None => match cfg.and_then(|c| c.group.clone()) {
            Some(g) => g,
            None => trusty_common::github_path::derive_remote_repo(dir)
                .map(|r| r.owner_repo())
                .map_err(|e| SecretsConfigError::GroupUndetermined {
                    dir: dir.to_path_buf(),
                    reason: e.to_string(),
                })?,
        },
    };
    trusty_common::credentials::validate_group(&group)
        .map_err(|e| SecretsConfigError::InvalidGroup(e.to_string()))?;
    Ok(group)
}

/// Read the `secrets:` section from an explicit config path, FAILING CLOSED
/// on a config that will not parse.
///
/// Why: collapsing a read/parse failure to `None` makes an unreadable config
/// indistinguishable from "no `secrets:` section", so `add`/`list`/`remove`
/// would fall through to the git-remote-derived group and silently target a
/// different vault than the operator configured. Same inversion #6927 found in
/// the disk keep-list, and the same three-way answer as
/// [`super::load_disk_keep_list_at`].
/// What: absent file (or an empty one) → `Ok(None)`; a valid file → its
/// section; a read or parse failure → `Err`.
/// Test: `secrets_config_load_reports_a_corrupt_config_instead_of_defaulting`,
/// `secrets_configure_writes_group_and_backend_to_config`.
pub fn load_at(path: &Path) -> Result<Option<SecretsConfig>, SecretsConfigError> {
    Ok(trusty_common::crate_config::load_at::<TrustyToolsConfig>(path)?.and_then(|c| c.secrets))
}

/// Read the `secrets:` section from the machine config file.
///
/// What: an unknown home is "no config" (`Ok(None)`), the same answer an
/// absent file gives; every other failure propagates.
/// Test: `secrets_config_load_reports_a_corrupt_config_instead_of_defaulting`
/// covers the fail-closed path through [`load_at`].
pub fn load() -> Result<Option<SecretsConfig>, SecretsConfigError> {
    match trusty_common::crate_config::crate_config_path(CRATE_NAME) {
        Some(path) => load_at(&path),
        None => Ok(None),
    }
}

/// Backend and group for one invocation, resolved together and fail-closed.
///
/// Why: the single entry point every `tm secrets` verb uses, so a corrupt
/// config cannot reach the group resolver through one caller while another
/// propagates it.
/// What: loads the section (propagating a read/parse failure), then resolves
/// the backend and the group. Holds no value.
/// Test: `secrets_config_load_reports_a_corrupt_config_instead_of_defaulting`,
/// `secrets_group_resolution_prefers_the_configured_group`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedSecrets {
    /// Backend id — `keychain` in slice 1.
    pub backend: String,
    /// The vault namespace this invocation reads and writes.
    pub group: String,
}

/// Resolve backend and group from the config file at `path`.
///
/// Test: `secrets_config_load_reports_a_corrupt_config_instead_of_defaulting`.
pub fn resolve_at(
    path: &Path,
    explicit_group: Option<&str>,
    dir: &Path,
) -> Result<ResolvedSecrets, SecretsConfigError> {
    let cfg = load_at(path)?;
    Ok(ResolvedSecrets {
        backend: resolve_backend(cfg.as_ref()),
        group: resolve_group(explicit_group, cfg.as_ref(), dir)?,
    })
}

/// Resolve backend and group from the machine config file.
///
/// Test: `secrets_config_load_reports_a_corrupt_config_instead_of_defaulting`
/// covers the shared [`resolve_at`] body.
pub fn resolve(
    explicit_group: Option<&str>,
    dir: &Path,
) -> Result<ResolvedSecrets, SecretsConfigError> {
    let cfg = load()?;
    Ok(ResolvedSecrets {
        backend: resolve_backend(cfg.as_ref()),
        group: resolve_group(explicit_group, cfg.as_ref(), dir)?,
    })
}

/// Merge `cfg` into the config file at `path`, preserving every other setting.
///
/// Why: `tm secrets configure`'s only write path. It must not drop the
/// operator's unrelated settings, so it reads the whole config, replaces one
/// section, and writes the whole config back.
/// What: returns the path written.
/// Test: `secrets_configure_writes_group_and_backend_to_config`.
pub fn save_at(path: &Path, cfg: SecretsConfig) -> Result<PathBuf, SecretsConfigError> {
    let mut config = trusty_common::crate_config::load_at::<TrustyToolsConfig>(path)?
        .unwrap_or_else(TrustyToolsConfig::default);
    config.secrets = Some(cfg);
    Ok(trusty_common::crate_config::save_at(path, &config)?)
}

/// Merge `cfg` into the machine config file.
///
/// Test: the live smoke in PR #7521's report.
pub fn save(cfg: SecretsConfig) -> Result<PathBuf, SecretsConfigError> {
    let path = trusty_common::crate_config::crate_config_path(CRATE_NAME)
        .ok_or(SecretsConfigError::HomeUnavailable)?;
    save_at(&path, cfg)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: `configure` must persist both settings and leave the rest of the
    /// config untouched — the regression a naive whole-file overwrite causes.
    /// Test: itself.
    #[test]
    fn secrets_configure_writes_group_and_backend_to_config() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("config.yaml");

        let pre = TrustyToolsConfig {
            default_model: Some("opus".to_string()),
            ..Default::default()
        };
        trusty_common::crate_config::save_at(&path, &pre).unwrap();

        save_at(
            &path,
            SecretsConfig::new(KEYCHAIN_BACKEND, Some("bobmatnyc/trusty-tools".to_string())),
        )
        .unwrap();

        let loaded = load_at(&path)
            .expect("a valid config must load")
            .expect("secrets section must round-trip");
        assert_eq!(loaded.backend.as_deref(), Some(KEYCHAIN_BACKEND));
        assert_eq!(loaded.group.as_deref(), Some("bobmatnyc/trusty-tools"));

        let whole = trusty_common::crate_config::load_at::<TrustyToolsConfig>(&path)
            .unwrap()
            .unwrap();
        assert_eq!(
            whole.default_model.as_deref(),
            Some("opus"),
            "an unrelated setting must survive the merge"
        );
    }

    /// Why: a project with no `secrets:` block still resolves to the shipped
    /// default backend (DOC-74 §6.1, owner ruling G-2).
    /// Test: itself.
    #[test]
    fn secrets_config_defaults_to_keychain() {
        assert_eq!(resolve_backend(None), KEYCHAIN_BACKEND);
        assert_eq!(
            resolve_backend(Some(&SecretsConfig::default())),
            KEYCHAIN_BACKEND
        );
        assert_eq!(
            resolve_backend(Some(&SecretsConfig::new("onepassword", None))),
            "onepassword"
        );
    }

    /// Why: an explicit `--group` outranks config, which outranks the git
    /// remote — and the derivation is never reached when either is present.
    /// Test: itself.
    #[test]
    fn secrets_group_resolution_prefers_the_configured_group() {
        let tmp = tempfile::TempDir::new().unwrap();
        let cfg = SecretsConfig::new(KEYCHAIN_BACKEND, Some("owner/from-config".to_string()));

        assert_eq!(
            resolve_group(Some("owner/explicit"), Some(&cfg), tmp.path()).unwrap(),
            "owner/explicit"
        );
        assert_eq!(
            resolve_group(None, Some(&cfg), tmp.path()).unwrap(),
            "owner/from-config"
        );
        assert!(
            resolve_group(Some("../escape"), None, tmp.path()).is_err(),
            "an illegal group must be refused, not sanitized"
        );
    }

    /// Why: a config that will not parse must not read as "no `secrets:`
    /// section". Collapsing it to `None` sends `add`/`list`/`remove` to the
    /// git-remote-derived group — a DIFFERENT vault than the operator
    /// configured — with nothing said about it. The group resolver must never
    /// be consulted at all when the load failed, which is what the error
    /// VARIANT proves here: fail-open reached `resolve_group` and returned
    /// `GroupUndetermined` (or, in a checkout with a remote, a group).
    /// Test: itself.
    #[test]
    fn secrets_config_load_reports_a_corrupt_config_instead_of_defaulting() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("config.yaml");
        std::fs::write(&path, "secrets:\n  backend: [unclosed\n").unwrap();

        let err = load_at(&path).unwrap_err();
        assert!(
            matches!(err, SecretsConfigError::Config(_)),
            "a corrupt config must surface as a config error, got: {err}"
        );

        let err = resolve_at(&path, None, tmp.path()).unwrap_err();
        assert!(
            matches!(err, SecretsConfigError::Config(_)),
            "resolution must stop at the load, never reach the group resolver: {err}"
        );

        // An ABSENT file is still the documented "no configuration" answer.
        let missing = tmp.path().join("absent.yaml");
        assert_eq!(load_at(&missing).unwrap(), None);
    }

    /// Why: fail-closed — a directory with no git remote and no configured
    /// group must error with the fix, not silently pick a vault.
    /// Test: itself.
    #[test]
    fn secrets_group_resolution_fails_without_a_remote() {
        let tmp = tempfile::TempDir::new().unwrap();
        let err = resolve_group(None, None, tmp.path()).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("tm secrets configure"),
            "the error must name the fix: {msg}"
        );
    }
}
