//! The one-time removal of the launch keys earlier tm releases wrote into a
//! project's `<ws>/.claude/settings.json` (#12127, parts 4a and 4b of #12077).
//!
//! Why: #12127 moves `outputStyle`, `model`, `autoMemoryEnabled` and
//! `skillOverrides` (4a), and `spinnerTipsEnabled`, `spinnerTipsOverride` and
//! `attribution` (4b), onto the `--settings` flag tier. The values earlier
//! releases left in the project file are tm's to take back, once, and no value
//! the user set may go.
//! What: [`clean_tm_launch_keys_once`] removes each of those keys, and each
//! `skillOverrides` entry, whose value equals what tm wrote ([`TmLaunchKeys`];
//! `spinnerTipsOverride` and `attribution` match only as a whole value),
//! reports every other value at those keys, and writes its own done marker
//! `<ws>/.trusty-mpm/`[`LAUNCH_KEYS_CLEANUP_MARKER_FILE`]. The marker is
//! distinct from the sandbox pass's (`sandbox-cleanup.json`), because that
//! marker already exists on workspaces 1.14.16's sandbox pass finished, and
//! each pass must finish and retry on its own. It reuses the sandbox pass's
//! read, lock, prune, publish and marker writer unchanged. Only
//! `settings.json` is touched: every earlier writer of these keys went through
//! `merge_settings` or the skill-override writer, which write that file alone.
//! Test: `claude_settings_keys_cleanup_tests.rs`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use super::cleanup::{SandboxCleanupError, marker_body, prune_locked, write_marker};
use super::detect::read_object;
use crate::core::session_launch::{COMPANION_KEYS, tm_companion_keys};
use crate::core::skill_overrides::{OFF, SKILL_OVERRIDES_KEY};

/// The done marker's file name under `<ws>/.trusty-mpm/`.
pub const LAUNCH_KEYS_CLEANUP_MARKER_FILE: &str = "launch-keys-cleanup.json";

/// The exact values tm wrote into the project file.
///
/// What: `outputStyle` = `output_style` (this launch's style id, the value
/// every earlier launch wrote unconditionally), `autoMemoryEnabled: false`,
/// `model` = `model` (the value tm's model record holds; `None` when there is
/// no record, so no `model` is tm's), and one `"off"` entry per skill in
/// `skills_off` (this launch's computed set; `None` when stack detection was
/// partial, so no entry is tm's). #12077 part 4b: the spinner tips and the
/// attribution footer are tm's constants ([`tm_companion_keys`]), so they need
/// no field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TmLaunchKeys {
    /// The style id tm wrote.
    pub output_style: String,
    /// The `model` tm recorded writing, if any.
    pub model: Option<String>,
    /// The skills tm turned off, or `None` when they cannot be known.
    pub skills_off: Option<BTreeSet<String>>,
}

impl TmLaunchKeys {
    /// tm's values as one settings object, the shape `prune` removes.
    fn as_value(&self) -> Value {
        let mut map = Map::new();
        map.insert("outputStyle".to_owned(), json!(self.output_style));
        map.insert("autoMemoryEnabled".to_owned(), json!(false));
        if let Some(model) = &self.model {
            map.insert("model".to_owned(), json!(model));
        }
        if let Some(skills) = self.skills_off.as_ref().filter(|s| !s.is_empty()) {
            let off = skills.iter().map(|s| (s.clone(), json!(OFF))).collect();
            map.insert(SKILL_OVERRIDES_KEY.to_owned(), Value::Object(off));
        }
        // #12077: part 4b's three keys, at the constants tm wrote.
        map.extend(tm_companion_keys());
        Value::Object(map)
    }
}

