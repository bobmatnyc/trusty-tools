//! The one-time removal of the sandbox keys tm 1.14.14 and 1.14.15 wrote into
//! tm's user tier, `<config_dir>/settings.json` (#12077 slice 3b).
//!
//! Why: MB1693 owner answer 1 — "clean it in slice 3. Remove a key only when
//! its value equals the exact value tm wrote. Any other value stays, and the
//! run reports it."
//! What: [`clean_tm_user_sandbox_keys_once`] removes each key and list entry
//! equal to tm's exact user-tier value set (`user_policy`, the detector's
//! rule), reports every other value at those keys, and writes a done marker
//! beside the file (`<config_dir>/`[`USER_CLEANUP_MARKER_FILE`]). The config
//! dir is the one each launch path already hands the sandbox step, the same
//! dir 1.14.14 and 1.14.15 wrote into. The marker sits there, not in a
//! workspace, because every workspace shares the file: one cleanup per file.
//! Test: `claude_sandbox_cleanup_user_tests.rs`.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::TmSandboxKey;
use super::cleanup::{
    SandboxCleanupError, marker_body, prune_locked, removed_labels, write_marker,
};
use super::detect::{USER_WHOLE_LISTS, entry_label, read_object, user_keys_in};
use crate::core::session_launch::{SandboxRole, user_policy};

/// The user-tier done marker's file name under the config dir.
pub const USER_CLEANUP_MARKER_FILE: &str = "tm-sandbox-cleanup.json";

/// What one user-tier cleanup run did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UserCleanupOutcome {
    /// The marker exists; nothing was read or written.
    AlreadyDone,
    /// A relative dir or the operator's `<home>/.claude`, which the 1.14.14
    /// and 1.14.15 writer refused; nothing was read or written.
    NotTmOwned(PathBuf),
    /// The config dir does not exist; it is not created, and no marker.
    NoConfigDir,
    /// No `settings.json`; it is not created, and the marker records
    /// `"settings": "absent"` so a file written later is never cleaned.
    NoSettingsFile,
    /// `settings.json` is a symlink; nothing was written, no marker either.
    SymlinkedSettings(PathBuf),
    /// The keys removed, the labels of the values kept at tm's keys because
    /// they differ from tm's, and the marker written.
    Cleaned {
        /// tm's keys and entries, removed.
        removed: Vec<TmSandboxKey>,
        /// The user's values at tm's keys, kept.
        kept: Vec<String>,
    },
}

/// Remove tm's 1.14.14/1.14.15 sandbox keys from `<config_dir>/settings.json`,
/// once.
///
/// Why: see the module doc.
/// What, in order:
/// 1. `config_dir` is relative or `<home>/.claude`: [`UserCleanupOutcome::NotTmOwned`];
/// 2. `config_dir` does not exist: [`UserCleanupOutcome::NoConfigDir`];
/// 3. the marker exists: [`UserCleanupOutcome::AlreadyDone`];
/// 4. `settings.json` is a symlink: [`UserCleanupOutcome::SymlinkedSettings`],
///    no write and no marker;
/// 5. no `settings.json`: [`UserCleanupOutcome::NoSettingsFile`], the marker
///    written;
/// 6. otherwise, under the settings lock, remove each key and entry equal to
///    tm's value set, rewrite only when one matched, then write the marker
///    with the removed and kept labels. A file with nothing of tm's is never
///    rewritten; a rewrite sorts the keys (`preserve_order` is off).
///
/// # Errors
///
/// [`SandboxCleanupError`] when a read, parse, lock, rewrite or the marker
/// write fails; the marker is then not written, so a later launch retries.
///
/// Test: `a_matching_user_tier_key_is_removed_12077`,
/// `a_different_user_tier_value_stays_and_is_reported_12077`,
/// `a_user_added_env_vars_entry_stays_12077`,
/// `a_user_tier_parse_failure_writes_no_marker_12077`,
/// `a_user_tier_write_failure_marks_only_the_project_tier_then_retries_12077`,
/// `a_symlinked_user_tier_file_is_skipped_12077`,
/// `a_missing_user_tier_file_is_not_created_12077`,
/// `a_dir_tm_never_wrote_is_never_cleaned_12077`.
pub fn clean_tm_user_sandbox_keys_once(
    config_dir: &Path,
    home: &Path,
) -> Result<UserCleanupOutcome, SandboxCleanupError> {
    // #12077: the 1.14.15 writer's own refusal (`apply_launch_sandbox`).
    if !config_dir.is_absolute() || config_dir == home.join(".claude") {
        return Ok(UserCleanupOutcome::NotTmOwned(config_dir.to_path_buf()));
    }
    if !config_dir.is_dir() {
        return Ok(UserCleanupOutcome::NoConfigDir);
    }
    let marker = config_dir.join(USER_CLEANUP_MARKER_FILE);
    if marker.exists() {
        return Ok(UserCleanupOutcome::AlreadyDone);
    }
    let file = config_dir.join("settings.json");
    // #12077: `publish` renames over the path, which would replace the link.
    if std::fs::symlink_metadata(&file).is_ok_and(|m| m.file_type().is_symlink()) {
        return Ok(UserCleanupOutcome::SymlinkedSettings(file));
    }
    // Every non-Architect role wrote the same value set.
    let ours = user_policy(SandboxRole::Pm).expect("the PM role always has a user-tier policy");
    let Some(settings) = read_object(&file)? else {
        return mark_absent(&marker, &file);
    };
    let read = if user_keys_in(&file, &settings, &ours).is_empty() {
        // #12077 owner answer 4: nothing of tm's, so no rewrite at all.
        settings
    } else {
        match prune_locked(&file, &ours, USER_WHOLE_LISTS)? {
            Some(read) => read,
            // #12077: the file went while we waited; mark it done as absent.
            None => return mark_absent(&marker, &file),
        }
    };
    let removed = user_keys_in(&file, &read, &ours);
    let mut kept = Vec::new();
    collect_kept(&read, &ours, "", &mut kept);
    write_marker(
        &marker,
        &user_body(&file, &removed_labels(&removed), &kept, false),
    )?;
    Ok(UserCleanupOutcome::Cleaned { removed, kept })
}

