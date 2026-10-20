//! The read-back half of the #8928 step 5 sandbox settings generator.
//!
//! Why: `.claude/settings.local.json` outranks the project file and can turn
//! `sandbox.enabled` or `allowUnsandboxedCommands` off (spike row C1), and a
//! merge can lose to a concurrent writer. The plan's step 5 therefore reads
//! the merged files back and refuses the launch unless they hold the sandbox
//! the role asked for.
//! What: [`verify_sandbox_settings`]; split from `sandbox_settings.rs` for the
//! SLOC cap.
//! Test: `sandbox_settings_tests.rs`.

use std::path::Path;

use serde_json::Value;

use super::sandbox_settings::{SandboxRole, SandboxSettingsError};

/// Read back the project, local and user tiers and refuse a wrong sandbox.
///
/// What: for a PM or agent, refuses unless the project file's
/// `sandbox.enabled` is `true` and `sandbox.allowUnsandboxedCommands` is
/// `false`, the user file's `sandbox.network.strictAllowlist` is `true`, and
/// `settings.local.json` sets no `sandbox` key. For the Architect, refuses
/// unless the project file's `sandbox.enabled` is `false`. FAIL-CLOSED: a
/// missing project or user file, and any of the three that cannot be read or
/// is not a JSON object, is an error; only a missing local file is fine.
///
/// # Errors
///
/// [`SandboxSettingsError::Unreadable`] or [`SandboxSettingsError::Refused`].
///
/// Test: `a_local_override_refuses_the_launch`,
/// `a_weakened_project_file_refuses_the_launch`,
/// `unreadable_settings_refuse_the_launch`.
pub fn verify_sandbox_settings(
    role: SandboxRole,
    workspace: &Path,
    user_config_dir: &Path,
) -> Result<(), SandboxSettingsError> {
    let claude = workspace.join(".claude");
    let project = read(&claude.join("settings.json"), true)?.unwrap_or(Value::Null);
    let enabled = project.pointer("/sandbox/enabled");
    if role == SandboxRole::Architect {
        return (enabled == Some(&Value::Bool(false)))
            .then_some(())
            .ok_or_else(|| {
                refused("the Architect's project settings do not set `sandbox.enabled: false`")
            });
    }
    if let Some(local) = read(&claude.join("settings.local.json"), false)?
        && let Some(sandbox) = local.get("sandbox")
    {
        let keys = sandbox
            .as_object()
            .map(|o| o.keys().cloned().collect::<Vec<_>>().join(", "))
            .unwrap_or_default();
        return Err(refused(&format!(
            "`.claude/settings.local.json` overrides the sandbox ({keys}); remove its `sandbox` key"
        )));
    }
    if enabled != Some(&Value::Bool(true)) {
        return Err(refused(
            "the project settings do not set `sandbox.enabled: true`",
        ));
    }
    if project.pointer("/sandbox/allowUnsandboxedCommands") != Some(&Value::Bool(false)) {
        return Err(refused(
            "the project settings do not set `sandbox.allowUnsandboxedCommands: false`",
        ));
    }
    let user = read(&user_config_dir.join("settings.json"), true)?.unwrap_or(Value::Null);
    if user.pointer("/sandbox/network/strictAllowlist") != Some(&Value::Bool(true)) {
        return Err(refused(
            "tm's user-level settings do not set `sandbox.network.strictAllowlist: true`",
        ));
    }
    Ok(())
}

/// `path` as a JSON object; `Ok(None)` when absent and not `required`.
fn read(path: &Path, required: bool) -> Result<Option<Value>, SandboxSettingsError> {
    let unreadable = |why: String| SandboxSettingsError::Unreadable {
        path: path.to_path_buf(),
        why,
    };
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && !required => return Ok(None),
        Err(e) => return Err(unreadable(e.to_string())),
    };
    match serde_json::from_str::<Value>(&text) {
        Ok(value) if value.is_object() => Ok(Some(value)),
        Ok(_) => Err(unreadable("not a JSON object".to_owned())),
        Err(e) => Err(unreadable(e.to_string())),
    }
}

/// A refusal naming `why`.
fn refused(why: &str) -> SandboxSettingsError {
    SandboxSettingsError::Refused(format!("#8928 sandbox settings: {why}"))
}
