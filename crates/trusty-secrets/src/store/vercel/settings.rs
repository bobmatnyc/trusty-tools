//! [`VercelSettings`] and [`VercelLink`]: how the Vercel sync target runs
//! the CLI, and which project it pushes to (#9069 PR 2).
//!
//! Why: D6 — the project comes only from the account's own machine config,
//! keyed by the checkout's origin remote. D7 — the CLI uses its own stored
//! login; an inherited `VERCEL_*` variable would choose the token, the team
//! or the project of a push, so every one is removed from the child's
//! environment. D16 — the CLI is a Node script, so the child's `PATH` is the
//! pinned program's directory and the system directories, never the
//! spawner's.
//! What: [`VercelSettings::from_machine`] checks `secrets.vercel` and picks
//! the mapping; [`steering_vars`] lists the inherited names a run removes;
//! [`path_overlay`] is the child's `PATH`.
//! Test: `vercel_without_a_mapping_is_refused_before_any_spawn`,
//! `vercel_unpinned_program_is_cli_not_installed`,
//! `vercel_child_runs_in_an_empty_dir_with_mapping_ids_and_no_inherited_vercel_env`,
//! `vercel_path_overlay_is_the_pinned_program_dir_and_system_dirs`.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::api::{BackendId, SecretsError};
use crate::store::RemoteIdentity;
use crate::store::config::{MachineSecretsConfig, VercelSection};
use crate::store::program::is_executable_file;

/// How long one `vercel` call may take; the request deadline also bounds it.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);

/// The variable the mapping's team id reaches the CLI through.
pub(super) const ORG_ENV: &str = "VERCEL_ORG_ID";
/// The variable the mapping's project id reaches the CLI through.
pub(super) const PROJECT_ENV: &str = "VERCEL_PROJECT_ID";
/// What follows the pinned program's directory in the child's `PATH` (D16).
pub(super) const SYSTEM_PATH: &str = "/usr/bin:/bin";

/// Removed from every run whether inherited or not; the prefix rule in
/// [`steering_vars`] covers the rest.
const STEERING: [&str; 4] = [ORG_ENV, PROJECT_ENV, "VERCEL_TOKEN", "VERCEL_SCOPE"];

/// How the backend invokes the Vercel CLI.
///
/// Why: see the module docs.
/// What: `program` is an absolute path; `global_config`, when set, is
/// passed as `--global-config`; each push runs in a fresh empty directory
/// under `work_root`. Holds no secret, so `Debug` is derived.
/// Test: `vercel_unpinned_program_is_cli_not_installed`.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct VercelSettings {
    /// The program to run; absolute, or the backend spawns nothing.
    pub program: OsString,
    /// The CLI's global config directory, passed as `--global-config`.
    pub global_config: Option<PathBuf>,
    /// How long one `vercel` call may take.
    pub timeout: Duration,
    /// The 0700 root each push's empty working directory is made under.
    pub work_root: PathBuf,
}

/// The Vercel project a push lands in, from the machine config's mapping.
///
/// What: ids hold only `[A-Za-z0-9_-]`, 1 to 64 bytes, so they never
/// carry a separator into the child's environment.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct VercelLink {
    /// The team or user id (`VERCEL_ORG_ID`).
    pub org_id: String,
    /// The project id (`VERCEL_PROJECT_ID`).
    pub project_id: String,
}

impl VercelLink {
    /// The link to `project_id` under `org_id`, or `None` when either id is
    /// not `[A-Za-z0-9_-]{1,64}`.
    pub fn new(org_id: &str, project_id: &str) -> Option<Self> {
        let valid = |id: &str| {
            (1..=64).contains(&id.len())
                && id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        };
        (valid(org_id) && valid(project_id)).then(|| Self {
            org_id: org_id.to_string(),
            project_id: project_id.to_string(),
        })
    }
}

