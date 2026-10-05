//! Project identity: from a directory path to scopes and a backend.
//!
//! Why: a `secrets.*` request names its project by a directory. The server —
//! not the caller — derives owner and repository from that checkout's git
//! remote, so a caller cannot claim another project's vault by naming it.
//! S1 left the project-config location open; DOC-74 §6.1 puts the project's
//! `secrets:` section "inside the project's own tracked config", and the
//! tracked per-crate project config in this workspace lives at
//! `<repo>/.trusty-tools/<crate>.yaml` (`.trusty-tools/trusty-memory.yaml`).
//! What: [`PROJECT_CONFIG_SUBPATH`] (`.trusty-tools/trusty-secrets.yaml`, read
//! through S1's `load_project_at`), and [`ProjectContext`]: the checkout
//! root, its config, its [`ScopeSet`], and the backend resolved by the §6.1
//! precedence.
//! Test: `server_scopes_round_trip_over_a_real_socket`,
//! `server_project_without_a_remote_is_a_fixed_error`,
//! `server_project_config_overrides_the_project_vault`,
//! `server_vault_outside_the_project_is_refused`.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use super::errors::ErrorKind;
use super::router::State;
use crate::api::{SecretsError, VaultName};
use crate::store::config::{self, ProjectSecretsConfig, ResolvedConfig};
use crate::store::{ScopeSet, SecretBackend};

/// The project's `secrets:` config file, relative to the checkout root.
pub const PROJECT_CONFIG_SUBPATH: &str = ".trusty-tools/trusty-secrets.yaml";

/// One request's project, resolved.
///
/// Why: see the module docs.
/// What: built per request — no cache — so a changed remote or config is
/// seen on the next call.
/// Test: `server_scopes_round_trip_over_a_real_socket`.
#[derive(Debug)]
pub struct ProjectContext {
    root: PathBuf,
    config: Option<ProjectSecretsConfig>,
    scopes: ScopeSet,
}

impl ProjectContext {
    /// Resolve the checkout containing `dir`.
    ///
    /// What: `dir` must be absolute and a directory
    /// ([`ErrorKind::ProjectInvalid`]). The checkout root comes from
    /// `git rev-parse --show-toplevel`; outside a checkout the scopes are
    /// undetermined. The project config is read from the root, then
    /// [`ScopeSet::derive`] runs on the root with its `vault` override. Every
    /// failure folds to a fixed [`ErrorKind`].
    /// Test: `server_project_without_a_remote_is_a_fixed_error`,
    /// `server_project_path_must_be_an_absolute_directory`.
    pub fn resolve(dir: &Path) -> Result<Self, ErrorKind> {
        if !dir.is_absolute() || !dir.is_dir() {
            return Err(ErrorKind::ProjectInvalid);
        }
        let root = checkout_root(dir).ok_or(SecretsError::ScopeUndetermined {
            dir: dir.to_path_buf(),
            reason: "not inside a git checkout",
        })?;
        let config = config::load_project_at(&root.join(PROJECT_CONFIG_SUBPATH))?;
        let vault_override = config.as_ref().and_then(|c| c.vault.clone());
        let scopes = ScopeSet::derive(&root, vault_override)?;
        Ok(Self {
            root,
            config,
            scopes,
        })
    }

    /// The checkout root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The project config file path.
    pub fn config_path(&self) -> PathBuf {
        self.root.join(PROJECT_CONFIG_SUBPATH)
    }

    /// The project's scopes.
    pub fn scopes(&self) -> &ScopeSet {
        &self.scopes
    }

    /// Refuse a vault that is neither this project's vault nor its owner's.
    ///
    /// Test: `server_vault_outside_the_project_is_refused`.
    pub fn require_in_scope(&self, vault: &VaultName) -> Result<(), ErrorKind> {
        if self.scopes.lookup_order().any(|v| v == vault) {
            Ok(())
        } else {
            Err(ErrorKind::VaultOutOfScope)
        }
    }

    /// The backend this project uses (DOC-74 §6.1).
    ///
    /// What: project `secrets.backend`, else machine `default_backend`, else
    /// `keychain`; a machine config that does not parse fails closed.
    /// Test: `server_set_list_delete_round_trip_over_a_real_socket`.
    pub fn backend(&self, state: &State) -> Result<Arc<dyn SecretBackend>, ErrorKind> {
        let resolved = self.resolved_config(state)?;
        Ok((state.backends)(&resolved.backend)?)
    }

    /// The §6.1 resolution for this project and the server's machine config.
    pub fn resolved_config(&self, state: &State) -> Result<ResolvedConfig, ErrorKind> {
        let machine = config::load_machine_at(&state.settings.machine_config)?;
        Ok(config::resolve(self.config.as_ref(), machine.as_ref()))
    }
}

/// The top level of the checkout containing `dir`, or `None` outside one.
///
/// What: `git -C <dir> rev-parse --show-toplevel`; a missing `git` reads as
/// `None`. No network, and the output is never logged.
fn checkout_root(dir: &Path) -> Option<PathBuf> {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let root = String::from_utf8(output.stdout).ok()?;
    let root = root.trim_end_matches(['\n', '\r']);
    (!root.is_empty()).then(|| PathBuf::from(root))
}
