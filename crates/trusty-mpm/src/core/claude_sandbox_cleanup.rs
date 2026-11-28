//! The one-time removal of the sandbox keys tm 1.14.14 and 1.14.15 wrote into
//! a project's `<ws>/.claude/settings.json` (#12077 slice 3), and the launch
//! pass that also runs the user tier and deletes the role record.
//!
//! Why (#12077): "tm removes its own keys once. At the next
//! launch tm deletes only the keys it wrote, then never writes that file
//! again. This is the one allowed write to the generic file."
//! What: [`clean_tm_sandbox_keys_once`] removes, from the project settings
//! file only, each key and list entry that equals the workspace's role record
//! (the detector's matching rule), then writes a done marker
//! (`<ws>/.trusty-mpm/`[`CLEANUP_MARKER_FILE`]). [`clean_tm_sandbox_keys_on_launch`]
//! is the launch wrapper: it runs the project tier, then tm's user tier
//! (`claude_sandbox_cleanup_user.rs`), then deletes the role record when the
//! switch is off; it logs a failure and never refuses the launch.
//! `settings.local.json` is never touched: 1.14.14 and 1.14.15 only read it
//! (`sandbox_settings_verify.rs`), and their one writer, `write_project_tier`,
//! went through `merge_settings`, which writes `settings.json` alone.
//! `~/.claude/settings.json` is never touched: the 1.14.14 writer refused it.
//! Test: `claude_sandbox_cleanup_tests.rs`.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::TmSandboxKey;
use super::cleanup_user::{clean_tm_user_sandbox_keys_once, log_user_outcome};
use super::detect::{project_keys_in, read_object, recorded_project_policy};
use crate::core::session_launch::{SandboxRole, SandboxSettingsError};

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
        write_marker(&marker, &marker_body("project", &[], true))?;
        return Ok(CleanupOutcome::NoSettingsFile);
    };
    let removed = if project_keys_in(&file, &settings, &ours).is_empty() {
        Vec::new()
    } else {
        // #12077 slice 3b: the one write path both tiers share.
        match prune_locked(&file, &ours, &[])? {
            Some(read) => project_keys_in(&file, &read, &ours),
            None => {
                // #12077: the file went while we waited; mark it done as absent.
                write_marker(&marker, &marker_body("project", &[], true))?;
                return Ok(CleanupOutcome::NoSettingsFile);
            }
        }
    };
    write_marker(
        &marker,
        &marker_body("project", &removed_labels(&removed), false),
    )?;
    Ok(CleanupOutcome::Cleaned(removed))
}

/// Under `file`'s settings lock, re-read it, remove each leaf of `ours` it
/// holds with tm's value ([`prune`]) and publish only when something went.
///
/// Why: #12077 — both tiers take the same lock, temp-file and rename path.
/// What: returns the object as read inside the lock, before the prune, so the
/// caller labels what went; `None` when the file went while we waited. A list
/// under a key in `whole` is tm's only when it equals tm's list.
///
/// # Errors
///
/// [`SandboxCleanupError::Read`] for a re-read failure,
/// [`SandboxCleanupError::Write`] for a lock or publish failure.
pub(super) fn prune_locked(
    file: &Path,
    ours: &Value,
    whole: &[&str],
) -> Result<Option<Value>, SandboxCleanupError> {
    let write = |source| SandboxCleanupError::Write {
        path: file.to_path_buf(),
        source,
    };
    crate::core::settings_lock::with_settings_lock(
        file,
        || -> Result<Option<Value>, SandboxCleanupError> {
            // Re-read inside the lock: another writer may have published.
            let Some(read) = read_object(file)? else {
                return Ok(None);
            };
            let mut pruned = read.clone();
            if prune(&mut pruned, ours, whole) {
                crate::core::settings_lock::publish(file, &pruned).map_err(write)?;
            }
            Ok(Some(read))
        },
    )
    .map_err(write)?
}

/// The key labels of `removed`, for a marker.
pub(super) fn removed_labels(removed: &[TmSandboxKey]) -> Vec<String> {
    removed.iter().map(|k| k.key.clone()).collect()
}

/// A done marker's body: the tier that ran (#12077 slice 3b), the removed key
/// labels, plus `"settings": "absent"` when there was no settings file.
pub(super) fn marker_body(tier: &str, removed: &[String], absent: bool) -> Value {
    let mut body = json!({"issue": 12077, "tier": tier, "removed": removed});
    if absent {
        body["settings"] = json!("absent");
    }
    body
}

/// Write the done marker `body` (#12077).
pub(super) fn write_marker(marker: &Path, body: &Value) -> Result<(), SandboxCleanupError> {
    crate::core::settings_lock::publish(marker, body).map_err(|source| {
        SandboxCleanupError::Marker {
            path: marker.to_path_buf(),
            source,
        }
    })
}