/// What one launch-key cleanup run did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchKeysCleanupOutcome {
    /// The marker exists; nothing was read or written.
    AlreadyDone,
    /// No `settings.json`; it is not created, and the marker records
    /// `"settings": "absent"` so a file the user writes later is never cleaned.
    NoSettingsFile,
    /// `settings.json` is a symlink; nothing was written, no marker either.
    SymlinkedSettings(PathBuf),
    /// The labels removed and kept, and the marker written.
    Cleaned {
        /// tm's values, removed (`outputStyle`, `skillOverrides.<skill>`, …).
        removed: Vec<String>,
        /// The user's values at tm's keys, kept because they differ.
        kept: Vec<String>,
    },
}

/// Remove tm's launch keys from `workspace`'s project settings, once.
///
/// What, in order:
/// 1. the marker exists: [`LaunchKeysCleanupOutcome::AlreadyDone`];
/// 2. `settings.json` is a symlink: [`LaunchKeysCleanupOutcome::SymlinkedSettings`],
///    no write and no marker;
/// 3. no `settings.json`: [`LaunchKeysCleanupOutcome::NoSettingsFile`],
///    nothing created under `.claude/`, the marker written;
/// 4. otherwise, when a value equals tm's, under the settings lock re-read,
///    remove it ([`prune_locked`]) and publish; then write the marker with
///    the removed and kept labels. A file holding none of tm's values is never
///    rewritten, so it stays byte-identical; a rewrite sorts the keys, as the
///    sandbox pass's does.
///
/// # Errors
///
/// [`SandboxCleanupError`] when a read, parse, lock, rewrite or the marker
/// write fails; the marker is then not written, so a later launch retries.
///
/// Test: `matching_launch_keys_are_removed_and_others_kept_12127`,
/// `a_file_with_no_tm_launch_key_is_byte_identical_12127`,
/// `a_second_launch_key_pass_writes_nothing_12127`,
/// `a_launch_key_parse_failure_writes_no_marker_12127`,
/// `a_launch_key_publish_failure_writes_no_marker_12127`,
/// `a_launch_key_marker_failure_writes_no_marker_12127`,
/// `a_symlinked_settings_file_keeps_its_launch_keys_12127`,
/// `a_missing_settings_file_is_marked_absent_12127`,
/// `a_partial_detection_removes_no_skill_entry_12127`,
/// `tms_companion_values_are_removed_and_edits_kept_12077`,
/// `a_companion_publish_failure_is_an_error_and_writes_no_marker_12077`,
/// `an_unreadable_settings_file_fails_the_cleanup_and_keeps_no_marker_12077`.
pub fn clean_tm_launch_keys_once(
    workspace: &Path,
    ours: &TmLaunchKeys,
) -> Result<LaunchKeysCleanupOutcome, SandboxCleanupError> {
    let marker = workspace
        .join(".trusty-mpm")
        .join(LAUNCH_KEYS_CLEANUP_MARKER_FILE);
    if marker.exists() {
        return Ok(LaunchKeysCleanupOutcome::AlreadyDone);
    }
    let file = workspace.join(".claude").join("settings.json");
    // #12127: `publish` renames over the path, which would replace the link.
    if std::fs::symlink_metadata(&file).is_ok_and(|m| m.file_type().is_symlink()) {
        return Ok(LaunchKeysCleanupOutcome::SymlinkedSettings(file));
    }
    let wanted = ours.as_value();
    let Some(settings) = read_object(&file)? else {
        return mark_absent(&marker, ours);
    };
    let read = if split(&settings, &wanted).0.is_empty() {
        // #12127: nothing of tm's, so no rewrite at all.
        settings
    } else {
        // #12077: part 4b's object values are tm's only when whole and equal.
        match prune_locked(&file, &wanted, &COMPANION_KEYS)? {
            Some(read) => read,
            None => return mark_absent(&marker, ours),
        }
    };
    let (removed, kept) = split(&read, &wanted);
    write_marker(&marker, &body(&removed, &kept, ours, false))?;
    Ok(LaunchKeysCleanupOutcome::Cleaned { removed, kept })
}

