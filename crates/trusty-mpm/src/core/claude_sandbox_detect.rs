//! Read-only detection of the sandbox keys tm 1.14.14 and 1.14.15 wrote
//! (#12077).
//!
//! Why: the owner has not chosen how those keys are cleaned up, but every
//! cleanup mode needs to know which keys are tm's. A key the operator set must
//! never be reported as tm's.
//! What: [`detect_tm_sandbox_keys`] reads, and never writes or deletes:
//! - the project tier, `<ws>/.claude/settings.json` and
//!   `<ws>/.claude/settings.local.json`: a key or list entry is tm's when it
//!   equals the value the workspace's role record
//!   (`<ws>/.trusty-mpm/sandbox-roles.json`) holds for it. No record means tm
//!   owns nothing there.
//! - tm's user tier, `<user_tier_dir>/settings.json`: it has no record, so a
//!   key is tm's when it equals tm's exact value set ([`user_policy`]) —
//!   `sandbox.network.strictAllowlist: true`, `sandbox.network.allowedDomains`
//!   equal to the whole domain list, and each `{name, mode: "deny"}` entry of
//!   `sandbox.credentials.envVars`. An operator value that equals tm's is
//!   reported as tm's (the audit's stated risk).
//!
//! `~/.claude/settings.json` is not read: the 1.14.14 writer refused it as a
//! user tier, so it never received a sandbox key from tm.
//! Test: `claude_sandbox_tests.rs`.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::core::session_launch::{SandboxRole, SandboxSettingsError, user_policy};

/// One settings key, or one list entry, that tm wrote.
///
/// Test: `a_recorded_policy_marks_only_matching_project_keys`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TmSandboxKey {
    /// The settings file that holds it.
    pub file: PathBuf,
    /// The dotted key path; a list entry is `<path>[<entry>]`, the entry's
    /// `name` for an object entry, its text for a string.
    pub key: String,
}

/// Every sandbox key tm wrote into the project tier of `workspace` and into
/// tm's user tier under `user_tier_dir`.
///
/// Why/What: see the module doc. It writes nothing; the one-time cleanup
/// (`claude_sandbox_cleanup.rs`) shares its record read and matching rule.
///
/// # Errors
///
/// [`SandboxSettingsError::Unreadable`] when the role record or a settings
/// file exists but cannot be read or is not a JSON object. A missing file is
/// not an error.
///
/// Test: `no_role_record_means_tm_owns_nothing_in_the_project`,
/// `a_recorded_policy_marks_only_matching_project_keys`,
/// `the_user_tier_matches_only_tms_exact_values`,
/// `a_corrupt_role_record_is_an_error_not_a_guess`.
pub fn detect_tm_sandbox_keys(
    workspace: &Path,
    user_tier_dir: &Path,
) -> Result<Vec<TmSandboxKey>, SandboxSettingsError> {
    let mut found = Vec::new();
    if let Some(ours) = recorded_project_policy(workspace)? {
        for name in ["settings.json", "settings.local.json"] {
            let file = workspace.join(".claude").join(name);
            if let Some(settings) = read_object(&file)? {
                found.extend(project_keys_in(&file, &settings, &ours));
            }
        }
    }
    let file = user_tier_dir.join("settings.json");
    if let (Some(settings), Some(ours)) = (read_object(&file)?, user_policy(SandboxRole::Pm)) {
        found.extend(user_keys_in(&file, &settings, &ours));
    }
    Ok(found)
}

