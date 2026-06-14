//! [`ConfigCheckpointer`] — snapshot and restore Claude Code config files.
//!
//! Why: every mutating config operation must be reversible; a checkpoint
//! captures the full pre-change state so a single restore call undoes it.
//! What: `create` writes a [`ConfigCheckpoint`] JSON file, `list` reads them
//! back newest-first, `restore` rewrites the config files from a checkpoint,
//! and `delete` removes one checkpoint.
//! Test: see `super::tests`.

use std::collections::HashMap;
use std::path::Path;

use crate::core::claude_config::{CheckpointPaths, ClaudeConfigPaths, ConfigCheckpoint};
use crate::core::{Error, Result};

/// The four config files a checkpoint snapshots, keyed by the relative path
/// stored inside the checkpoint.
///
/// Why: `create` and `restore` must agree on exactly which files form a
/// checkpoint and on the stable relative key each is stored under; deriving the
/// list in one place keeps them consistent.
/// What: returns `(relative-key, absolute-path)` pairs for the user- and
/// project-level `settings.json` / `settings.local.json` files.
pub(super) fn checkpoint_targets(paths: &ClaudeConfigPaths) -> [(&'static str, &Path); 4] {
    [
        ("user/settings.json", paths.user_settings.as_path()),
        (
            "user/settings.local.json",
            paths.user_local_settings.as_path(),
        ),
        ("project/settings.json", paths.project_settings.as_path()),
        (
            "project/settings.local.json",
            paths.project_local_settings.as_path(),
        ),
    ]
}

/// Snapshots and restores a project's Claude Code config files.
///
/// Why: every mutating config operation must be reversible; a checkpoint
/// captures the full pre-change state so a single restore call undoes it.
/// What: `create` writes a [`ConfigCheckpoint`] JSON file, `list` reads them
/// back newest-first, `restore` rewrites the config files from a checkpoint,
/// and `delete` removes one checkpoint.
/// Test: `apply_creates_checkpoint_before_change`, `restore_reverts_to_pre_apply_state`,
/// `checkpoint_list_newest_first`, `safe_restore_does_not_delete_new_files`.
pub struct ConfigCheckpointer;

impl ConfigCheckpointer {
    /// Snapshot every Claude Code config file for `project`.
    ///
    /// Why: callers need a restorable point-in-time copy of the config before
    /// any change.
    /// What: reads each of the four settings files (absent files are simply not
    /// recorded), writes a [`ConfigCheckpoint`] to
    /// `<project>/.trusty-mpm/checkpoints/<id>.json`, and returns the id. The id
    /// is `checkpoint-{YYYYMMDD}-{HHMMSS}-{4-char-random}` so concurrent
    /// checkpoints in the same second do not collide.
    /// Test: `apply_creates_checkpoint_before_change`.
    pub fn create(
        paths: &ClaudeConfigPaths,
        project: &Path,
        label: Option<&str>,
    ) -> Result<String> {
        let now = chrono::Utc::now();
        let id = format!(
            "checkpoint-{}-{}",
            now.format("%Y%m%d-%H%M%S"),
            random_suffix()
        );

        let mut files = HashMap::new();
        for (key, path) in checkpoint_targets(paths) {
            if let Ok(content) = std::fs::read_to_string(path) {
                files.insert(key.to_string(), content);
            }
        }

        let checkpoint = ConfigCheckpoint {
            id: id.clone(),
            created_at: now.to_rfc3339(),
            project: project.to_path_buf(),
            label: label.map(str::to_string),
            files,
        };

        let dir = CheckpointPaths::dir(project);
        std::fs::create_dir_all(&dir).map_err(Error::Io)?;
        let file = CheckpointPaths::for_id(project, &id);
        let json = serde_json::to_string_pretty(&checkpoint)
            .map_err(|e| Error::Protocol(format!("serialize checkpoint: {e}")))?;
        std::fs::write(&file, json).map_err(Error::Io)?;
        tracing::info!("created config checkpoint {id} for {}", project.display());
        Ok(id)
    }

    /// List every checkpoint for `project`, newest first.
    ///
    /// Why: the dashboard offers a restore picker; newest-first matches what an
    /// operator expects.
    /// What: reads each `*.json` file in the checkpoints directory, skipping any
    /// that fail to parse, and sorts by `created_at` descending. A missing
    /// directory yields an empty list.
    /// Test: `checkpoint_list_newest_first`.
    pub fn list(project: &Path) -> Result<Vec<ConfigCheckpoint>> {
        let dir = CheckpointPaths::dir(project);
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(_) => return Ok(Vec::new()),
        };
        let mut checkpoints: Vec<ConfigCheckpoint> = entries
            .flatten()
            .filter(|e| {
                e.path()
                    .extension()
                    .and_then(|x| x.to_str())
                    .is_some_and(|x| x.eq_ignore_ascii_case("json"))
            })
            .filter_map(|e| {
                let raw = std::fs::read_to_string(e.path()).ok()?;
                match serde_json::from_str::<ConfigCheckpoint>(&raw) {
                    Ok(cp) => Some(cp),
                    Err(err) => {
                        tracing::warn!(
                            "skipping malformed checkpoint {}: {err}",
                            e.path().display()
                        );
                        None
                    }
                }
            })
            .collect();
        // Newest first. `created_at` is RFC3339, which sorts lexically.
        checkpoints.sort_by_key(|c| std::cmp::Reverse(c.created_at.clone()));
        Ok(checkpoints)
    }

    /// Restore `project`'s config files to the state in a checkpoint.
    ///
    /// Why: the undo half of the safety model — re-apply a known-good config.
    /// What: loads the checkpoint, then for every file recorded in it rewrites
    /// the on-disk file (creating parent directories as needed). Files that were
    /// *absent* in the checkpoint are left untouched — this is a safe restore,
    /// so config files created after the checkpoint are never deleted.
    /// Test: `restore_reverts_to_pre_apply_state`, `safe_restore_does_not_delete_new_files`.
    pub fn restore(project: &Path, checkpoint_id: &str) -> Result<()> {
        let file = CheckpointPaths::for_id(project, checkpoint_id);
        let raw = std::fs::read_to_string(&file)
            .map_err(|e| Error::Protocol(format!("checkpoint `{checkpoint_id}` not found: {e}")))?;
        let checkpoint: ConfigCheckpoint = serde_json::from_str(&raw)
            .map_err(|e| Error::Protocol(format!("malformed checkpoint `{checkpoint_id}`: {e}")))?;

        let paths = crate::core::claude_config::ClaudeConfigReader::paths_for_project(project);
        for (key, path) in checkpoint_targets(&paths) {
            // Only files captured in the checkpoint are restored. A file absent
            // from `files` was absent at snapshot time and is left as-is.
            if let Some(content) = checkpoint.files.get(key) {
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent).map_err(Error::Io)?;
                }
                std::fs::write(path, content).map_err(Error::Io)?;
            }
        }
        tracing::info!(
            "restored config checkpoint {checkpoint_id} for {}",
            project.display()
        );
        Ok(())
    }

    /// Delete one checkpoint from `project`.
    ///
    /// Why: checkpoints accumulate; the operator needs to prune them.
    /// What: removes `<project>/.trusty-mpm/checkpoints/<id>.json`. A missing
    /// file is reported as a protocol error.
    /// Test: `checkpoint_delete_removes_file`.
    pub fn delete(project: &Path, checkpoint_id: &str) -> Result<()> {
        let file = CheckpointPaths::for_id(project, checkpoint_id);
        std::fs::remove_file(&file).map_err(|e| {
            Error::Protocol(format!("cannot delete checkpoint `{checkpoint_id}`: {e}"))
        })
    }
}

/// A short pseudo-random suffix for checkpoint ids.
///
/// Why: two checkpoints created in the same second must not collide; a UUID's
/// first four hex chars are random enough without pulling in an RNG crate.
/// What: returns the first four characters of a fresh v4 UUID.
/// Test: covered indirectly by `apply_creates_checkpoint_before_change`.
fn random_suffix() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..4].to_string()
}
