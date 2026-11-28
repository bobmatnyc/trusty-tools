//! tm's hooks on the carrier body (#12077 part 4c, Architect 14:38Z Q1/Q2).
//!
//! Why: the carrier is the one `--settings` body every launch path writes, so
//! building tm's hooks into it on every launch reaches every path, keeps a
//! fresh exe path, and keeps the hooks out of the stored launch-settings
//! record, which the divert worker reads under `TRUSTY_MPM_DISABLE_HOOKS`.
//! What: [`LaunchHooks`] names the framework layout and the resolved hook
//! binary; [`resolve_launch_hooks`] resolves both for a production launch and
//! refuses when no stable binary resolves (Q2); [`hooks_for_launch`] strips
//! tm's old project copies and returns the `hooks` object for the body.
//! Test: `carrier_hooks_tests.rs`.

use std::path::{Path, PathBuf};

use serde_json::Value;

use super::carrier::{CarrierInputs, ProvisionError};
use crate::core::paths::FrameworkPaths;

/// What a launch needs to build its hooks.
#[derive(Debug, Clone, Copy)]
pub struct LaunchHooks<'a> {
    /// The framework layout whose config decides the toggles.
    pub fw: &'a FrameworkPaths,
    /// The stable hook binary every tm command names.
    pub exe: &'a Path,
}

/// The framework layout and hook binary for a production launch in `cwd`.
///
/// What: [`FrameworkPaths::for_managed_workspace`]`(cwd)` and
/// [`crate::core::standalone::hooks::resolve_stable_hook_exe`]`(exe_override)`.
///
/// # Errors
///
/// [`ProvisionError::HookExe`] when no stable binary resolves: every profile's
/// launch is refused, so a tm session never runs unguarded (Q2).
///
/// Test: `an_unresolvable_hook_binary_refuses_a_pm_launch_12077`.
pub fn resolve_launch_hooks(
    cwd: &Path,
    exe_override: Option<&Path>,
) -> Result<(FrameworkPaths, PathBuf), ProvisionError> {
    let exe = crate::core::standalone::hooks::resolve_stable_hook_exe(exe_override)?;
    Ok((FrameworkPaths::for_managed_workspace(cwd), exe))
}

/// The `hooks` object this launch's body carries; `None` when the inputs name
/// no [`LaunchHooks`].
///
/// What: an Architect first passes the Claude Code version floor (#10459);
/// then tm's old entries leave `<cwd>/.claude/settings{,.local}.json`
/// ([`crate::core::session_launch::launch_hooks::strip_tm_project_hooks`]);
/// then the groups are built for `inputs.profile`.
///
/// # Errors
///
/// [`ProvisionError::ArchitectVersion`], [`ProvisionError::Strip`] or
/// [`ProvisionError::HookExe`]; each refuses the launch.
///
/// Test: `the_carrier_body_carries_the_hooks_12077`,
/// `an_unwritable_project_file_refuses_the_carrier_12077`,
/// `the_carrier_refuses_an_architect_below_the_claude_floor_12077`.
pub(super) fn hooks_for_launch(
    inputs: &CarrierInputs<'_>,
) -> Result<Option<Value>, ProvisionError> {
    let Some(hooks) = inputs.hooks else {
        return Ok(None);
    };
    // #12077 part 4c: the one floor check every launch path reaches, `tm run`
    // and the control-plane worker included.
    crate::core::architect_claude_version::require_for_profile(inputs.profile)?;
    use crate::core::session_launch::launch_hooks;
    let removed = launch_hooks::strip_tm_project_hooks(inputs.cwd)?;
    for path in &removed {
        tracing::info!(path = %path.display(), "#12077: removed tm's old project hooks");
    }
    let additions =
        launch_hooks::launch_hook_additions(hooks.fw, inputs.cwd, inputs.profile, hooks.exe)?;
    Ok(additions.get("hooks").cloned())
}

#[cfg(test)]
#[path = "carrier_hooks_tests.rs"]
mod tests;
