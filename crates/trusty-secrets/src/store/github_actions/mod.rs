//! [`GitHubActionsBackend`]: a write-only sync target over the GitHub CLI,
//! `gh` (#9069, DOC-74 §15.4).
//!
//! Why: an operator pushes a project key's value into the repository's
//! Actions secrets. The value reaches `gh` on stdin only (§8.2), and an
//! Actions secret can never be read back, so the write is proven by the
//! secret's metadata instead (D8): exit 0 alone never counts.
//! What, for a key `K` and the checkout's origin repository `<o>/<r>`
//! (D6; the vault name is never used to pick it):
//! 1. baseline: `gh api --method GET repos/<o>/<r>/actions/secrets/K`.
//!    A 404 is "absent"; a signed-out `gh` is
//!    [`SecretsError::BackendLocked`] and nothing is pushed (G4); any other
//!    failure is an error.
//! 2. push: `gh secret set K --repo github.com/<o>/<r> --app actions` with
//!    the value on stdin and no `--body`.
//! 3. confirm: the baseline read again. Only an `updated_at` present and
//!    newer than the baseline (or present where the baseline was absent)
//!    counts; anything else is an error whose text says the value may have
//!    landed.
//!
//! Every run removes the inherited `GH_*` and token variables
//! ([`settings`]'s `steering_vars`), and gets `GH_CONFIG_DIR` only from the
//! machine config (D7). `get`, `delete` and `list_names` are unsupported, so
//! no read-back and no delete sweep ever reaches a repository (D12).
//! Test: `github_actions_tests.rs` beside this module, and
//! `server/sync_target_tests.rs` for the `copy` path.

mod markers;
mod settings;
#[cfg(test)]
pub(crate) mod shim;

#[cfg(test)]
mod github_actions_tests;

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Deserialize;

use super::cli::{CliCommand, CliRun, CliSpec, Verdict};
use super::config::{self, MachineSecretsConfig};
use super::{Capabilities, RemoteIdentity, SecretBackend};
use crate::api::{BackendId, SecretKey, SecretValue, SecretsError, VaultName};

pub use settings::{DEFAULT_TIMEOUT, GitHubActionsSettings};

/// The runner's facts about `gh`: id, program, hints and markers.
const SPEC: CliSpec = CliSpec::new(BackendId::GITHUB_ACTIONS, "gh", DEFAULT_TIMEOUT)
    .with_hints(
        "install the GitHub CLI, or set `secrets.github_actions.program` in the machine \
         config to the absolute path of `gh`",
        "log in with `gh auth login` as an account that may write this repository's \
         Actions secrets",
    )
    .with_markers(markers::MISSING, markers::LOCKED);

const BAD_NAME: &str = "a GitHub Actions secret name holds only [A-Za-z0-9_], does not start \
     with a digit, and does not start with `GITHUB_`";
const UNREADABLE: &str = "the secret's metadata from `gh api` could not be interpreted";
const NOT_SHOWN: &str = "the push exited 0, but the secret's `updated_at` did not advance; \
     the value may not have landed";
const UNCONFIRMED: &str =
    "the push ran, but the read that confirms it failed; the value may have landed";

/// The GitHub Actions sync target for one repository.
///
/// Why: see the module docs.
/// What: stateless apart from its settings and destination; every call
/// runs `gh` afresh, and no value is kept.
/// Test: `github_push_writes_the_value_on_stdin_only`.
#[derive(Debug)]
pub struct GitHubActionsBackend {
    settings: GitHubActionsSettings,
    identity: RemoteIdentity,
}

/// [`SecretsError::CliNotInstalled`] for `program`, with the install hint.
fn not_installed(program: &OsStr) -> SecretsError {
    SecretsError::CliNotInstalled {
        program: program.to_string_lossy().into_owned(),
        hint: SPEC.install_hint,
    }
}

/// The metadata `gh api` returns for one Actions secret; values never appear.
#[derive(Deserialize)]
struct SecretMeta {
    name: String,
    updated_at: String,
}

impl GitHubActionsBackend {
    /// A backend pushing into `identity`'s repository as `settings` say.
    pub fn new(settings: GitHubActionsSettings, identity: RemoteIdentity) -> Self {
        Self { settings, identity }
    }

