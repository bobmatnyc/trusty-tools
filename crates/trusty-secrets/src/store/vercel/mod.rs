//! [`VercelBackend`]: a write-only sync target over the Vercel CLI (#9069
//! PR 2, DOC-74 §15.4).
//!
//! Why: an operator pushes a project key's value into a Vercel project's
//! environment variables. The value reaches `vercel` on stdin only (§8.2);
//! a sensitive variable can never be read back, so the write is proven by
//! the listing instead (D8): exit 0 alone never counts.
//! What, for a key `K`, each run in a fresh empty 0700 directory passed as
//! `--cwd` with `VERCEL_ORG_ID`/`VERCEL_PROJECT_ID` from the account's
//! mapping (V4) and every inherited `VERCEL_*` removed:
//! 1. baseline: `vercel env ls <target>` for `production`, then `preview`.
//!    It proves the login and the link; a signed-out CLI is
//!    [`SecretsError::BackendLocked`] and nothing is pushed.
//! 2. push: `vercel env add K <target>` + [`ADD_FLAGS`] per target, the
//!    value on stdin. A failure stops the key there and says a target may
//!    have been written (V3). `development` is never pushed (V2, D10).
//! 3. confirm: the listings again; each must name `K` (V6).
//! 4. a `K` the baseline already listed was updated in place by `--force`
//!    (V5, never remove-then-add), but a listing cannot show that the value
//!    changed, so the key fails with a fixed reason (D9 (a), pending the
//!    live check).
//!
//! `get`, `delete` and `list_names` are unsupported, so no read-back and no
//! delete sweep ever reaches a project (D12).
//! Test: `vercel_tests.rs` beside this module, and
//! `server/sync_target_tests.rs` for the `copy` path.

mod listing;
mod markers;
mod settings;
#[cfg(test)]
pub(crate) mod shim;

#[cfg(test)]
mod vercel_tests;

use std::ffi::OsStr;
use std::path::Path;
use std::sync::Arc;

use super::cli::{CliCommand, CliSpec, Verdict, WorkDir};
use super::config::{self, MachineSecretsConfig};
use super::{Capabilities, RemoteIdentity, SecretBackend};
use crate::api::{BackendId, SecretKey, SecretValue, SecretsError, VaultName};

pub use settings::{DEFAULT_TIMEOUT, VercelLink, VercelSettings};

/// The runner's facts about `vercel`: id, program, hints and markers.
const SPEC: CliSpec = CliSpec::new(BackendId::VERCEL, "vercel", DEFAULT_TIMEOUT)
    .with_hints(
        "install the Vercel CLI and set `secrets.vercel.program` in the machine config to \
         its absolute path",
        "log in with `vercel login` as an account that may write this project's \
         environment variables",
    )
    .with_markers(markers::MISSING, markers::LOCKED);

/// The targets a push writes, in order. `development` is never one (D10).
pub(crate) const TARGETS: [&str; 2] = ["production", "preview"];

/// The flags each `env add` carries. UNCONFIRMED (D9): the #9069 live check
/// pins that `--force` overwrites in place and that `--sensitive` exists.
const ADD_FLAGS: [&str; 2] = ["--sensitive", "--force"];

const BAD_NAME: &str =
    "a Vercel environment variable name holds only [A-Za-z0-9_] and does not start with a digit";
const UNREADABLE: &str = "the `vercel env ls` listing could not be interpreted";
const PUSH_FAILED: &str = "`vercel env add` failed or timed out on one target; a target may \
     have been written, so check both production and preview";
const UNCONFIRMED: &str =
    "the push ran, but the listing that confirms it failed; the value may have been written";
const NOT_SHOWN: &str = "the push exited 0, but `vercel env ls` does not list the variable on \
     every target; the value may not have landed";
const UPDATE_UNPROVEN: &str = "the variable already existed on Vercel and was overwritten in \
     place, but `vercel env ls` cannot show that its value changed; check the project in Vercel";

/// The Vercel sync target for one project.
///
/// Why: see the module docs.
/// What: stateless apart from its settings and link; every call runs
/// `vercel` afresh, and no value is kept.
/// Test: `vercel_push_runs_production_then_preview_with_the_value_on_stdin`.
#[derive(Debug)]
pub struct VercelBackend {
    settings: VercelSettings,
    link: VercelLink,
}

