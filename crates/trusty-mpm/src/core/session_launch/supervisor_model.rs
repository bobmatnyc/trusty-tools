//! The supervisor `model` key in the project settings, owned by tm (#8453).
//!
//! Why: a supervisor launch writes `"model": "opus"` into
//! `.claude/settings.json`, because some launch paths pass no `--model`. The
//! first version of that write was one-way: a project switched back to the PM
//! profile kept the supervisor model, and a `model` the operator had set was
//! overwritten.
//! What: [`write_supervisor_model`] writes `model` only when it is absent or
//! holds the value tm recorded writing, and records the write under the
//! harness directory (`tm-written-model.json`, keyed by settings path). A PM
//! launch removes `model` only when tm recorded writing it and the value is
//! still tm's. A `model` tm did not write is never changed.
//! Test: `session_launch/tests_supervisor_profile_8453.rs`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::PrepError;
use crate::core::session_profile::{SUPERVISOR_MODEL, SessionProfile};

/// The record file, under [`crate::core::harness_root::harness_dir`].
const RECORD_FILE: &str = "tm-written-model.json";

/// The Claude Code settings key.
const MODEL_KEY: &str = "model";

/// The record: settings-file path → the `model` value tm wrote there.
type Record = BTreeMap<String, String>;

/// Where the record for `project_dir` lives.
fn record_path(project_dir: &Path) -> PathBuf {
    crate::core::harness_root::harness_dir(project_dir).join(RECORD_FILE)
}

/// The record key for `project_dir`'s settings file.
fn settings_key(project_dir: &Path) -> String {
    project_dir
        .join(".claude")
        .join("settings.json")
        .display()
        .to_string()
}

/// Read the record; absent → empty, unreadable or malformed → empty and warned
/// (tm then owns nothing, so it removes nothing).
fn load_record(path: &Path) -> Record {
    match std::fs::read_to_string(path) {
        Ok(raw) => serde_json::from_str(&raw).unwrap_or_else(|err| {
            tracing::warn!(path = %path.display(), "ignoring a malformed model record: {err}");
            Record::new()
        }),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Record::new(),
        Err(err) => {
            tracing::warn!(path = %path.display(), "cannot read the model record: {err}");
            Record::new()
        }
    }
}

/// Write the record; an empty record removes the file.
fn store_record(path: &Path, record: &Record) -> Result<(), PrepError> {
    let io = |source| PrepError::Io {
        path: path.to_path_buf(),
        source,
    };
    if record.is_empty() {
        return match std::fs::remove_file(path) {
            Err(err) if err.kind() != std::io::ErrorKind::NotFound => Err(io(err)),
            _ => Ok(()),
        };
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(io)?;
    }
    let body = serde_json::to_string_pretty(record).map_err(|e| io(std::io::Error::other(e)))?;
    std::fs::write(path, body).map_err(io)
}

/// Write or remove the supervisor `model` key for a launch of `profile`.
///
/// Why: see the module doc.
/// What: supervisor → set `model` to [`SUPERVISOR_MODEL`] when it is absent
/// or equals the recorded tm value, then record it; a user-set `model` is
/// kept and the record dropped. PM → with a record, remove `model` when it
/// still equals the recorded value, then drop the record; without one, touch
/// nothing.
/// Test: `a_supervisor_launch_gets_the_supervisor_prompt_style_and_model`,
/// `flipping_back_to_pm_removes_the_model_tm_wrote`,
/// `a_user_set_model_is_never_touched`.
pub(super) fn write_supervisor_model(
    project_dir: &Path,
    profile: SessionProfile,
) -> Result<(), PrepError> {
    let path = record_path(project_dir);
    let mut record = load_record(&path);
    let key = settings_key(project_dir);
    let owned = record.get(&key).cloned();
    if profile.is_supervisor() {
        let mut wrote = false;
        super::settings::merge_settings(project_dir, |settings| {
            let ours = match settings.get(MODEL_KEY) {
                None => true,
                Some(current) => owned
                    .as_deref()
                    .is_some_and(|v| current.as_str() == Some(v)),
            };
            if ours {
                settings[MODEL_KEY] = serde_json::Value::from(SUPERVISOR_MODEL);
                wrote = true;
            }
        })?;
        if wrote {
            record.insert(key, SUPERVISOR_MODEL.to_owned());
        } else {
            tracing::warn!(
                project = %project_dir.display(),
                "the project settings set their own `model`; the supervisor model \
                 `{SUPERVISOR_MODEL}` is passed on the command line only"
            );
            record.remove(&key);
        }
        return store_record(&path, &record);
    }
    let Some(owned) = owned else {
        return Ok(());
    };
    super::settings::merge_settings(project_dir, |settings| {
        if settings.get(MODEL_KEY).and_then(serde_json::Value::as_str) == Some(owned.as_str())
            && let Some(map) = settings.as_object_mut()
        {
            map.remove(MODEL_KEY);
        }
    })?;
    record.remove(&key);
    store_record(&path, &record)
}
