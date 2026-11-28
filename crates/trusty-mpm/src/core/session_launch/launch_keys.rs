//! The four Claude settings tm delivers on the `--settings` flag tier, and the
//! tm-owned record that carries them from `prepare_session` to the launch
//! (#12127).
//!
//! Why: #12077 — tm writes none of `outputStyle`, `model`, `autoMemoryEnabled`
//! or `skillOverrides` into a generic Claude settings file. `prepare_session`
//! still decides them; every launch path then reads them back when it builds
//! its one `--settings` payload (the owner carrier,
//! `crate::core::launch_policy::carrier`), because Claude Code keeps only the
//! last `--settings`. The record stands where the project file stood: a launch
//! that did not prepare reads what the last preparation of that directory left.
//! What: [`launch_keys_for`] computes the keys. The flag tier outranks the
//! project and local files, so each key is left out wherever the old
//! project-tier write let a user's value win (see the function).
//! [`write_launch_keys`] stores them under
//! `<harness>/.trusty-mpm/launch-settings/<sha256 of the dir>.json`, and
//! [`read_launch_keys`] reads them back for a launch cwd.
//! [`deliver_launch_keys`] is the prep step: the one-time removal of the
//! values earlier releases wrote into the project file
//! (`crate::core::claude_sandbox::clean_tm_launch_keys_on_launch`), then the
//! record write, whose failure refuses the launch.
//! #12077 row 13: the record also carries the `claudeMdExcludes` `tm doctor
//! --fix` adds for an ancestor `CLAUDE.md` ([`add_launch_exclude`]), so the
//! exclude reaches only this directory's tm launches; a launch keeps them.
//! Test: `launch_keys_tests.rs`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use super::PrepError;
use super::settings::AUTO_MEMORY_KEY;
use crate::core::claude_md_excludes::EXCLUDES_KEY;
use crate::core::claude_sandbox::{TmLaunchKeys, clean_tm_launch_keys_on_launch};
use crate::core::session_profile::{ARCHITECT_MODEL, SessionProfile};
use crate::core::skill_overrides::{OFF, SKILL_OVERRIDES_KEY, planned_skill_overrides};

/// The record directory under [`crate::core::harness_root::harness_dir`].
pub const LAUNCH_KEYS_DIR: &str = "launch-settings";

/// The Claude Code output-style key.
pub const OUTPUT_STYLE_KEY: &str = "outputStyle";

/// The Claude Code model key.
pub const MODEL_KEY: &str = "model";

/// Why the launch-settings record could not be read or written (#12127).
///
/// Why: the record is the only carrier of the four keys; a launch that cannot
/// read or write it would run without them, so every variant refuses.
/// Test: `a_malformed_record_refuses_the_carrier_12127`,
/// `an_unwritable_record_refuses_the_prepared_launch_12127`.
#[derive(Debug, thiserror::Error)]
pub enum LaunchKeysError {
    /// The record exists but could not be read.
    #[error(
        "#12127 launch settings: cannot read the launch-settings record {path}: {source}; \
         refusing the launch"
    )]
    Read {
        /// The record file.
        path: PathBuf,
        /// The read error.
        source: std::io::Error,
    },
    /// The record is not JSON or has no `settings` object.
    #[error(
        "#12127 launch settings: the launch-settings record {path} is not valid ({reason}); \
         refusing the launch"
    )]
    Malformed {
        /// The record file.
        path: PathBuf,
        /// What was wrong with it.
        reason: String,
    },
    /// The record could not be written.
    #[error(
        "#12127 launch settings: cannot write the launch-settings record {path}: {source}; \
         refusing the launch"
    )]
    Write {
        /// The record file.
        path: PathBuf,
        /// The write error.
        source: std::io::Error,
    },
}

/// `dir` canonicalised, or as given when it cannot be.
fn canonical(dir: &Path) -> PathBuf {
    std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf())
}