    /// One `gh` invocation with the steering variables removed.
    ///
    /// What: a `program` that is not absolute is
    /// [`SecretsError::CliNotInstalled`], and nothing is spawned.
    fn command(&self, vault: &VaultName, key: &SecretKey) -> Result<CliCommand, SecretsError> {
        let s = &self.settings;
        if !Path::new(&s.program).is_absolute() {
            return Err(not_installed(&s.program));
        }
        let mut command = CliCommand::new(SPEC)
            .program(&s.program)
            .timeout(s.timeout)
            .target(vault, key);
        // #9069: D7 — `gh`'s own login and host, never the spawner's.
        let inherited = std::env::vars_os().map(|(name, _)| name);
        for name in settings::steering_vars(inherited) {
            command = command.env_remove(name);
        }
        if let Some(dir) = &s.config_dir {
            command = command.env(settings::CONFIG_DIR_ENV, dir);
        }
        Ok(command)
    }

    fn failed(&self, vault: &VaultName, key: &SecretKey, reason: &'static str) -> SecretsError {
        SecretsError::Backend {
            backend: BackendId::GITHUB_ACTIONS.to_string(),
            vault: vault.to_string(),
            key: key.to_string(),
            reason: reason.to_string(),
        }
    }

    /// The error a run that is neither `Ok` nor handled maps to.
    fn run_error(&self, run: CliRun, vault: &VaultName, key: &SecretKey) -> SecretsError {
        match run.into_value() {
            Err(e) => e,
            Ok(_) => self.failed(vault, key, UNREADABLE),
        }
    }

    /// The secret's `updated_at`, or `None` when the repository has no such
    /// secret.
    ///
    /// What: a 404 is `None`; a signed-out `gh` is
    /// [`SecretsError::BackendLocked`]; any other failure, and exit 0 with
    /// text that is not this secret's metadata, is an error.
    fn updated_at(
        &self,
        vault: &VaultName,
        key: &SecretKey,
        name: &str,
    ) -> Result<Option<String>, SecretsError> {
        let RemoteIdentity { owner, repo, .. } = &self.identity;
        let path = format!("repos/{owner}/{repo}/actions/secrets/{name}");
        let run = self
            .command(vault, key)?
            .args(["api", "--method", "GET", &path])
            .run()?;
        match run.verdict {
            Verdict::Ok => match serde_json::from_str::<SecretMeta>(run.stdout.expose()) {
                Ok(meta)
                    if meta.name.eq_ignore_ascii_case(name) && is_timestamp(&meta.updated_at) =>
                {
                    Ok(Some(meta.updated_at))
                }
                _ => Err(self.failed(vault, key, UNREADABLE)),
            },
            Verdict::Missing => Ok(None),
            _ => Err(self.run_error(run, vault, key)),
        }
    }

    /// `gh secret set`, the value on stdin and nowhere else.
    fn push(
        &self,
        vault: &VaultName,
        key: &SecretKey,
        name: &str,
        value: &SecretValue,
    ) -> Result<(), SecretsError> {
        let RemoteIdentity { owner, repo, .. } = &self.identity;
        let repo_arg = format!("github.com/{owner}/{repo}");
        // #9069: DOC-74 §8.2 — no `--body`; the runner refuses the value in
        // argv and the environment before spawning.
        let run = self
            .command(vault, key)?
            .args([
                "secret", "set", name, "--repo", &repo_arg, "--app", "actions",
            ])
            .run_with_stdin(value)?;
        match run.verdict {
            Verdict::Ok => Ok(()),
            _ => Err(self.run_error(run, vault, key)),
        }
    }
}

/// `key` as a GitHub Actions secret name: uppercased, `[A-Z0-9_]`, not
/// starting with a digit or `GITHUB_`.
///
/// Why: a trusty key may hold `.` and `-`, which GitHub refuses; checking
/// first keeps a push that cannot land from spawning anything.
/// Test: `github_key_names_github_refuses_never_spawn`.
fn github_name(key: &SecretKey) -> Option<String> {
    let name = key.as_str().to_ascii_uppercase();
    let valid = name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !name.starts_with(|c: char| c.is_ascii_digit())
        && !name.starts_with("GITHUB_");
    valid.then_some(name)
}

/// Whether `text` is a UTC timestamp `YYYY-MM-DDTHH:MM:SSZ`, the form whose
/// text order is its time order.
fn is_timestamp(text: &str) -> bool {
    let shape = b"0000-00-00T00:00:00Z";
    text.len() == shape.len()
        && text.bytes().zip(shape).all(|(byte, want)| match want {
            b'0' => byte.is_ascii_digit(),
            other => byte == *other,
        })
}