/// Mark the user tier done with no settings file, so a file written later is
/// never cleaned (#12077).
fn mark_absent(marker: &Path, file: &Path) -> Result<UserCleanupOutcome, SandboxCleanupError> {
    write_marker(marker, &user_body(file, &[], &[], true))?;
    Ok(UserCleanupOutcome::NoSettingsFile)
}

/// The user-tier marker: [`marker_body`] for tier `user`, plus the file and
/// the kept labels (#12077).
fn user_body(file: &Path, removed: &[String], kept: &[String], absent: bool) -> Value {
    let mut body = marker_body("user", removed, absent);
    body["file"] = json!(file.display().to_string());
    body["kept"] = json!(kept);
    body
}

/// Record in `out` the label of each value `have` holds at one of tm's keys
/// that is not tm's value: the complement of the detector's rule.
///
/// What: objects recurse by key; a list under a [`USER_WHOLE_LISTS`] key is
/// kept whole when it differs, any other list reports each entry tm did not
/// write; a scalar is kept when it differs. A value of another shape is kept.
fn collect_kept(have: &Value, ours: &Value, path: &str, out: &mut Vec<String>) {
    match ours {
        Value::Object(map) => {
            let Some(theirs) = have.as_object() else {
                out.push(path.to_owned());
                return;
            };
            for (key, value) in map {
                if let Some(child) = theirs.get(key) {
                    let at = if path.is_empty() {
                        key.clone()
                    } else {
                        format!("{path}.{key}")
                    };
                    collect_kept(child, value, &at, out);
                }
            }
        }
        Value::Array(list) => {
            let last = path.rsplit('.').next().unwrap_or(path);
            match have.as_array() {
                Some(theirs) if !USER_WHOLE_LISTS.contains(&last) => {
                    for entry in theirs.iter().filter(|e| !list.contains(*e)) {
                        out.push(format!("{path}[{}]", entry_label(entry)));
                    }
                }
                _ if have != ours => out.push(path.to_owned()),
                _ => {}
            }
        }
        scalar => {
            if have != scalar {
                out.push(path.to_owned());
            }
        }
    }
}

/// Log one user-tier run of a launch (#12077 slice 3b).
///
/// What: removed keys at info; kept values, a symlink skip and a failure
/// (which names the file) at warn.
pub(super) fn log_user_outcome(
    config_dir: &Path,
    outcome: Result<UserCleanupOutcome, SandboxCleanupError>,
) {
    let file = config_dir.join("settings.json");
    match outcome {
        Ok(UserCleanupOutcome::Cleaned { removed, kept }) => {
            if !removed.is_empty() {
                let keys: Vec<&str> = removed.iter().map(|k| k.key.as_str()).collect();
                tracing::info!(
                    path = %file.display(),
                    ?keys,
                    "#12077: removed the sandbox keys tm 1.14.14/1.14.15 wrote into tm's user-tier settings"
                );
            }
            if !kept.is_empty() {
                tracing::warn!(
                    path = %file.display(),
                    ?kept,
                    "#12077: kept these user-tier sandbox values because they differ from what tm wrote"
                );
            }
        }
        Ok(UserCleanupOutcome::SymlinkedSettings(path)) => tracing::warn!(
            path = %path.display(),
            "#12077: tm's user-tier settings file is a symlink; the sandbox key cleanup is skipped so the link is kept"
        ),
        Ok(_) => {}
        Err(err) => tracing::warn!(
            path = %file.display(),
            error = %err,
            "#12077: user-tier sandbox key cleanup failed; the launch goes ahead and a later launch retries"
        ),
    }
}