/// The record file for a launch in `dir`.
///
/// What: `<harness_dir>/launch-settings/<first 16 bytes of sha256(dir)>.json`,
/// over the canonical path, so a worktree and its main checkout keep apart
/// records under one harness and a symlinked spelling finds the same file.
/// Test: `a_record_round_trips_through_its_dir_12127`.
pub fn record_path(dir: &Path) -> PathBuf {
    use sha2::{Digest as _, Sha256};
    let dir = canonical(dir);
    let digest = Sha256::digest(dir.as_os_str().as_encoded_bytes());
    let hex: String = digest.iter().take(16).map(|b| format!("{b:02x}")).collect();
    crate::core::harness_root::harness_dir(&dir)
        .join(LAUNCH_KEYS_DIR)
        .join(format!("{hex}.json"))
}

/// A settings layer's top-level object; absent, unreadable or not an object
/// reads as empty, as Claude Code applies no value from it.
fn read_layer(path: &Path) -> Map<String, Value> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|value| match value {
            Value::Object(map) => Some(map),
            _ => None,
        })
        .unwrap_or_default()
}

/// The four keys a launch of `profile` in `project_dir` carries inline.
///
/// Why: the flag tier outranks the project and local files; the old
/// project-tier write did not, so a key is left out wherever it used to lose.
/// What, read after the one-time removal:
/// - `outputStyle` = `style_id` and `autoMemoryEnabled: false`, unless
///   `settings.local.json` sets the key (the old write overwrote the project
///   file, and the local file outranked it);
/// - `model` = [`ARCHITECT_MODEL`] for an Architect only, unless either file
///   sets `model` (the old write kept a user's value);
/// - `skillOverrides`: each of `skills_off` as `"off"` unless either file
///   names that skill (the old write added an entry only where none was); none
///   when either file holds a `skillOverrides` that is not an object (the old
///   write skipped such a file), and none for a partial detection (`None`).
///
/// Test: `the_record_carries_each_key_with_the_old_value_12127`,
/// `a_user_value_keeps_winning_where_it_used_to_12127`.
pub(super) fn launch_keys_for(
    project_dir: &Path,
    style_id: &str,
    profile: SessionProfile,
    skills_off: Option<&BTreeSet<&'static str>>,
) -> Map<String, Value> {
    let claude = project_dir.join(".claude");
    let project = read_layer(&claude.join("settings.json"));
    let local = read_layer(&claude.join("settings.local.json"));
    let mut keys = Map::new();
    if !local.contains_key(OUTPUT_STYLE_KEY) {
        keys.insert(OUTPUT_STYLE_KEY.to_owned(), json!(style_id));
    }
    if !local.contains_key(AUTO_MEMORY_KEY) {
        keys.insert(AUTO_MEMORY_KEY.to_owned(), json!(false));
    }
    if profile.is_architect() && !project.contains_key(MODEL_KEY) && !local.contains_key(MODEL_KEY)
    {
        keys.insert(MODEL_KEY.to_owned(), json!(ARCHITECT_MODEL));
    }
    let layers = [&project, &local].map(|layer| layer.get(SKILL_OVERRIDES_KEY).cloned());
    let unusable = layers.iter().flatten().any(|v| !v.is_object());
    let named = |skill: &str| layers.iter().flatten().any(|v| v.get(skill).is_some());
    let off: Map<String, Value> = skills_off
        .filter(|_| !unusable)
        .into_iter()
        .flatten()
        .filter(|skill| !named(skill))
        .map(|skill| ((*skill).to_owned(), json!(OFF)))
        .collect();
    if !off.is_empty() {
        keys.insert(SKILL_OVERRIDES_KEY.to_owned(), Value::Object(off));
    }
    keys
}

/// Store `keys` as the record for `project_dir`, atomically.
///
/// # Errors
///
/// [`LaunchKeysError::Write`] when the record cannot be staged or renamed.
///
/// Test: `a_record_round_trips_through_its_dir_12127`,
/// `an_unwritable_record_refuses_the_prepared_launch_12127`.
pub(super) fn write_launch_keys(
    project_dir: &Path,
    keys: &Map<String, Value>,
) -> Result<PathBuf, LaunchKeysError> {
    let path = record_path(project_dir);
    let body = json!({
        "issue": 12127,
        "dir": canonical(project_dir).display().to_string(),
        "settings": keys,
    });
    crate::core::settings_lock::publish(&path, &body).map_err(|source| LaunchKeysError::Write {
        path: path.clone(),
        source,
    })?;
    Ok(path)
}