/// Whether the confirming read shows the push: present after, and absent
/// or older before.
///
/// Why: D8 — exit 0 alone never counts. `updated_at` has one-second
/// resolution, so a push in the same second as the baseline reads as not
/// shown and fails closed.
/// Test: `github_exit_zero_without_a_newer_updated_at_is_a_failure`.
fn advanced(before: Option<&str>, after: Option<&str>) -> bool {
    match (before, after) {
        (_, None) => false,
        (None, Some(_)) => true,
        (Some(before), Some(after)) => after > before,
    }
}

impl SecretBackend for GitHubActionsBackend {
    fn id(&self) -> BackendId {
        BackendId::github_actions()
    }

    /// `WRITE | SYNC_TARGET`: never read back, never listed, never swept.
    fn capabilities(&self) -> Capabilities {
        Capabilities::WRITE | Capabilities::SYNC_TARGET
    }

    fn get(
        &self,
        _vault: &VaultName,
        _key: &SecretKey,
    ) -> Result<Option<SecretValue>, SecretsError> {
        Err(SecretsError::Unsupported {
            backend: BackendId::GITHUB_ACTIONS.to_string(),
            operation: "get",
        })
    }

    /// Push `value` as the Actions secret `key`; see the module docs.
    ///
    /// Test: `github_push_writes_the_value_on_stdin_only`,
    /// `github_exit_zero_without_a_newer_updated_at_is_a_failure`,
    /// `github_signed_out_preflight_is_backend_locked_and_never_pushes`,
    /// `github_verify_read_failure_after_push_fails_the_key`.
    fn set(
        &self,
        vault: &VaultName,
        key: &SecretKey,
        value: &SecretValue,
    ) -> Result<(), SecretsError> {
        let name = github_name(key).ok_or_else(|| self.failed(vault, key, BAD_NAME))?;
        // #9069: G4 — the baseline read also proves the login before a push.
        let before = self.updated_at(vault, key, &name)?;
        self.push(vault, key, &name, value)?;
        // #9069: D8 — fail closed; a deadline keeps its own kind and text.
        let after = self.updated_at(vault, key, &name).map_err(|e| match e {
            SecretsError::DeadlineExceeded { .. } => e,
            _ => self.failed(vault, key, UNCONFIRMED),
        })?;
        if advanced(before.as_deref(), after.as_deref()) {
            Ok(())
        } else {
            Err(self.failed(vault, key, NOT_SHOWN))
        }
    }

    fn delete(&self, _vault: &VaultName, _key: &SecretKey) -> Result<bool, SecretsError> {
        // #9069: D12 — a delete never removes a pushed copy.
        Err(SecretsError::Unsupported {
            backend: BackendId::GITHUB_ACTIONS.to_string(),
            operation: "delete",
        })
    }
}

/// The settings this machine's config gives the sync target.
///
/// What: reads `machine_config`; without a `secrets.github_actions:`
/// section, [`SecretsError::BackendNotEnabled`]; then
/// [`GitHubActionsSettings::from_machine`]. Spawns nothing.
fn settings_at(
    machine_config: &Path,
    dirs: &[PathBuf],
) -> Result<GitHubActionsSettings, SecretsError> {
    let id = BackendId::github_actions();
    let machine: Option<MachineSecretsConfig> = config::load_machine_at(machine_config)?;
    let Some(machine) = machine.filter(|m| m.enables(&id)) else {
        return Err(SecretsError::BackendNotEnabled {
            backend: id.to_string(),
        });
    };
    GitHubActionsSettings::from_machine(&machine, machine_config, dirs)
}

/// Open the sync target for `identity`'s repository when the machine
/// config enables it.
///
/// Why: ruling 74 — only the account's own machine config enables a push
/// and names the program; D6 — the destination is the origin remote.
/// What: [`settings_at`], then a backend over `identity`. Spawns nothing.
/// Test: `github_open_requires_the_machine_section`.
pub fn open_in(
    machine_config: &Path,
    identity: &RemoteIdentity,
    dirs: &[PathBuf],
) -> Result<Arc<dyn SecretBackend>, SecretsError> {
    let settings = settings_at(machine_config, dirs)?;
    Ok(Arc::new(GitHubActionsBackend::new(
        settings,
        identity.clone(),
    )))
}

/// [`open_in`]'s checks without a destination, for doctor.
#[cfg(feature = "server")]
pub(crate) fn ready(machine_config: &Path, dirs: &[PathBuf]) -> Result<Capabilities, SecretsError> {
    settings_at(machine_config, dirs).map(|_| Capabilities::WRITE | Capabilities::SYNC_TARGET)
}
