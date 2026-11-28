//! The opt-in Claude Code sandbox for tm launches (#12077 slice 2).
//!
//! Why (#12077): the sandbox is off by default and opt-in
//! through one switch, and tm never writes a sandbox key into a generic Claude
//! settings file (`<ws>/.claude/settings.json`, `<ws>/.claude/settings.local.json`,
//! `~/.claude/settings.json`) or into tm's user-tier `claude-config/settings.json`.
//! What: [`SANDBOX_OPT_IN_ENV`] is the switch and [`sandbox_opted_in`] reads it.
//! [`opted_in_launch_sandbox`] renders a role's whole policy (project and user
//! tier in one object) for a launch that opted in; the launch carries it as an
//! inline `--settings '<json>'` through
//! [`crate::core::architect_lockdown::Lockdown::with_sandbox`], so no Claude
//! settings file is written. [`detect_tm_sandbox_keys`] reports, read-only,
//! which sandbox keys tm 1.14.14 and 1.14.15 wrote into those files, and
//! [`clean_tm_sandbox_keys_once`] removes them from the project file, once;
//! [`clean_tm_user_sandbox_keys_once`] from tm's user-tier file, once.
//! Test: `claude_sandbox_tests.rs`.

use std::path::Path;

use serde_json::{Map, Value};

use crate::core::session_launch::{
    SandboxInputs, SandboxRole, launch_sandbox_inputs, project_policy, user_policy,
};

#[path = "claude_sandbox_detect.rs"]
mod detect;
pub use detect::{TmSandboxKey, detect_tm_sandbox_keys};

// #12077 slice 3: the one allowed write to the project's generic settings file.
#[path = "claude_sandbox_cleanup.rs"]
mod cleanup;
pub use cleanup::{
    CLEANUP_MARKER_FILE, CleanupOutcome, SandboxCleanupError, clean_tm_sandbox_keys_on_launch,
    clean_tm_sandbox_keys_once,
};

// #12077 slice 3b: the same one-time pass over tm's user-tier settings file.
#[path = "claude_sandbox_cleanup_user.rs"]
mod cleanup_user;
pub use cleanup_user::{
    USER_CLEANUP_MARKER_FILE, UserCleanupOutcome, clean_tm_user_sandbox_keys_once,
};

/// The one opt-in switch: `TM_CLAUDE_SANDBOX=1` turns the sandbox on.
///
/// Why: #12077 — an env var reaches every launch path (`tm launch`, `tm run`,
/// `tm session start`, the divert worker) without a config read, and a
/// daemon-spawned session carries the client's value in its spawn request.
/// What: exactly `1` is on; unset or any other value is off.
/// Test: `only_exactly_one_opts_in`.
pub const SANDBOX_OPT_IN_ENV: &str = "TM_CLAUDE_SANDBOX";

/// The per-workspace role record 1.14.14 wrote under `<ws>/.trusty-mpm/`.
pub const ROLE_RECORD_FILE: &str = "sandbox-roles.json";

/// Whether `value` (the raw [`SANDBOX_OPT_IN_ENV`] value) opts in.
///
/// Test: `only_exactly_one_opts_in`.
pub fn opt_in_from(value: Option<&std::ffi::OsStr>) -> bool {
    value == Some(std::ffi::OsStr::new("1"))
}

/// Whether this process opted in: [`opt_in_from`] over [`SANDBOX_OPT_IN_ENV`].
///
/// Test: `the_process_switch_reads_the_env_var`.
pub fn sandbox_opted_in() -> bool {
    opt_in_from(std::env::var_os(SANDBOX_OPT_IN_ENV).as_deref())
}

