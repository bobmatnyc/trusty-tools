//! The record of the supervisor `model` tm wrote into the project settings
//! (#8453), read now only by the one-time launch-key cleanup (#12127).
//!
//! Why: before #12127 a supervisor launch wrote `"model": "opus"` into
//! `.claude/settings.json` and recorded each write under the harness directory
//! (`tm-written-model.json`, keyed by settings path). #12127 carries the pin on
//! the `--settings` flag tier instead (`launch_keys`), so tm writes no `model`
//! any more; the record is what tells the cleanup, exactly, which `model`
//! value in a project file is tm's.
//! What: [`recorded_model`] returns the value tm recorded writing into
//! `project_dir`'s settings file. A `model` tm did not record is never removed.
//! Test: `a_model_tm_recorded_is_removed_and_any_other_kept_12127`,
//! `an_unrecorded_model_is_kept_12127`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The record file, under [`crate::core::harness_root::harness_dir`].
const RECORD_FILE: &str = "tm-written-model.json";

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

/// The `model` value tm recorded writing into `project_dir`'s settings file.
///
/// What: absent record or entry → `None`; a malformed record is warned about
/// and reads as `None`, so tm owns nothing and removes nothing.
///
/// # Errors
///
/// The read error when the record exists but cannot be read, so the cleanup
/// leaves its pass unmarked and a later launch retries.
///
/// Test: `a_model_tm_recorded_is_removed_and_any_other_kept_12127`.
pub(super) fn recorded_model(project_dir: &Path) -> std::io::Result<Option<String>> {
    let path = record_path(project_dir);
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err),
    };
    match serde_json::from_str::<BTreeMap<String, String>>(&raw) {
        Ok(mut record) => Ok(record.remove(&settings_key(project_dir))),
        Err(err) => {
            tracing::warn!(path = %path.display(), "ignoring a malformed model record: {err}");
            Ok(None)
        }
    }
}