/// The keys the last preparation of `dir` recorded; `None` when none did.
///
/// # Errors
///
/// [`LaunchKeysError::Read`] for an unreadable record,
/// [`LaunchKeysError::Malformed`] for one that is not JSON or has no
/// `settings` object.
///
/// Test: `a_record_round_trips_through_its_dir_12127`,
/// `a_malformed_record_refuses_the_carrier_12127`.
pub fn read_launch_keys(dir: &Path) -> Result<Option<Map<String, Value>>, LaunchKeysError> {
    let path = record_path(dir);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(LaunchKeysError::Read { path, source }),
    };
    let malformed = |reason: String| LaunchKeysError::Malformed {
        path: path.clone(),
        reason,
    };
    let value: Value = serde_json::from_str(&text).map_err(|e| malformed(e.to_string()))?;
    match value.get("settings") {
        Some(Value::Object(map)) => Ok(Some(map.clone())),
        _ => Err(malformed("no `settings` object".to_owned())),
    }
}

/// Set one of the four keys on the record for `dir`, keeping the others.
///
/// Why: #12077 — `tm doctor --fix` repairs `autoMemoryEnabled` on the record
/// the launch delivers it through, never in the project settings file.
/// What: reads the record (absent reads as empty), inserts `key`, and stores
/// it through [`write_launch_keys`].
///
/// # Errors
///
/// Every [`read_launch_keys`] error, unchanged, so a broken record is never
/// overwritten; [`LaunchKeysError::Write`] when the store fails.
///
/// Test: `auto_memory_repair_writes_the_launch_record_not_the_project_file_12077`,
/// `a_malformed_launch_record_fails_the_check_and_the_repair_12077`,
/// `an_unwritable_launch_record_fails_the_repair_12077`.
pub fn set_launch_key(dir: &Path, key: &str, value: Value) -> Result<PathBuf, LaunchKeysError> {
    let mut keys = read_launch_keys(dir)?.unwrap_or_default();
    keys.insert(key.to_owned(), value);
    write_launch_keys(dir, &keys)
}

/// Add `absolute` to `claudeMdExcludes` on the record for `dir` (#12077 row 13).
///
/// Why: an ancestor `CLAUDE.md` exclude written into a settings file reaches
/// every session that file reaches; on the record it reaches only this
/// directory's tm launches, which pass the record as `--settings`. Claude Code
/// unions the array with the file layers' entries.
/// What: reads the record (absent reads as empty), appends `absolute`'s
/// display form when it is not listed, and stores it through
/// [`write_launch_keys`]. `Ok(false)` when it was already listed.
///
/// # Errors
///
/// Every [`read_launch_keys`] error, so a broken record is never overwritten;
/// [`LaunchKeysError::Malformed`] when the recorded key is not an array;
/// [`LaunchKeysError::Write`] when the store fails.
///
/// Test: `the_exclude_lands_on_the_launch_settings_record_12077`,
/// `an_unreadable_launch_record_fails_the_exclude_12077`.
pub fn add_launch_exclude(dir: &Path, absolute: &Path) -> Result<bool, LaunchKeysError> {
    let mut keys = read_launch_keys(dir)?.unwrap_or_default();
    let wanted = absolute.display().to_string();
    let entry = keys
        .entry(EXCLUDES_KEY.to_owned())
        .or_insert_with(|| Value::Array(Vec::new()));
    let Some(list) = entry.as_array_mut() else {
        return Err(LaunchKeysError::Malformed {
            path: record_path(dir),
            reason: format!("`{EXCLUDES_KEY}` is not an array"),
        });
    };
    if list.iter().any(|v| v.as_str() == Some(wanted.as_str())) {
        return Ok(false);
    }
    list.push(Value::String(wanted));
    write_launch_keys(dir, &keys)?;
    Ok(true)
}

