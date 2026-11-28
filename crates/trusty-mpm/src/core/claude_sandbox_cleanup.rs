//! The one-time removal of the sandbox keys tm 1.14.14 and 1.14.15 wrote into
//! a project's `<ws>/.claude/settings.json` (#12077 slice 3).
//!
//! Why: owner ruling 2026-11-28 — "tm removes its own keys once. At the next
//! launch tm deletes only the keys it wrote, then never writes that file
//! again. This is the one allowed write to the generic file."
//! What: [`clean_tm_sandbox_keys_once`] removes, from the project settings
//! file only, each key and list entry that equals the workspace's role record
//! (the detector's matching rule), then writes a done marker
//! (`<ws>/.trusty-mpm/`[`CLEANUP_MARKER_FILE`]). [`clean_tm_sandbox_keys_on_launch`]
//! is the launch wrapper: it logs a failure and never refuses the launch.
//! `settings.local.json` is never touched: 1.14.14 and 1.14.15 only read it
//! (`sandbox_settings_verify.rs`), and their one writer, `write_project_tier`,
//! went through `merge_settings`, which writes `settings.json` alone. The user
//! tiers are out of scope (owner decision pending).
//! Test: `claude_sandbox_cleanup_tests.rs`.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::TmSandboxKey;
use super::detect::{project_keys_in, read_object, recorded_project_policy};
use crate::core::session_launch::SandboxSettingsError;

/// The done marker's file name under `<ws>/.trusty-mpm/`.
pub const CLEANUP_MARKER_FILE: &str = "sandbox-cleanup.json";

/// What one cleanup run did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CleanupOutcome {
    /// The marker exists; nothing was read or written.
    AlreadyDone,
    /// No role record, so tm owns nothing; nothing was written.
    NoRecord,
    /// No `<ws>/.claude/settings.json`; it is not created, and the marker
    /// records `"settings": "absent"` so a file the user writes later is
    /// never cleaned.
    NoSettingsFile,
    /// `<ws>/.claude/settings.json` is a symlink; nothing was written, no
    /// marker either, so the user's link is never replaced by a regular file.
    SymlinkedSettings(PathBuf),
    /// The keys removed (empty when none matched) and the marker written.
    Cleaned(Vec<TmSandboxKey>),
}