/// [`SecretsError::CliNotInstalled`] for `program`, with the install hint.
fn not_installed(program: &OsStr) -> SecretsError {
    SecretsError::CliNotInstalled {
        program: program.to_string_lossy().into_owned(),
        hint: SPEC.install_hint,
    }
}

impl VercelBackend {
    /// A backend pushing into `link`'s project as `settings` say.
    pub fn new(settings: VercelSettings, link: VercelLink) -> Self {
        Self { settings, link }
    }

    /// One `vercel` run of `args` in `work`, steering variables removed.
    ///
    /// What: a `program` that is not absolute is
    /// [`SecretsError::CliNotInstalled`], and nothing is spawned.
    fn command(
        &self,
        vault: &VaultName,
        key: &SecretKey,
        work: &WorkDir,
        args: &[&str],
    ) -> Result<CliCommand, SecretsError> {
        let s = &self.settings;
        if !Path::new(&s.program).is_absolute() {
            return Err(not_installed(&s.program));
        }
        let mut command = CliCommand::new(SPEC)
            .program(&s.program)
            .timeout(s.timeout)
            .target(vault, key)
            .args(args)
            // #9069: V4 — `--cwd` beats any `.vercel/project.json` around.
            .arg("--cwd")
            .arg(work.path());
        if let Some(dir) = &s.global_config {
            command = command.arg("--global-config").arg(dir);
        }
        // #9069: D7, V4 — the CLI's own login; the team and project come
        // from the account's mapping, never the spawner.
        let inherited = std::env::vars_os().map(|(name, _)| name);
        for name in settings::steering_vars(inherited) {
            command = command.env_remove(name);
        }
        Ok(command
            .env("PATH", settings::path_overlay(&s.program))
            .env(settings::ORG_ENV, &self.link.org_id)
            .env(settings::PROJECT_ENV, &self.link.project_id))
    }

    fn failed(&self, vault: &VaultName, key: &SecretKey, reason: &'static str) -> SecretsError {
        SecretsError::Backend {
            backend: BackendId::VERCEL.to_string(),
            vault: vault.to_string(),
            key: key.to_string(),
            reason: reason.to_string(),
        }
    }

    /// Whether `target`'s listing names `name`.
    ///
    /// What: a signed-out CLI is [`SecretsError::BackendLocked`]; any other
    /// failure, and exit 0 with text that is not a listing, is an error.
    fn lists(
        &self,
        vault: &VaultName,
        key: &SecretKey,
        work: &WorkDir,
        name: &str,
        target: &str,
    ) -> Result<bool, SecretsError> {
        let run = self
            .command(vault, key, work, &["env", "ls", target])?
            .run()?;
        if run.verdict != Verdict::Ok {
            return Err(match run.into_value() {
                Err(e) => e,
                Ok(_) => self.failed(vault, key, UNREADABLE),
            });
        }
        // #9069: V6 — output the parser does not know is a failure.
        listing::names(run.stdout.expose())
            .map(|names| names.contains(name))
            .ok_or_else(|| self.failed(vault, key, UNREADABLE))
    }

    /// `vercel env add` on `target`, the value on stdin and nowhere else.
    fn push(
        &self,
        vault: &VaultName,
        key: &SecretKey,
        work: &WorkDir,
        target: &str,
        value: &SecretValue,
    ) -> Result<(), SecretsError> {
        let name = key.as_str();
        let args = ["env", "add", name, target, ADD_FLAGS[0], ADD_FLAGS[1]];
        // #9069: DOC-74 §8.2 — the runner refuses the value in argv and the
        // environment before spawning.
        let run = self
            .command(vault, key, work, &args)?
            .run_with_stdin(value)
            .map_err(|e| self.after_push(e, vault, key))?;
        match run.verdict {
            Verdict::Ok => Ok(()),
            _ => Err(self.failed(vault, key, PUSH_FAILED)),
        }
    }

    /// The error a failed push or confirming read reports: a deadline and
    /// a missing program keep their kind; anything else says a target may
    /// have been written.
    fn after_push(&self, e: SecretsError, vault: &VaultName, key: &SecretKey) -> SecretsError {
        match e {
            SecretsError::DeadlineExceeded { .. } | SecretsError::CliNotInstalled { .. } => e,
            _ => self.failed(vault, key, PUSH_FAILED),
        }
    }
}