/// The `claudeMdExcludes` strings on the record for `dir`, best effort.
///
/// What: an absent or unreadable record, or a key that is not an array,
/// contributes nothing, as an unreadable settings layer does in
/// [`crate::core::claude_md_excludes::merged_excludes`]: the read can only
/// under-report, which shows the operator a finding rather than hiding one.
/// Test: `a_recorded_exclude_yields_no_step_12077`.
pub fn recorded_claude_md_excludes(dir: &Path) -> Vec<String> {
    let Ok(Some(keys)) = read_launch_keys(dir) else {
        return Vec::new();
    };
    keys.get(EXCLUDES_KEY)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str().map(str::to_owned))
        .collect()
}

/// The prep step for the four keys (#12127).
///
/// Why: see the module doc.
/// What: reads the `claudeMdExcludes` the current record carries (#12077 row
/// 13), removes, once, the values earlier releases wrote into the project
/// file (an unreadable model record skips the pass, unmarked, so a later launch
/// retries), then records [`launch_keys_for`] for the launch with those
/// excludes kept.
///
/// # Errors
///
/// [`PrepError::LaunchKeys`] when the current record cannot be read or the new
/// one cannot be written; it is fatal, so the launch is refused rather than
/// run without the keys or with the recorded excludes dropped.
///
/// Test: `prepare_session_writes_none_of_the_four_keys_12127`,
/// `an_unwritable_record_refuses_the_prepared_launch_12127`,
/// `deliver_keeps_the_recorded_claude_md_excludes_12077`,
/// `deliver_refuses_over_an_unreadable_record_12077`.
pub(super) fn deliver_launch_keys(
    project_dir: &Path,
    style_id: &str,
    profile: SessionProfile,
) -> Result<(), PrepError> {
    // #12077 row 13: read before any write, so a refusal changes nothing.
    let excludes = read_launch_keys(project_dir)?.and_then(|mut keys| keys.remove(EXCLUDES_KEY));
    let skills_off = planned_skill_overrides(project_dir);
    match super::architect_model::recorded_model(project_dir) {
        Ok(model) => clean_tm_launch_keys_on_launch(
            project_dir,
            &TmLaunchKeys {
                output_style: style_id.to_owned(),
                model,
                skills_off: skills_off
                    .as_ref()
                    .map(|set| set.iter().map(|s| (*s).to_owned()).collect()),
            },
        ),
        Err(err) => tracing::warn!(
            project = %project_dir.display(),
            error = %err,
            "#12127: cannot read tm's model record; the launch-key cleanup waits for a later launch"
        ),
    }
    let mut keys = launch_keys_for(project_dir, style_id, profile, skills_off.as_ref());
    if let Some(excludes) = excludes {
        keys.insert(EXCLUDES_KEY.to_owned(), excludes);
    }
    write_launch_keys(project_dir, &keys)?;
    Ok(())
}

/// Vouch, in the provisioning ledger, for the record and the cleanup marker a
/// launch in `project_dir` wrote (#12127), so a provisioned clone's dirt check
/// does not count them as work. Runs after the ledger's own `record`; a failure
/// only keeps that workspace at decommission, so it is logged.
///
/// Test: `a_spawn_provisioned_clone_passes_the_dirt_check_11221`.
pub(super) fn vouch_for_launch_keys(project_dir: &Path) {
    let ws = canonical(project_dir);
    let marker = ws
        .join(".trusty-mpm")
        .join(crate::core::claude_sandbox::LAUNCH_KEYS_CLEANUP_MARKER_FILE);
    for file in [record_path(project_dir), marker] {
        if let Err(err) =
            crate::session_manager::provisioning_ledger::record_harness_file(&ws, &file)
        {
            tracing::warn!(path = %file.display(), "#12127: not vouched for in the ledger: {err}");
        }
    }
}

/// The settings a launch in `project_dir` runs with, for tests: the project
/// file with the recorded keys laid over it, as the flag tier outranks it.
#[cfg(test)]
pub(crate) fn effective_settings(project_dir: &Path) -> Value {
    let mut file = Value::Object(read_layer(
        &project_dir.join(".claude").join("settings.json"),
    ));
    let keys = read_launch_keys(project_dir)
        .expect("a readable launch-settings record")
        .unwrap_or_default();
    crate::core::claude_sandbox::merge_settings_payload(&mut file, &Value::Object(keys));
    file
}

#[cfg(test)]
#[path = "launch_keys_tests.rs"]
mod tests;
