//! The per-role record behind one workspace's project sandbox tier (#10102).
//!
//! Why: a PM, an agent and the Architect can each launch into one workspace,
//! and all three write the same `.claude/settings.json`. `merge_policy`
//! replaces allow lists, so an agent launch dropped the PM's tmux socket from
//! `allowUnixSockets`; and the Architect's `sandbox.enabled: false` and a
//! sandboxed role's `true` overwrote each other. The allow lists cannot simply
//! be unioned with the file, because the file may hold operator entries
//! (an operator-added sccache socket) that tm must keep replacing.
//! What: [`write_project_tier`] keeps `<workspace>/.trusty-mpm/sandbox-roles.json`,
//! a map from role to the [`project_policy`] that role last wrote. Under that
//! record's lock it replaces the launching role's entry, unions every
//! sandboxed role's policy into one (all lists unioned, scalars equal across
//! sandboxed roles) and merges that into the project file with
//! [`merge_policy`], so tm's lists stay tm's — every recorded role's
//! entries, never the operator's. The Architect and a sandboxed role cannot
//! share a workspace: `sandbox.enabled` has one value per file, so the second
//! role's launch is refused and neither entry is overwritten.
//! Test: `two_sandboxed_roles_in_one_workspace_keep_each_others_entries`,
//! `the_architect_and_a_sandboxed_role_never_overwrite_each_other`.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use super::{SandboxInputs, SandboxRole, SandboxSettingsError, merge_policy, project_policy};

/// The record's file name under `<workspace>/.trusty-mpm/`.
// #12077: one name, shared with the read-only detection.
const RECORD_FILE: &str = crate::core::claude_sandbox::ROLE_RECORD_FILE;

/// The record path for `workspace`.
pub(super) fn record_path(workspace: &Path) -> PathBuf {
    workspace.join(".trusty-mpm").join(RECORD_FILE)
}

/// The record key for `role`.
fn role_key(role: SandboxRole) -> &'static str {
    match role {
        SandboxRole::Pm => "pm",
        SandboxRole::Agent => "agent",
        SandboxRole::Architect => "architect",
    }
}

/// Record `role`'s policy and merge the workspace's combined policy into the
/// project settings file.
///
/// Why/What: see the module doc.
///
/// # Errors
///
/// [`SandboxSettingsError::Record`] when the record cannot be locked, read or
/// written; [`SandboxSettingsError::Refused`] when the Architect and a
/// sandboxed role would share the workspace;
/// [`SandboxSettingsError::Project`] when the settings merge fails.
///
/// Test: `two_sandboxed_roles_in_one_workspace_keep_each_others_entries`,
/// `the_architect_and_a_sandboxed_role_never_overwrite_each_other`.
pub(super) fn write_project_tier(
    role: SandboxRole,
    inputs: &SandboxInputs,
) -> Result<(), SandboxSettingsError> {
    let path = record_path(&inputs.workspace);
    let io = |source| SandboxSettingsError::Record {
        path: path.clone(),
        source,
    };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(io)?;
    }
    crate::core::settings_lock::with_settings_lock(&path, || {
        let mut record = load_record(&path)?;
        if let Some(other) = conflicting_role(&record, role) {
            return Err(conflict(role, other, &inputs.workspace));
        }
        record[role_key(role)] = project_policy(role, inputs);
        let combined = combined_policy(&record, role);
        super::super::settings::merge_settings(&inputs.workspace, |s| merge_policy(s, &combined))?;
        crate::core::settings_lock::publish(&path, &record).map_err(io)
    })
    .map_err(io)?
}

/// Read the role record; a missing file is an empty record.
///
/// Why: #10102 fix round — resetting a corrupt record to `{}` would forget a
/// recorded PM and let an Architect launch flip its `sandbox.enabled`.
/// What: `{}` when the file does not exist; FAIL-CLOSED otherwise — an
/// unreadable, unparseable or non-object file is a
/// [`SandboxSettingsError::Record`] naming the file.
/// Test: `a_corrupt_role_record_refuses_the_launch`.
fn load_record(path: &Path) -> Result<Value, SandboxSettingsError> {
    let fail = |why: String| SandboxSettingsError::Record {
        path: path.to_path_buf(),
        source: std::io::Error::other(format!(
            "{why}; fix or remove `{}` (it records which roles share this workspace)",
            path.display()
        )),
    };
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(json!({})),
        Err(e) => return Err(fail(format!("cannot be read: {e}"))),
    };
    match serde_json::from_str::<Value>(&text) {
        Ok(value) if value.is_object() => Ok(value),
        Ok(_) => Err(fail("is not a JSON object".to_owned())),
        Err(e) => Err(fail(format!("is not JSON: {e}"))),
    }
}

/// The recorded role whose `sandbox.enabled` differs from `role`'s, if any.
fn conflicting_role(record: &Value, role: SandboxRole) -> Option<&'static str> {
    let sandboxed = role != SandboxRole::Architect;
    let others: &[SandboxRole] = if sandboxed {
        &[SandboxRole::Architect]
    } else {
        &[SandboxRole::Pm, SandboxRole::Agent]
    };
    others
        .iter()
        .map(|r| role_key(*r))
        .find(|key| record.get(*key).is_some())
}

/// The refusal for a launch that would flip another role's `sandbox.enabled`.
fn conflict(role: SandboxRole, other: &str, workspace: &Path) -> SandboxSettingsError {
    SandboxSettingsError::Refused(format!(
        "#8928 sandbox settings: `{}` already holds the `{other}` role's sandbox policy, and \
         the `{}` role needs the opposite `sandbox.enabled`; one project settings file cannot \
         hold both. Launch the Architect from its own directory (`tm fleet init`), or remove \
         `{}` if the `{other}` role no longer runs there",
        workspace.display(),
        role_key(role),
        record_path(workspace).display()
    ))
}

/// The policy the project file gets: `role`'s own for the Architect, else
/// every recorded sandboxed role's policy with all lists unioned.
fn combined_policy(record: &Value, role: SandboxRole) -> Value {
    if role == SandboxRole::Architect {
        return record
            .get(role_key(role))
            .cloned()
            .unwrap_or_else(|| json!({}));
    }
    let mut combined = Value::Object(Map::new());
    for key in [SandboxRole::Pm, SandboxRole::Agent].map(role_key) {
        if let Some(policy) = record.get(key) {
            // #12077: the same union the inline sandbox payload uses.
            crate::core::claude_sandbox::merge_settings_payload(&mut combined, policy);
        }
    }
    combined
}