/// Mark the pass done with no settings file (#12127).
fn mark_absent(
    marker: &Path,
    ours: &TmLaunchKeys,
) -> Result<LaunchKeysCleanupOutcome, SandboxCleanupError> {
    write_marker(marker, &body(&[], &[], ours, true))?;
    Ok(LaunchKeysCleanupOutcome::NoSettingsFile)
}

/// The marker: [`marker_body`] for tier `launch-keys`, plus the kept labels
/// and `"skillOverrides": "unknown"` when detection was partial.
fn body(removed: &[String], kept: &[String], ours: &TmLaunchKeys, absent: bool) -> Value {
    let mut body = marker_body("launch-keys", removed, absent);
    body["refs"] = json!(12127);
    body["kept"] = json!(kept);
    if ours.skills_off.is_none() {
        body["skillOverrides"] = json!("unknown");
    }
    body
}

/// The labels of `have`'s values at tm's keys: (equal to tm's, different).
///
/// What: `skillOverrides` is matched per entry, and a `skillOverrides` that is
/// not an object is the user's; every other key, `spinnerTipsOverride` and
/// `attribution` included (#12077 part 4b), is tm's only when its whole value
/// is equal.
fn split(have: &Value, ours: &Value) -> (Vec<String>, Vec<String>) {
    let (mut removed, mut kept) = (Vec::new(), Vec::new());
    let (Some(have), Some(ours)) = (have.as_object(), ours.as_object()) else {
        return (removed, kept);
    };
    for (key, value) in ours {
        let Some(theirs) = have.get(key) else {
            continue;
        };
        let per_entry = key == SKILL_OVERRIDES_KEY;
        match (value, theirs) {
            (Value::Object(wanted), Value::Object(theirs)) if per_entry => {
                for (entry, v) in wanted {
                    if let Some(t) = theirs.get(entry) {
                        let label = format!("{key}.{entry}");
                        if t == v {
                            removed.push(label);
                        } else {
                            kept.push(label);
                        }
                    }
                }
            }
            (Value::Object(_), _) if per_entry => kept.push(key.clone()),
            _ if theirs == value => removed.push(key.clone()),
            _ => kept.push(key.clone()),
        }
    }
    (removed, kept)
}

/// The launch-key pass of a launch: never refuses it (#12127).
///
/// What: [`clean_tm_launch_keys_once`]; removed labels log at info, kept
/// values, a symlink skip, a partial detection and every failure (naming the
/// file) at warn.
/// Test: `the_launch_key_pass_never_refuses_12127`.
pub fn clean_tm_launch_keys_on_launch(workspace: &Path, ours: &TmLaunchKeys) {
    let file = workspace.join(".claude").join("settings.json");
    match clean_tm_launch_keys_once(workspace, ours) {
        Ok(LaunchKeysCleanupOutcome::Cleaned { removed, kept }) => {
            if !removed.is_empty() {
                tracing::info!(
                    path = %file.display(),
                    ?removed,
                    "#12127: removed the launch keys earlier tm releases wrote into the project settings"
                );
            }
            if !kept.is_empty() {
                tracing::warn!(
                    path = %file.display(),
                    ?kept,
                    "#12127: kept these project settings values because they differ from what tm wrote"
                );
            }
            if ours.skills_off.is_none() {
                tracing::warn!(
                    path = %file.display(),
                    "#12127: stack detection was partial, so no `skillOverrides` entry was removed"
                );
            }
        }
        Ok(LaunchKeysCleanupOutcome::SymlinkedSettings(path)) => tracing::warn!(
            path = %path.display(),
            "#12127: the project settings file is a symlink; the launch-key cleanup is skipped so the link is kept"
        ),
        Ok(_) => {}
        Err(err) => tracing::warn!(
            path = %file.display(),
            error = %err,
            "#12127: launch-key cleanup failed; the launch goes ahead and a later launch retries"
        ),
    }
}
