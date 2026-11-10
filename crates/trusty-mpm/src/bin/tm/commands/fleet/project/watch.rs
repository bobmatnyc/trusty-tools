//! The watch list file, `local/watch.json` (#10895, plan D1).
//!
//! Why: the Architect and the owner change the watch set through one command
//! (plan O2), and two commands may run at once (AC3), so every write is a
//! locked read-modify-write that publishes by rename.
//! What: [`WatchFile`] is `{"version": 1, "projects": [{"key": ...}]}`.
//! [`read`] parses it without a lock (writes are atomic renames); [`update`]
//! is `json_rmw::update` with the version check; [`ensure`] creates the file
//! once from the `CLAUDE.md` import (plan D9).
//! Test: `concurrent_adds_for_two_projects_keep_both_entries`,
//! `a_future_version_is_refused_and_left_unchanged`,
//! `migration_imports_registered_lines_and_skips_the_rest`.

use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};

use super::migrate;
use super::registry::Registry;

/// The list's path below the Architect directory.
pub(crate) const WATCH: &str = "local/watch.json";

/// The one format version this binary reads and writes.
pub(crate) const VERSION: u32 = 1;

/// The whole list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct WatchFile {
    /// Format version; anything but [`VERSION`] is refused.
    pub(crate) version: u32,
    /// One entry per watched project, in the order added.
    pub(crate) projects: Vec<Entry>,
}

impl Default for WatchFile {
    fn default() -> Self {
        Self {
            version: VERSION,
            projects: Vec::new(),
        }
    }
}

/// One watched project. Path and alias are read from the registry, never
/// stored (plan D2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Entry {
    /// The project's registry key.
    pub(crate) key: String,
}

impl WatchFile {
    /// Whether `key` is on the list.
    pub(crate) fn contains(&self, key: &str) -> bool {
        self.projects.iter().any(|e| e.key == key)
    }

    /// Refuse a version this binary does not know; it would drop its fields.
    fn check_version(&self, path: &Path) -> anyhow::Result<()> {
        if self.version != VERSION {
            bail!(
                "{} has version {}; this tm reads version {VERSION} only — upgrade tm",
                path.display(),
                self.version
            );
        }
        Ok(())
    }
}

/// `<dir>/local/watch.json`.
pub(crate) fn path(dir: &Path) -> PathBuf {
    dir.join(WATCH)
}

/// The list, or `None` when the file is absent.
///
/// An unreadable or malformed file is an error: a reader must never take it
/// for an empty list.
pub(crate) fn read(path: &Path) -> anyhow::Result<Option<WatchFile>> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("cannot read {}", path.display())),
    };
    let file: WatchFile = serde_json::from_slice(&bytes)
        .with_context(|| format!("{} is not a valid watch list", path.display()))?;
    file.check_version(path)?;
    Ok(Some(file))
}

/// Lock the list, re-read it, apply `f`, and publish it only when `f` is `Ok`.
///
/// Why: AC3 and AC4 — concurrent writers keep each other's entries, and a
/// failed step leaves the file byte-identical.
/// What: `trusty_common::json_rmw::update` (sidecar lock `watch.json.lock`,
/// re-read, atomic rename). A wrong version fails inside the lock, before
/// `f` runs.
/// Test: `concurrent_adds_for_two_projects_keep_both_entries`,
/// `a_future_version_is_refused_and_left_unchanged`.
pub(crate) fn update<R>(
    path: &Path,
    f: impl FnOnce(&mut WatchFile) -> anyhow::Result<R>,
) -> anyhow::Result<R> {
    trusty_common::json_rmw::update(path, |file: &mut WatchFile| {
        file.check_version(path)?;
        f(file)
    })
}

/// Create the list from the `CLAUDE.md` import when it is absent (plan D9).
///
/// Why: an upgraded Architect keeps supervising the projects of its
/// hand-written list with no manual step (plan O3), and the import must run
/// once only: an emptied list stays empty (AC7).
/// What: returns no notes when the list exists. Otherwise, under the list's
/// lock, re-checks absence, builds [`migrate::plan`], and writes the list
/// with the imported keys — also when none matched. The returned notes name
/// every imported and every skipped line. `CLAUDE.md` is only read.
/// Test: `migration_imports_registered_lines_and_skips_the_rest`,
/// `migration_runs_once_and_an_emptied_list_stays_empty`.
pub(crate) fn ensure(dir: &Path, registry: &Registry) -> anyhow::Result<Vec<String>> {
    let path = path(dir);
    if exists(&path)? {
        return Ok(Vec::new());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("cannot create {}", parent.display()))?;
    }
    trusty_common::file_lock::with_exclusive_lock(&path, || -> anyhow::Result<Vec<String>> {
        // #10895: another command may have imported while this one waited.
        if exists(&path)? {
            return Ok(Vec::new());
        }
        let plan = migrate::plan(dir, registry)?;
        let file = WatchFile {
            version: VERSION,
            projects: plan
                .keys
                .iter()
                .map(|key| Entry { key: key.clone() })
                .collect(),
        };
        let bytes = serde_json::to_vec_pretty(&file)?;
        trusty_common::atomic_file::write_atomic(&path, &bytes)
            .with_context(|| format!("cannot write {}", path.display()))?;
        Ok(plan.notes)
    })
    .with_context(|| format!("cannot lock {}", path.display()))?
}

/// `path` exists; an error other than absence is an error, not "absent".
fn exists(path: &Path) -> anyhow::Result<bool> {
    path.try_exists()
        .with_context(|| format!("cannot read {}", path.display()))
}