/// The one-time cleanup pass of a launch: never refuses it.
///
/// Why: #12077 — the cleanup must never block a launch, and a failure must
/// not be silent. MB1693 answers 1 and 2: the same pass cleans tm's user tier
/// and then deletes the role record.
/// What, in order, each step run whatever the others returned:
/// 1. the project tier, [`clean_tm_sandbox_keys_once`];
/// 2. tm's user tier `<config_dir>/settings.json` when the launch has a
///    config dir, [`clean_tm_user_sandbox_keys_once`];
/// 3. when `opted_in` is false and the project tier's marker exists, so
///    step 1 has read the record, the role record is deleted, but only when
///    `role` could have shared it ([`delete_role_record`]).
///
/// Each tier writes its own marker, and only when it finished, so a partial
/// success re-runs only the tier that did not finish. Removed keys log at
/// info; kept user-tier values, a symlink skip and every failure (naming its
/// file) log at warn, and the launch goes ahead.
///
/// Test: `the_shared_launch_step_runs_the_cleanup_and_never_refuses_12077`,
/// `the_role_record_is_deleted_only_when_the_flag_is_off_12077`,
/// `a_launch_keeps_a_record_of_a_role_it_never_shared_12077`,
/// `a_user_tier_write_failure_marks_only_the_project_tier_then_retries_12077`,
/// `a_file_with_no_tm_key_is_byte_identical_in_both_tiers_12077`.
pub fn clean_tm_sandbox_keys_on_launch(
    workspace: &Path,
    config_dir: Option<&Path>,
    home: &Path,
    role: SandboxRole,
    opted_in: bool,
) {
    log_project_outcome(workspace, clean_tm_sandbox_keys_once(workspace));
    // #12077 slice 3b: owner answer 1, the user tier, in the same pass.
    if let Some(dir) = config_dir {
        log_user_outcome(dir, clean_tm_user_sandbox_keys_once(dir, home));
    }
    // #12077 slice 3b: owner answer 2 — the record goes once the project tier
    // has read it and finished; an opted-in launch keeps it.
    let tm_dir = workspace.join(".trusty-mpm");
    if !opted_in && tm_dir.join(CLEANUP_MARKER_FILE).exists() {
        delete_role_record(&tm_dir, role);
    }
}

/// Delete `<ws>/.trusty-mpm/sandbox-roles.json` when every role it names is
/// one `role` could have shared it with under 1.14.14/1.14.15.
///
/// Why: #12077 (PR #12133) — `tm fleet init` is an Architect launch, and it
/// deleted a PM's record. 1.14.14/1.14.15 refused an Architect launch over a
/// PM or agent entry (`sandbox_roles::conflicting_role`), so that record is
/// never one the Architect's launch wrote.
/// What: a PM or agent launch deletes a record naming only `pm`/`agent`; an
/// Architect launch one naming only `architect`. A record naming another
/// role, an empty record, or one that cannot be read is kept and logged.
fn delete_role_record(tm_dir: &Path, role: SandboxRole) {
    let record = tm_dir.join(super::ROLE_RECORD_FILE);
    match read_object(&record) {
        Ok(None) => return,
        Ok(Some(roles)) if shares_record(&roles, role) => {}
        // #12077: another role's record, so not this launch's to delete.
        Ok(Some(roles)) => {
            let named: Vec<&str> = roles
                .as_object()
                .map(|m| m.keys().map(String::as_str).collect())
                .unwrap_or_default();
            tracing::info!(
                path = %record.display(),
                ?role,
                ?named,
                "#12077: kept the sandbox role record; it names a role this launch never shared it with"
            );
            return;
        }
        // #12077: an unreadable record names no owner, so it is kept.
        Err(err) => {
            tracing::warn!(
                path = %record.display(),
                error = %err,
                "#12077: the sandbox role record cannot be read, so it is kept; a later launch retries"
            );
            return;
        }
    }
    match std::fs::remove_file(&record) {
        Ok(()) => tracing::info!(
            path = %record.display(),
            "#12077: deleted the sandbox role record tm 1.14.14/1.14.15 wrote"
        ),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => tracing::warn!(
            path = %record.display(),
            error = %e,
            "#12077: deleting the sandbox role record failed; a later launch retries"
        ),
    }
}

/// Whether `role` could have shared the role record `roles` (#12077): a
/// non-empty object whose every key is a role of `role`'s sandbox class.
///
/// What: the keys are `sandbox_roles::role_key`'s, frozen with the 1.14.15
/// record format; `pm` and `agent` shared one record, `architect` none.
fn shares_record(roles: &Value, role: SandboxRole) -> bool {
    let class: &[&str] = if role == SandboxRole::Architect {
        &["architect"]
    } else {
        &["pm", "agent"]
    };
    roles
        .as_object()
        .is_some_and(|m| !m.is_empty() && m.keys().all(|k| class.contains(&k.as_str())))
}

/// Log one project-tier run (#12077).
fn log_project_outcome(workspace: &Path, outcome: Result<CleanupOutcome, SandboxCleanupError>) {
    match outcome {
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
/// What: the detector's rule (`collect`) applied as a removal. Objects recurse
/// by key; a list under a key in `whole` goes only when equal to tm's list,
/// any other list loses each entry `ours` lists; a scalar key goes when equal.
/// A container emptied by the removal goes too. `retain` keeps the remaining
/// keys and entries in their order.
fn prune(have: &mut Value, ours: &Value, whole: &[&str]) -> bool {
    match (have, ours) {
        (Value::Object(map), Value::Object(wanted)) => {
            let mut changed = false;
            for (key, value) in wanted {
                let Some(child) = map.get_mut(key) else {
                    continue;
                };
                let drop = match value {
                    // #12077 slice 3b: a whole-list key is tm's only when equal.
                    Value::Array(_) if whole.contains(&key.as_str()) => child == value,
                    Value::Object(_) | Value::Array(_) => {
                        let pruned = prune(child, value, whole);
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