impl VercelSettings {
    /// `program`, no global config, the default timeout, and `work_root`.
    pub fn new(program: impl Into<OsString>, work_root: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            global_config: None,
            timeout: DEFAULT_TIMEOUT,
            work_root: work_root.into(),
        }
    }

    /// Settings and the project link for `identity`'s checkout.
    ///
    /// What: in order, each failing before anything is spawned:
    /// - no `projects` entry for `<owner>/<repo>` is
    ///   [`SecretsError::BackendNotEnabled`] (V1, D14);
    /// - the section's checks in [`checked`];
    /// - a mapped id outside `[A-Za-z0-9_-]{1,64}` is [`SecretsError::Config`].
    ///
    /// Errors name `machine_path` and the key, never a value.
    /// Test: `vercel_without_a_mapping_is_refused_before_any_spawn`,
    /// `vercel_unpinned_program_is_cli_not_installed`.
    pub fn from_machine(
        machine: &MachineSecretsConfig,
        machine_path: &Path,
        identity: &RemoteIdentity,
        work_root: &Path,
    ) -> Result<(Self, VercelLink), SecretsError> {
        let section = machine.vercel.clone().unwrap_or_default();
        // #9069: V1 — no mapping, no push; refused before the program check.
        let mapped = section
            .project(&identity.owner, &identity.repo)
            .cloned()
            .ok_or_else(|| SecretsError::BackendNotEnabled {
                backend: BackendId::VERCEL.to_string(),
            })?;
        let settings = checked(section, machine_path, work_root)?;
        let link = VercelLink::new(&mapped.org_id, &mapped.project_id).ok_or_else(|| {
            invalid(
                machine_path,
                "`secrets.vercel.projects` ids hold only [A-Za-z0-9_-], 1 to 64 characters",
            )
        })?;
        Ok((settings, link))
    }
}

fn invalid(machine_path: &Path, reason: &str) -> SecretsError {
    SecretsError::Config {
        path: machine_path.to_path_buf(),
        reason: reason.to_string(),
    }
}

/// The settings `section` gives, without a destination.
///
/// What: an `account` is [`SecretsError::Config`]; a `config_path` must be
/// an absolute existing directory ([`SecretsError::Config`]); the `program`
/// pin is required — absent is [`SecretsError::CliNotInstalled`], relative
/// or holding `:` is [`SecretsError::Config`], not an executable file is
/// [`SecretsError::CliNotInstalled`]. `PATH` is never searched.
/// Test: `vercel_unpinned_program_is_cli_not_installed`.
pub(super) fn checked(
    section: VercelSection,
    machine_path: &Path,
    work_root: &Path,
) -> Result<VercelSettings, SecretsError> {
    if section.account.is_some() {
        return Err(invalid(
            machine_path,
            "`secrets.vercel.account` is not used: the account is the one the Vercel CLI's login selects",
        ));
    }
    if let Some(dir) = &section.config_path
        && !(dir.is_absolute() && dir.is_dir())
    {
        return Err(invalid(
            machine_path,
            "`secrets.vercel.config_path` must be the absolute path of the Vercel CLI's global config directory",
        ));
    }
    // #9069: §3.3 — a pin only; the CLI usually sits outside the system dirs.
    let program = match section.program {
        None => return Err(super::not_installed(OsStr::new(super::SPEC.program))),
        Some(pin) if !pin.is_absolute() || pin.as_os_str().as_encoded_bytes().contains(&b':') => {
            return Err(invalid(
                machine_path,
                "`secrets.vercel.program` must be an absolute path with no `:`",
            ));
        }
        Some(pin) if is_executable_file(&pin) => pin,
        Some(pin) => return Err(super::not_installed(pin.as_os_str())),
    };
    Ok(VercelSettings {
        program: program.into_os_string(),
        global_config: section.config_path,
        timeout: DEFAULT_TIMEOUT,
        work_root: work_root.to_path_buf(),
    })
}

/// The inherited variable names a `vercel` run removes.
///
/// Why: D7, V4 — `VERCEL_TOKEN` replaces the stored login, and
/// `VERCEL_ORG_ID`, `VERCEL_PROJECT_ID` and `VERCEL_SCOPE` choose the team
/// or project a push lands in.
/// What: [`STEERING`] plus every name in `inherited` that starts with
/// `VERCEL_`. The two ids return as an overlay only from the mapping.
/// Test: `vercel_child_runs_in_an_empty_dir_with_mapping_ids_and_no_inherited_vercel_env`.
pub(super) fn steering_vars(inherited: impl IntoIterator<Item = OsString>) -> Vec<OsString> {
    let mut names: Vec<OsString> = STEERING.iter().map(OsString::from).collect();
    names.extend(
        inherited
            .into_iter()
            .filter(|name| name.as_encoded_bytes().starts_with(b"VERCEL_")),
    );
    names
}

/// The child's `PATH`: the pinned program's directory, then
/// [`SYSTEM_PATH`] (D16).
///
/// Why: a `#!/usr/bin/env node` shebang resolves `node` on `PATH`, so the
/// spawner's `PATH` would choose the interpreter that receives the value.
/// Test: `vercel_path_overlay_is_the_pinned_program_dir_and_system_dirs`.
pub(super) fn path_overlay(program: &OsStr) -> OsString {
    let mut path = Path::new(program)
        .parent()
        .map(|dir| dir.as_os_str().to_os_string())
        .unwrap_or_default();
    path.push(":");
    path.push(SYSTEM_PATH);
    path
}
