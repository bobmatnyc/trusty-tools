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
//! root, its config, the machine config, its [`ScopeSet`], and the backend
//! resolved by the §6.1 precedence. #9328 (owner ruling 06 R2): the project
//! file is tracked, so its `vault` may only pick a vault under the remote's
//! owner; a wider override comes only from the untracked machine config.
//! #9326: likewise, on a Keychain build the project file may not select the
//! `file` backend; only the machine config may. #4567: nor may it turn the
//! credential audit off ([`ErrorKind::TrackedAuditRefused`]). #7524 H1: on a
//! Keychain build no request writes a value into `file` unless the account's
//! own machine config selected it; `ProjectContext::open_for_write` is that
//! one check.
//! Test: `server_scopes_round_trip_over_a_real_socket`,
//! `server_project_without_a_remote_is_a_fixed_error`,
//! `server_project_config_overrides_the_project_vault`,
//! `server_vault_outside_the_project_is_refused`,
//! `server_tracked_vault_override_outside_the_owner_is_refused`,
//! `server_non_github_remote_is_a_fixed_error`,
//! `server_tracked_file_backend_is_refused_on_a_keychain_build`,
//! `server_copy_to_file_is_refused_on_a_keychain_build_without_machine_selection`,
//! `server_copy_to_file_is_refused_when_only_a_spawner_chosen_config_selects_it`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::errors::ErrorKind;
use super::router::State;
use crate::api::{BackendId, SecretsError, VaultName};
use crate::store::config::{self, MachineSecretsConfig, ProjectSecretsConfig, ResolvedConfig};
use crate::store::{ScopeSet, SecretBackend, platform};

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
    machine: Option<MachineSecretsConfig>,
    scopes: ScopeSet,
    keychain_compiled: bool,
}

impl ProjectContext {
    /// Resolve the checkout containing `dir`.
    ///
    /// What: `dir` must be absolute and a directory
    /// ([`ErrorKind::ProjectInvalid`]). The checkout root comes from
    /// `git rev-parse --show-toplevel`; outside a checkout the scopes are
    /// undetermined. The project config is read from the root and the
    /// machine config from `state`, then [`ScopeSet::derive`] runs on the
    /// root with both (machine `project_vaults` first, then the tracked
    /// `vault`, which must sit under the remote's owner). A config that does
    /// not parse fails closed. Every failure folds to a fixed [`ErrorKind`].
    /// Test: `server_project_without_a_remote_is_a_fixed_error`,
    /// `server_project_path_must_be_an_absolute_directory`,
    /// `server_tracked_vault_override_outside_the_owner_is_refused`,
    /// `server_tracked_file_backend_is_refused_on_a_keychain_build`.
    pub fn resolve(state: &State, dir: &Path) -> Result<Self, ErrorKind> {
        if !dir.is_absolute() || !dir.is_dir() {
            return Err(ErrorKind::ProjectInvalid);
        }
        let root = checkout_root(dir).ok_or(SecretsError::ScopeUndetermined {
            dir: dir.to_path_buf(),
            reason: "not inside a git checkout",
        })?;
        let config_path = root.join(PROJECT_CONFIG_SUBPATH);
        let config = config::load_project_at(&config_path)?;
        // #9326: on a Keychain build only the machine config may pick `file`.
        // #7524: the build comes from `state`, as for `open_for_write`.
        config::check_project_backend_for(config.as_ref(), &config_path, state.keychain_compiled)?;
        // #4567: likewise only the machine config may turn the audit off.
        super::gate::check_tracked_audit(config.as_ref())?;
        let machine = config::load_machine_at(&state.settings.machine_config)?;
        // #9328: the tracked `vault` is checked against the remote's owner;
        // only the machine config may pick a vault outside it.
        let tracked = config.as_ref().and_then(|c| c.vault.clone());
        let scopes = ScopeSet::derive(&root, tracked, machine.as_ref())?;
        Ok(Self {
            root,
            config,
            machine,
            scopes,
            keychain_compiled: state.keychain_compiled,
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
    /// What: [`ScopeSet::require_in_scope`], the same check a pinned
    /// `secret://` reference passes (#9328).
    /// Test: `server_vault_outside_the_project_is_refused`.
    pub fn require_in_scope(&self, vault: &VaultName) -> Result<(), ErrorKind> {
        Ok(self.scopes.require_in_scope(vault)?)
    }

    /// The backend this project uses (DOC-74 §6.1).
    ///
    /// What: project `secrets.backend`, else machine `default_backend`, else
    /// `keychain`, from the configs read by [`Self::resolve`].
    /// Test: `server_set_list_delete_round_trip_over_a_real_socket`.
    pub fn backend(&self, state: &State) -> Result<Arc<dyn SecretBackend>, ErrorKind> {
        Ok((state.backends)(&self.resolved_config().backend)?)
    }

    /// Open backend `id` to write values into, after the `file` posture check.
    ///
    /// Why: #7524 H1, owner ruling item 74 — the Keychain ACL is a boundary
    /// against same-user callers, so a request may not move values into the
    /// plaintext `file` backend on its own say. Every value write (`set`, and
    /// `copy`'s destination) opens its backend here, so one check covers all.
    /// What: [`config::check_value_write_for`] with the server's
    /// `State::file_consent_config` — the account's own machine config, not
    /// `--machine-config` or one under `$HOME` — and the server's build: on a
    /// Keychain build, `file` is [`ErrorKind::FileBackendNotSelected`] unless
    /// that file's `default_backend` is `file`, and nothing is opened. Then
    /// the factory opens `id`.
    /// Test: `server_copy_to_file_is_refused_on_a_keychain_build_without_machine_selection`,
    /// `server_copy_to_file_is_refused_when_only_a_spawner_chosen_config_selects_it`,
    /// `server_set_into_file_is_refused_when_only_a_spawner_chosen_config_selects_it`,
    /// `server_copy_to_file_is_allowed_when_the_machine_config_selects_file`,
    /// `server_copy_to_file_is_allowed_on_a_build_without_a_keychain`.
    pub(crate) fn open_for_write(
        &self,
        state: &State,
        id: &BackendId,
    ) -> Result<Arc<dyn SecretBackend>, ErrorKind> {
        // #7524 H1: consent is the account's file, never the request's machine config.
        let consent = state.file_consent_config.as_deref();
        config::check_value_write_for(id, consent, self.keychain_compiled)?;
        Ok((state.backends)(id)?)
    }

    /// The machine config read by [`Self::resolve`], if the file exists.
    ///
    /// What: the untracked file `--machine-config` or `$HOME` names. #7519:
    /// the server reads CLI-backend enablement from the account's own file
    /// instead (`State::file_consent_config`).
    pub fn machine_config(&self) -> Option<&MachineSecretsConfig> {
        self.machine.as_ref()
    }

    /// The §6.1 resolution for this project and the server's machine config.
    pub fn resolved_config(&self) -> ResolvedConfig {
        config::resolve(self.config.as_ref(), self.machine.as_ref())
    }
}

/// The top level of the checkout containing `dir`, or `None` outside one.
///
/// What: `git -C <dir> rev-parse --show-toplevel` with the git redirect
/// variables removed; a missing `git` reads as `None`. No network, and the
/// output is never logged.
fn checkout_root(dir: &Path) -> Option<PathBuf> {
    // #9065: through the scrubbed helper, so an inherited `GIT_DIR` cannot
    // pick the checkout.
    let output = platform::git_command(dir)
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