/// The union of every role policy in `workspace`'s role record; `None` when
/// there is no record.
///
/// Why: #10102 — tm merged every recorded role's policy into the project
/// file, so the union is what tm wrote there. #12077: the cleanup reads the
/// same union.
///
/// # Errors
///
/// [`SandboxSettingsError::Unreadable`] for a record that exists but cannot
/// be read or is not a JSON object.
pub(super) fn recorded_project_policy(
    workspace: &Path,
) -> Result<Option<Value>, SandboxSettingsError> {
    let record_path = workspace.join(".trusty-mpm").join(super::ROLE_RECORD_FILE);
    let Some(record) = read_object(&record_path)? else {
        return Ok(None);
    };
    let mut ours = Value::Object(serde_json::Map::new());
    if let Value::Object(roles) = &record {
        for policy in roles.values() {
            super::merge_settings_payload(&mut ours, policy);
        }
    }
    Ok(Some(ours))
}

/// Each project-tier key or list entry of `settings` (read from `file`) that
/// equals the recorded policy `ours`: the one matching rule (#12077).
pub(super) fn project_keys_in(file: &Path, settings: &Value, ours: &Value) -> Vec<TmSandboxKey> {
    let mut found = Vec::new();
    collect(file, settings, ours, "", &[], &mut found);
    found
}

/// The user-tier lists that are tm's only as a whole: 1.14.15 replaced
/// `allowedDomains` outright, so a list that differs at all is the user's.
pub(super) const USER_WHOLE_LISTS: &[&str] = &["allowedDomains"];

/// Each user-tier key or list entry of `settings` (read from `file`) that
/// equals tm's exact value set `ours` (#12077): the detector's and the
/// cleanup's one matching rule for that tier.
pub(super) fn user_keys_in(file: &Path, settings: &Value, ours: &Value) -> Vec<TmSandboxKey> {
    let mut found = Vec::new();
    collect(file, settings, ours, "", USER_WHOLE_LISTS, &mut found);
    found
}

/// Record in `out` each leaf of `ours` that `have` holds with the same value.
///
/// What: objects recurse by key; a list under a key in `whole` counts only
/// when the two lists are equal, any other list per entry; a scalar counts
/// when equal.
fn collect(
    file: &Path,
    have: &Value,
    ours: &Value,
    path: &str,
    whole: &[&str],
    out: &mut Vec<TmSandboxKey>,
) {
    let push = |out: &mut Vec<TmSandboxKey>, key: String| {
        out.push(TmSandboxKey {
            file: file.to_path_buf(),
            key,
        });
    };
    match ours {
        Value::Object(map) => {
            for (key, value) in map {
                if let Some(theirs) = have.get(key) {
                    let child = if path.is_empty() {
                        key.clone()
                    } else {
                        format!("{path}.{key}")
                    };
                    collect(file, theirs, value, &child, whole, out);
                }
            }
        }
        Value::Array(list) => {
            let Some(theirs) = have.as_array() else {
                return;
            };
            let last = path.rsplit('.').next().unwrap_or(path);
            if whole.contains(&last) {
                if theirs == list {
                    push(out, path.to_owned());
                }
                return;
            }
            for entry in list.iter().filter(|e| theirs.contains(*e)) {
                push(out, format!("{path}[{}]", entry_label(entry)));
            }
        }
        scalar => {
            if have == scalar {
                push(out, path.to_owned());
            }
        }
    }
}

/// A list entry's label: an object's `name`, a string's text, else its JSON.
pub(super) fn entry_label(entry: &Value) -> String {
    match entry {
        Value::String(text) => text.clone(),
        Value::Object(map) => match map.get("name") {
            Some(Value::String(name)) => name.clone(),
            _ => entry.to_string(),
        },
        other => other.to_string(),
    }
}

/// The JSON object in `path`; `None` when the file does not exist.
pub(super) fn read_object(path: &Path) -> Result<Option<Value>, SandboxSettingsError> {
    let bad = |why: String| SandboxSettingsError::Unreadable {
        path: path.to_path_buf(),
        why,
    };
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(bad(e.to_string())),
    };
    match serde_json::from_str::<Value>(&text) {
        Ok(value) if value.is_object() => Ok(Some(value)),
        Ok(_) => Err(bad("not a JSON object".to_owned())),
        Err(e) => Err(bad(e.to_string())),
    }
}
