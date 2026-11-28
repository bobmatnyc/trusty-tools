//! The control-plane worker's `--settings` body (#12077 part 4c, Q3).
//!
//! Why: a control-plane worker runs `claude` in its workdir with its default
//! tools (#9704). Its hooks used to be written into `<workdir>/.claude/` and
//! read back; they now ride one `--settings` body with the 4a/4b launch keys,
//! the same carrier every other launch path uses.
//! What: [`worker_settings`] provisions the carrier for the workdir's profile
//! and requires the egress guard in the body it built; any failure refuses
//! the spawn.
//! Test: `worker_settings_tests.rs`; end to end in `registry.rs`'s
//! `run_session_*_12077` tests.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::control::config::ControlPlaneConfig;
use crate::core::standalone::hooks::egress::{EgressGuardState, egress_guard_state};

/// Build the worker's carrier body for `workdir` and return its path.
///
/// What: the state root from `config.state_root`, else
/// [`crate::core::launch_policy::state_root`]; the profile resolved for
/// `workdir` under its framework config; then
/// [`crate::core::launch_policy::provision_settings_with_exe`] with
/// `config.hook_exe`, and [`require_egress_guard`] on the file.
///
/// # Errors
///
/// No state root, a refused carrier (unresolvable hook binary, unwritable
/// body or project file), no body, or a body whose egress guard is not
/// present. The caller spawns nothing.
///
/// Test: `a_worker_body_carries_the_egress_guard_12077`,
/// `an_unwritable_worker_body_refuses_the_spawn_12077`.
pub(crate) fn worker_settings(workdir: &Path, config: &ControlPlaneConfig) -> Result<PathBuf> {
    let resolved = crate::core::launch_policy::carrier_hooks::resolve_launch_hooks(
        workdir,
        config.hook_exe.as_deref(),
    );
    worker_settings_resolved(workdir, config, resolved)
}

/// [`worker_settings`] with the hook resolution supplied (#12077 part 4c, Q2).
///
/// Test: `an_unresolvable_hook_binary_refuses_the_worker_12077`.
pub(crate) fn worker_settings_resolved(
    workdir: &Path,
    config: &ControlPlaneConfig,
    resolved: Result<
        (crate::core::paths::FrameworkPaths, PathBuf),
        crate::core::launch_policy::ProvisionError,
    >,
) -> Result<PathBuf> {
    let refusal = || {
        format!(
            "refusing to spawn: the worker's hooks are not in place for {}",
            workdir.display()
        )
    };
    let state_root = config
        .state_root
        .clone()
        .or_else(crate::core::launch_policy::state_root)
        .with_context(|| format!("{}: no tm state root resolves", refusal()))?;
    let fw = crate::core::paths::FrameworkPaths::for_managed_workspace(workdir);
    let mpm = crate::core::config::MpmConfig::load(&fw.root);
    let profile = crate::core::session_profile::resolve(workdir, &mpm);
    let settings = crate::core::launch_policy::carrier::provision_settings_resolved(
        &state_root,
        workdir,
        None,
        profile,
        resolved,
    )
    .with_context(refusal)?;
    let file = settings
        .file
        .with_context(|| format!("{}: the carrier wrote no body", refusal()))?;
    require_egress_guard(&file).with_context(refusal)?;
    Ok(file)
}

/// Require the egress guard in the body at `file` (#9704, replacing the
/// project-file read-back).
///
/// Test: `a_body_without_the_egress_guard_is_refused_12077`.
pub(crate) fn require_egress_guard(file: &Path) -> Result<()> {
    let text = std::fs::read_to_string(file)
        .with_context(|| format!("read the worker body {}", file.display()))?;
    let value: serde_json::Value = serde_json::from_str(&text)
        .with_context(|| format!("{} is not valid JSON", file.display()))?;
    match egress_guard_state(&value) {
        EgressGuardState::Present => Ok(()),
        state => anyhow::bail!("egress guard {state:?} in {}", file.display()),
    }
}

#[cfg(test)]
#[path = "worker_settings_tests.rs"]
mod tests;
