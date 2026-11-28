//! The managed launch's sandbox step and the in-place relaunch's `--settings`
//! tail (#10102, #12077), split from `claude_code.rs` for the SLOC cap.
//!
//! Why: every managed spawn, resume and in-place relaunch goes through one
//! sandbox step; #12077 makes the sandbox opt-in and carries it inline.
//! What: [`managed_sandbox`], [`managed_sandbox_at`] and [`inplace_tail_args`].
//! Test: `claude_code_tests.rs`.

use std::path::Path;

use super::RuntimeError;

/// The sandbox step of a managed launch in `cwd` (#10102).
///
/// Why: #8928 step 6 — a managed spawn, resume or in-place relaunch goes
/// through the same sandbox step as every other launch path.
/// What: [`crate::core::session_launch::apply_launch_sandbox`] for
/// `profile`'s role, with `config_dir` as the user tier and the home from
/// [`crate::core::launch_home::sandbox_launch_home`]. #12077: enforcement is
/// off, so it writes nothing and refuses nothing; with no home resolved the
/// step is skipped ([`managed_sandbox_at`]). When `opted_in`, it returns the
/// role's inline sandbox payload
/// ([`crate::core::claude_sandbox::opted_in_launch_sandbox`]), still writing
/// no file; otherwise `None`.
/// Test: `spawn_writes_no_sandbox_settings_12077`,
/// `spawn_and_resume_go_ahead_over_a_local_override_12077`,
/// `a_split_config_base_launch_writes_no_sandbox_12077`,
/// `an_opted_in_spawn_and_resume_carry_the_sandbox_inline_12077`.
pub(super) fn managed_sandbox(
    cwd: &Path,
    config_dir: &Path,
    profile: crate::core::session_profile::SessionProfile,
    opted_in: bool,
) -> Result<Option<serde_json::Value>, RuntimeError> {
    // #10102: the real (sandbox-aware) home, never `fw.root`'s parent — that is
    // the config base, which can sit off `$HOME`.
    let home = crate::core::launch_home::sandbox_launch_home().map_err(RuntimeError::Spawn)?;
    managed_sandbox_at(cwd, home.as_deref(), config_dir, profile, opted_in)
}

/// [`managed_sandbox`] with the home passed in (#12077).
///
/// What: with no home, a default launch skips the step and an opted-in one
/// is refused ([`crate::core::claude_sandbox::NoHomeForSandbox`]); otherwise
/// the no-write sandbox step, then the inline payload when `opted_in`.
/// Test: `managed_sandbox_goes_ahead_with_no_home_12077`,
/// `a_managed_launch_refuses_an_opt_in_with_no_home_12077`,
/// `an_opted_in_managed_launch_keeps_the_role_record_without_the_env_12077`,
/// `a_default_managed_launch_deletes_the_role_record_despite_the_env_12077`.
pub(super) fn managed_sandbox_at(
    cwd: &Path,
    home: Option<&Path>,
    config_dir: &Path,
    profile: crate::core::session_profile::SessionProfile,
    opted_in: bool,
) -> Result<Option<serde_json::Value>, RuntimeError> {
    use crate::core::session_launch as sl;
    let role = sl::sandbox_role_for(profile);
    // #12077: Fail-Open Check — an opt-in with no home refuses the launch.
    let payload = crate::core::claude_sandbox::opted_in_launch_sandbox(opted_in, role, cwd, home)
        .map_err(|err| RuntimeError::Spawn(err.to_string()))?;
    if let Some(home) = home {
        let inputs = sl::launch_sandbox_inputs(cwd, home);
        // #12077: the launch's own opt-in (request or record), not the daemon's env.
        sl::apply_launch_sandbox(role, &inputs, Some(config_dir), opted_in)
            .map_err(|err| RuntimeError::Spawn(err.to_string()))?;
    }
    Ok(payload)
}

/// The in-place relaunch argv after `compose_inplace_args`: the owner
/// `--settings` (with the #12077 opted-in `sandbox` merged in) and the #8992
/// tool trim for `profile`.
///
/// What: #10127 — an Architect's deny joins the owner file as ONE
/// `--settings` ([`crate::core::architect_lockdown::Lockdown::settings_argv`]);
/// its error refuses the relaunch. The exec seam
/// (`build_inplace_exec_command`) drops the chat tokens and sets the trim env.
/// Test: `an_architect_inplace_relaunch_carries_the_lockdown`.
pub(super) fn inplace_tail_args(
    profile: crate::core::session_profile::SessionProfile,
    carrier: Option<&Path>,
    sandbox: Option<&serde_json::Value>,
) -> Result<Vec<String>, RuntimeError> {
    let mut args = crate::core::architect_lockdown::Lockdown::PRODUCTION
        .with_sandbox(sandbox)
        .settings_argv(profile, carrier)
        .map_err(super::lockdown_refusal)?;
    args.extend(crate::core::pm_tool_trim::trim_args(profile));
    Ok(args)
}