/// A cleanup run that failed; the marker was not written.
#[derive(Debug, thiserror::Error)]
pub enum SandboxCleanupError {
    /// The role record or the settings file could not be read or parsed.
    #[error("#12077 sandbox key cleanup: {0}")]
    Read(#[from] SandboxSettingsError),
    /// The settings file could not be locked or rewritten.
    #[error("#12077 sandbox key cleanup: rewriting `{path}` failed: {source}")]
    Write {
        /// The settings file.
        path: PathBuf,
        /// The underlying error.
        source: std::io::Error,
    },
    /// The done marker could not be written.
    #[error("#12077 sandbox key cleanup: writing the done marker `{path}` failed: {source}")]
    Marker {
        /// The marker file.
        path: PathBuf,
        /// The underlying error.
        source: std::io::Error,
    },
}

/// Remove tm's 1.14.14/1.14.15 sandbox keys from `workspace`'s project
/// settings file, once.
///
/// Why: see the module doc.
/// What, in order:
/// 1. the marker exists: [`CleanupOutcome::AlreadyDone`], no read;
/// 2. no role record: [`CleanupOutcome::NoRecord`], no write at all;
/// 3. `settings.json` is a symlink: [`CleanupOutcome::SymlinkedSettings`], no
///    write and no marker;
/// 4. no `settings.json`: [`CleanupOutcome::NoSettingsFile`], nothing created
///    under `.claude/`, the marker written;
/// 5. otherwise, under the settings lock, remove each key and list entry equal
///    to the recorded policy ([`prune`]), rewrite the file only when something
///    was removed, then write the marker.
///
/// The rewrite uses the settings writer (`settings_lock::publish`): pretty
/// JSON in serde_json's map order, so a file that was not pretty-printed is
/// reformatted. `preserve_order` is off for this crate (only a
/// build-dependency enables it), so the rewrite sorts the keys. A container
/// that held only tm's entries is removed with them.
///
/// # Errors
///
/// [`SandboxCleanupError`] when a read, parse, lock, rewrite or the marker
/// write fails; the marker is then not written, so a later launch retries.
///
/// Test: `only_matching_keys_and_entries_are_removed_12077`,
/// `a_changed_value_is_left_alone_12077`, `no_role_record_writes_nothing_12077`,
/// `a_second_launch_writes_nothing_12077`,
/// `a_parse_failure_writes_no_marker_and_retries_12077`,
/// `a_lock_failure_writes_no_marker_12077`,
/// `a_publish_failure_writes_no_marker_12077`,
/// `a_record_parse_failure_writes_no_marker_12077`,
/// `a_marker_write_failure_leaves_the_prune_and_retries_once_12077`,
/// `a_missing_settings_file_is_not_created_12077`,
/// `a_settings_file_written_after_an_absent_run_is_never_cleaned_12077`,
/// `a_symlinked_settings_file_is_skipped_12077`,
/// `the_local_settings_file_is_never_touched_12077`.
pub fn clean_tm_sandbox_keys_once(workspace: &Path) -> Result<CleanupOutcome, SandboxCleanupError> {
    let marker = workspace.join(".trusty-mpm").join(CLEANUP_MARKER_FILE);
    if marker.exists() {
        return Ok(CleanupOutcome::AlreadyDone);
    }
    let Some(ours) = recorded_project_policy(workspace)? else {
        return Ok(CleanupOutcome::NoRecord);
    };
    let file = workspace.join(".claude").join("settings.json");
    // #12077: `publish` renames over the path, which would replace the user's
    // link with a regular file; skip, unmarked.
    if std::fs::symlink_metadata(&file).is_ok_and(|m| m.file_type().is_symlink()) {
        return Ok(CleanupOutcome::SymlinkedSettings(file));
    }
    // Checked before the lock, whose sidecar would create `.claude/`.
    let Some(settings) = read_object(&file)? else {
        // #12077: mark an absent file done, or a file written later is cleaned.
        write_marker(&marker, Vec::new(), true)?;
        return Ok(CleanupOutcome::NoSettingsFile);
    };
    let removed = if project_keys_in(&file, &settings, &ours).is_empty() {
        Vec::new()
    } else {
        let write = |source| SandboxCleanupError::Write {
            path: file.clone(),
            source,
        };
        let locked = crate::core::settings_lock::with_settings_lock(
            &file,
            || -> Result<Option<Vec<TmSandboxKey>>, SandboxCleanupError> {
                // Re-read inside the lock: another writer may have published.
                let Some(mut settings) = read_object(&file)? else {
                    return Ok(None);
                };
                let removed = project_keys_in(&file, &settings, &ours);
                if prune(&mut settings, &ours) {
                    crate::core::settings_lock::publish(&file, &settings).map_err(write)?;
                }
                Ok(Some(removed))
            },
        )
        .map_err(write)?;
        match locked? {
            Some(removed) => removed,
            None => {
                // #12077: the file went while we waited; mark it done as absent.
                write_marker(&marker, Vec::new(), true)?;
                return Ok(CleanupOutcome::NoSettingsFile);
            }
        }
    };
    let labels = removed.iter().map(|k| k.key.clone()).collect();
    write_marker(&marker, labels, false)?;
    Ok(CleanupOutcome::Cleaned(removed))
}

/// Write the done marker: the removed key labels, plus `"settings": "absent"`
/// when there was no settings file (#12077).
fn write_marker(
    marker: &Path,
    removed: Vec<String>,
    absent: bool,
) -> Result<(), SandboxCleanupError> {
    let mut body = json!({"issue": 12077, "removed": removed});
    if absent {
        body["settings"] = json!("absent");
    }
    crate::core::settings_lock::publish(marker, &body).map_err(|source| {
        SandboxCleanupError::Marker {
            path: marker.to_path_buf(),
            source,
        }
    })
}

/// [`clean_tm_sandbox_keys_once`] for a launch: never refuses it.
///
/// Why: #12077 — the cleanup must never block a launch, and a failure must
/// not be silent.
/// What: logs the removed keys at info; logs a failure, which names the file,
/// at warn and returns, so the launch goes ahead and a later one retries.
/// Test: `the_shared_launch_step_runs_the_cleanup_and_never_refuses_12077`.
pub fn clean_tm_sandbox_keys_on_launch(workspace: &Path) {
    match clean_tm_sandbox_keys_once(workspace) {
        Ok(CleanupOutcome::Cleaned(removed)) if !removed.is_empty() => {
            let keys: Vec<&str> = removed.iter().map(|k| k.key.as_str()).collect();
            tracing::info!(
                workspace = %workspace.display(),
                ?keys,
                "#12077: removed the sandbox keys tm 1.14.14/1.14.15 wrote into the project settings"
            );
        }
        // #12077: name the link, so the user knows why the cleanup waits.
        Ok(CleanupOutcome::SymlinkedSettings(path)) => tracing::warn!(
            path = %path.display(),
            "#12077: the project settings file is a symlink; the sandbox key cleanup is skipped so the link is kept"
        ),
        Ok(_) => {}
        Err(err) => tracing::warn!(
            workspace = %workspace.display(),
            error = %err,
            "#12077: sandbox key cleanup failed; the launch goes ahead and a later launch retries"
        ),
    }
}

/// Remove from `have` each leaf of `ours` it holds with the same value;
/// `true` when `have` changed.
///
/// What: the detector's rule (`collect` with no whole-list keys) applied as a
/// removal. Objects recurse by key; a list loses each entry `ours` lists; a
/// scalar key goes when equal. A container emptied by the removal goes too.
/// `retain` keeps the remaining keys and entries in their order.
fn prune(have: &mut Value, ours: &Value) -> bool {
    match (have, ours) {
        (Value::Object(map), Value::Object(wanted)) => {
            let mut changed = false;
            for (key, value) in wanted {
                let Some(child) = map.get_mut(key) else {
                    continue;
                };
                let drop = match value {
                    Value::Object(_) | Value::Array(_) => {
                        let pruned = prune(child, value);
                        changed |= pruned;
                        pruned && is_empty_container(child)
                    }
                    scalar => child == scalar,
                };
                if drop {
                    map.retain(|k, _| k != key);
                    changed = true;
                }
            }
            changed
        }
        (Value::Array(list), Value::Array(wanted)) => {
            let before = list.len();
            list.retain(|entry| !wanted.contains(entry));
            list.len() != before
        }
        _ => false,
    }
}

/// Whether `value` is an empty object or an empty list.
fn is_empty_container(value: &Value) -> bool {
    match value {
        Value::Object(map) => map.is_empty(),
        Value::Array(list) => list.is_empty(),
        _ => false,
    }
}