impl SecretBackend for VercelBackend {
    fn id(&self) -> BackendId {
        BackendId::vercel()
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
            backend: BackendId::VERCEL.to_string(),
            operation: "get",
        })
    }

    /// Push `value` as the variable `key` on every target; see the module
    /// docs.
    ///
    /// Test: `vercel_push_runs_production_then_preview_with_the_value_on_stdin`,
    /// `vercel_one_target_failing_fails_the_key`,
    /// `vercel_missing_from_the_listing_after_exit_zero_is_a_failure`,
    /// `vercel_existing_variable_is_updated_in_place_and_fails_unproven`.
    fn set(
        &self,
        vault: &VaultName,
        key: &SecretKey,
        value: &SecretValue,
    ) -> Result<(), SecretsError> {
        let name = key.as_str();
        if !listing::is_variable_name(name) {
            return Err(self.failed(vault, key, BAD_NAME));
        }
        let work = WorkDir::create(&self.settings.work_root)?;
        // #9069: the baseline also proves the login and the link.
        let mut existed = false;
        for target in TARGETS {
            existed |= self.lists(vault, key, &work, name, target)?;
        }
        // #9069: V3 — the first failing target fails the key; no later one runs.
        for target in TARGETS {
            self.push(vault, key, &work, target, value)?;
        }
        for target in TARGETS {
            // #9069: D8 — fail closed; a deadline keeps its own kind and text.
            let shown = self
                .lists(vault, key, &work, name, target)
                .map_err(|e| match e {
                    SecretsError::DeadlineExceeded { .. } => e,
                    _ => self.failed(vault, key, UNCONFIRMED),
                })?;
            if !shown {
                return Err(self.failed(vault, key, NOT_SHOWN));
            }
        }
        // #9069: D9 (a) — presence cannot prove an overwrite took.
        if existed {
            return Err(self.failed(vault, key, UPDATE_UNPROVEN));
        }
        Ok(())
    }

    fn delete(&self, _vault: &VaultName, _key: &SecretKey) -> Result<bool, SecretsError> {
        // #9069: D12 — a delete never removes a pushed copy.
        Err(SecretsError::Unsupported {
            backend: BackendId::VERCEL.to_string(),
            operation: "delete",
        })
    }
}

/// The machine config's `secrets.vercel` section, or
/// [`SecretsError::BackendNotEnabled`] without one.
fn section_at(machine_config: &Path) -> Result<MachineSecretsConfig, SecretsError> {
    let id = BackendId::vercel();
    let machine: Option<MachineSecretsConfig> = config::load_machine_at(machine_config)?;
    machine
        .filter(|m| m.enables(&id))
        .ok_or_else(|| SecretsError::BackendNotEnabled {
            backend: id.to_string(),
        })
}

/// Open the sync target for `identity`'s checkout when the machine config
/// enables it and maps the checkout to a project.
///
/// Why: ruling 74 — only the account's own machine config enables a push
/// and names the program; D6 — the project comes only from its mapping.
/// What: no `secrets.vercel:` section, or no `projects` entry for the
/// checkout, is [`SecretsError::BackendNotEnabled`]; then
/// [`VercelSettings::from_machine`]'s checks. Each push's working
/// directory is made under `work_root`. Spawns nothing.
/// Test: `vercel_without_a_mapping_is_refused_before_any_spawn`.
pub fn open_in(
    machine_config: &Path,
    identity: &RemoteIdentity,
    work_root: &Path,
) -> Result<Arc<dyn SecretBackend>, SecretsError> {
    let machine = section_at(machine_config)?;
    let (settings, link) =
        VercelSettings::from_machine(&machine, machine_config, identity, work_root)?;
    Ok(Arc::new(VercelBackend::new(settings, link)))
}

/// [`open_in`]'s checks without a destination, for doctor.
///
/// What: the section's checks, then at least one `projects` entry, else
/// [`SecretsError::BackendNotEnabled`]. Spawns nothing.
#[cfg(feature = "server")]
pub(crate) fn ready(machine_config: &Path) -> Result<Capabilities, SecretsError> {
    let section = section_at(machine_config)?.vercel.unwrap_or_default();
    let mapped = !section.projects.is_empty();
    settings::checked(section, machine_config, Path::new(""))?;
    if !mapped {
        return Err(SecretsError::BackendNotEnabled {
            backend: BackendId::VERCEL.to_string(),
        });
    }
    Ok(Capabilities::WRITE | Capabilities::SYNC_TARGET)
}