/// The inline `--settings` payload for `role`: its [`project_policy`] with
/// its [`user_policy`] merged in.
///
/// Why: #12077 — the two tiers 1.14.14 wrote into two files travel as one
/// argv payload, because Claude Code keeps only the last `--settings`.
/// What: [`merge_settings_payload`] of the user tier into the project tier.
/// Test: `an_opted_in_pm_payload_carries_both_tiers`.
pub fn inline_sandbox_settings(role: SandboxRole, inputs: &SandboxInputs) -> Value {
    let mut payload = project_policy(role, inputs);
    if let Some(user) = user_policy(role) {
        merge_settings_payload(&mut payload, &user);
    }
    payload
}

/// An opt-in that cannot be honoured: the switch is on but no home resolved
/// (#12077 Fail-Open Check).
///
/// Why: the policy names the home's `.ssh`, `.aws` and tool caches; without a
/// home there is no policy, and launching unsandboxed after an explicit
/// opt-in would hide exactly the error the operator asked to watch for.
/// Test: `an_opt_in_with_no_home_refuses_naming_the_switch_12077`.
#[derive(Debug, thiserror::Error)]
#[error(
    "TM_CLAUDE_SANDBOX=1 asks for the Claude Code sandbox in `{}`, but no home directory \
     resolved to build its policy from; refusing the launch rather than starting it \
     unsandboxed (#12077). Set HOME, or unset TM_CLAUDE_SANDBOX to launch without the sandbox",
    .workspace.display()
)]
pub struct NoHomeForSandbox {
    /// The workspace the refused launch was for.
    pub workspace: std::path::PathBuf,
}

/// The sandbox payload a launch of `role` in `workspace` carries, if any.
///
/// Why: #12077 — the default launch carries no sandbox at all, and an
/// explicit opt-in is honoured or refused, never dropped.
/// What: `Ok(None)` unless `opted_in`; opted in, [`inline_sandbox_settings`]
/// over [`launch_sandbox_inputs`], or [`NoHomeForSandbox`] when `home` is
/// `None`. Reads and writes no file.
///
/// # Errors
///
/// [`NoHomeForSandbox`] when `opted_in` and no home resolved.
///
/// Test: `a_default_launch_gets_no_sandbox_payload`,
/// `an_opted_in_pm_payload_carries_both_tiers`,
/// `an_opt_in_with_no_home_refuses_naming_the_switch_12077`.
pub fn opted_in_launch_sandbox(
    opted_in: bool,
    role: SandboxRole,
    workspace: &Path,
    home: Option<&Path>,
) -> Result<Option<Value>, NoHomeForSandbox> {
    if !opted_in {
        return Ok(None);
    }
    // #12077: Fail-Open Check — an opt-in with no home refuses, never drops.
    let home = home.ok_or_else(|| NoHomeForSandbox {
        workspace: workspace.to_path_buf(),
    })?;
    Ok(Some(inline_sandbox_settings(
        role,
        &launch_sandbox_inputs(workspace, home),
    )))
}

/// Merge `payload` into `slot`: objects recurse, lists gain each entry they
/// lack, every other value replaces.
///
/// Why: the sandbox payload joins the owner carrier's `permissions.deny` and
/// the role record unions each role's lists (#10102); one rule serves both.
/// Test: `merging_unions_lists_and_keeps_other_keys`.
pub fn merge_settings_payload(slot: &mut Value, payload: &Value) {
    match payload {
        Value::Object(map) => {
            if !slot.is_object() {
                *slot = Value::Object(Map::new());
            }
            for (key, value) in map {
                merge_settings_payload(&mut slot[key.as_str()], value);
            }
        }
        Value::Array(wanted) => {
            if !slot.is_array() {
                *slot = Value::Array(Vec::new());
            }
            if let Some(list) = slot.as_array_mut() {
                for entry in wanted {
                    if !list.contains(entry) {
                        list.push(entry.clone());
                    }
                }
            }
        }
        other => *slot = other.clone(),
    }
}

#[cfg(test)]
#[path = "claude_sandbox_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "claude_sandbox_cleanup_tests.rs"]
mod cleanup_tests;

#[cfg(test)]
#[path = "claude_sandbox_cleanup_user_tests.rs"]
mod cleanup_user_tests;
