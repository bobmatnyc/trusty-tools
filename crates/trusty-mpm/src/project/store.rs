//! On-disk JSON persistence for project registry entries.
//!
//! Why: the project registry must survive daemon restarts without losing track
//! of which projects an operator has registered. A simple JSON file at a
//! well-known path gives crash recovery without requiring a database dependency,
//! mirroring the session-store pattern.
//! What: [`ProjectStore`] loads and saves a map of [`Project`]s from/to
//! `~/.trusty-mpm/projects.json`, using atomic temp-file + rename writes and
//! reload-on-read freshness detection.
//! Test: `store_load_save_round_trip`, `store_upsert_idempotent`,
//! `store_list_and_get`, `store_reload_picks_up_external_write`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::fs;
use tracing::debug;

use super::record::Project;

/// Errors that can arise from project store I/O or serialization.
///
/// Why: callers need structured error information to distinguish transient I/O
/// failures from data-corruption problems so they can surface actionable messages.
/// What: one variant per failure mode: I/O, serialization, or not-found.
/// Test: exercised indirectly through `ProjectStore` method tests.
#[derive(Debug, Error)]
pub enum ProjectStoreError {
    /// An I/O operation on the backing file failed.
    #[error("project store I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// JSON serialization or deserialization failed.
    #[error("project store serialization error: {0}")]
    Serialize(String),

    /// The requested project name was not found in the store.
    #[error("project not found: {0}")]
    NotFound(String),
}

/// In-memory representation of the serialized store file.
///
/// Why: serde needs a stable top-level shape for the JSON file; wrapping the
/// map in a versioned struct makes future schema migrations possible.
/// What: a flat map from project name to [`Project`].
/// Test: round-tripped implicitly by `ProjectStore` tests.
#[derive(Debug, Default, Serialize, Deserialize)]
struct StoredData {
    /// All projects, keyed by project name.
    projects: HashMap<String, Project>,
}

/// A cheap freshness fingerprint for the backing file.
///
/// Why: the cross-process reload check keys off "did the file change since we
/// last touched it". An mtime alone is insufficient on coarse filesystems — two
/// writes in the same second would compare equal. Pairing mtime with byte length
/// catches same-second writes that changed the file size.
/// What: the file's last-modified time and length in bytes.
/// Test: `store_reload_picks_up_external_write`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct FileSig {
    mtime: Option<SystemTime>,
    len: u64,
}

/// Async, file-backed store for [`Project`] records.
///
/// Why: the project registry must persist across daemon restarts, and both the
/// daemon and CLI may read the same `projects.json`. Atomic writes (temp +
/// rename) prevent torn reads, and reload-on-read freshness detection keeps the
/// in-memory view current when another process writes.
/// What: holds the in-memory map, the path to `projects.json`, and the file
/// signature observed at the last load/save. All mutations call `save()`
/// immediately; reads call `reload_if_changed()` first.
/// Test: `store_load_save_round_trip`, `store_upsert_idempotent`,
/// `store_reload_picks_up_external_write`.
#[derive(Debug)]
pub struct ProjectStore {
    data: StoredData,
    path: PathBuf,
    last_sig: Option<FileSig>,
}

impl ProjectStore {
    /// Load (or create) the project store at the given data directory.
    ///
    /// Why: on startup the registry needs to restore state from the previous
    /// run; if no file exists a fresh empty store is returned so the daemon can
    /// start cleanly.
    /// What: reads `<data_dir>/projects.json` if it exists; returns an empty
    /// store if the file is absent; propagates I/O and JSON errors.
    /// Test: `store_load_save_round_trip`.
    pub async fn load(data_dir: &Path) -> Result<Self, ProjectStoreError> {
        let path = data_dir.join("projects.json");
        let (data, last_sig) = Self::read_file(&path).await?;
        Ok(Self {
            data,
            path,
            last_sig,
        })
    }

    /// Stat `path` and return its freshness fingerprint, or `None` if absent.
    ///
    /// Why: both the reload check and `save` need the same (mtime, len) signature;
    /// centralising it keeps "what counts as the file's identity" in one place.
    /// What: on `metadata` success returns `Some(FileSig { mtime, len })`; on any
    /// stat error returns `None`.
    /// Test: exercised via `store_reload_*` tests.
    async fn sig_of(path: &Path) -> Option<FileSig> {
        let meta = fs::metadata(path).await.ok()?;
        Some(FileSig {
            mtime: meta.modified().ok(),
            len: meta.len(),
        })
    }

    /// Read and parse the backing file, returning the data and its fingerprint.
    ///
    /// Why: both `load` and `reload_if_changed` need the same "read or treat
    /// absence as empty" logic; factoring it out keeps the two callers in lockstep.
    /// Critically, only `NotFound` is treated as "file absent" — all other I/O
    /// errors (permission denied, directory in place of file, I/O fault) are
    /// propagated so the caller never silently overwrites `projects.json` with an
    /// empty store on a transient error.
    /// What: if `path` exists reads and JSON-parses it, returning `(data, Some(sig))`
    /// with the signature captured AFTER the read; if absent (`NotFound`) returns
    /// `(empty, None)`; propagates all other I/O errors as `ProjectStoreError::Io`.
    /// Test: `store_load_save_round_trip`, `store_reload_picks_up_external_write`,
    /// `store_not_found_starts_fresh`, `store_other_io_error_propagates`.
    async fn read_file(path: &Path) -> Result<(StoredData, Option<FileSig>), ProjectStoreError> {
        match fs::read_to_string(path).await {
            Ok(raw) => {
                let data = serde_json::from_str::<StoredData>(&raw)
                    .map_err(|e| ProjectStoreError::Serialize(e.to_string()))?;
                let sig = Self::sig_of(path).await.unwrap_or_default();
                Ok((data, Some(sig)))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                debug!(path = %path.display(), "no project store file found; starting fresh");
                Ok((StoredData::default(), None))
            }
            Err(e) => Err(ProjectStoreError::Io(e)),
        }
    }

    /// Reload the in-memory map from disk if the backing file changed.
    ///
    /// Why: another process (e.g. the CLI) may have written `projects.json` since
    /// this store last touched it. Reading first reconciles the in-memory view with
    /// the authoritative on-disk state.
    /// What: stats the file; if its fingerprint differs from `last_sig`, re-reads
    /// and replaces `data` and `last_sig`. A matching fingerprint is a fast no-op.
    /// Test: `store_reload_picks_up_external_write`,
    /// `store_reload_noop_when_unchanged`.
    pub async fn reload_if_changed(&mut self) -> Result<(), ProjectStoreError> {
        let current = Self::sig_of(&self.path).await;
        let unchanged = matches!((current, self.last_sig), (Some(a), Some(b)) if a == b);
        if unchanged {
            return Ok(());
        }
        let (data, sig) = Self::read_file(&self.path).await?;
        self.data = data;
        self.last_sig = sig;
        debug!(path = %self.path.display(), "project store reloaded after external change");
        Ok(())
    }

    /// Persist the current in-memory state to disk atomically.
    ///
    /// Why: every mutating operation must flush to disk for crash safety. Writing
    /// a temp file and renaming atomically prevents concurrent readers from seeing
    /// a torn (partially-written) file.
    /// What: serializes `data` to JSON, writes a sibling `.tmp` file, then renames
    /// it over the real path; records the resulting signature.
    /// Test: verified by every mutating store test.
    pub async fn save(&mut self) -> Result<(), ProjectStoreError> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).await?;
        }
        let json = serde_json::to_string_pretty(&self.data)
            .map_err(|e| ProjectStoreError::Serialize(e.to_string()))?;
        let tmp = self.path.with_extension("json.tmp");
        fs::write(&tmp, json).await?;
        fs::rename(&tmp, &self.path).await?;
        self.last_sig = Self::sig_of(&self.path).await;
        debug!(path = %self.path.display(), "project store saved");
        Ok(())
    }

    /// Insert or update a project record, keyed by `project.name`.
    ///
    /// Why: registration is idempotent — re-registering a project with the same
    /// name updates its fields rather than creating a duplicate. Reloading first
    /// ensures concurrent writes from another process are not lost.
    /// What: reloads from disk if changed, inserts or replaces the record keyed by
    /// `name`, then saves.
    /// Test: `store_upsert_idempotent`.
    pub async fn upsert(&mut self, project: Project) -> Result<(), ProjectStoreError> {
        self.reload_if_changed().await?;
        let key = project.name.clone();
        self.data.projects.insert(key, project);
        self.save().await
    }

    /// Look up a project by name, reloading from disk first if changed.
    ///
    /// Why: the caller must see out-of-process writes; reloading before answering
    /// ensures freshness.
    /// What: calls `reload_if_changed`, then returns a clone or `NotFound`.
    /// Test: `store_list_and_get`.
    pub async fn get(&mut self, name: &str) -> Result<Project, ProjectStoreError> {
        self.reload_if_changed().await?;
        self.data
            .projects
            .get(name)
            .cloned()
            .ok_or_else(|| ProjectStoreError::NotFound(name.to_string()))
    }

    /// Return all stored project records, reloading from disk first if changed.
    ///
    /// Why: `project_list` and registry-seeding operations both need the full set
    /// and must reflect any write made by another process.
    /// What: calls `reload_if_changed`, then clones and collects all values.
    /// Test: `store_list_and_get`.
    pub async fn all(&mut self) -> Result<Vec<Project>, ProjectStoreError> {
        self.reload_if_changed().await?;
        Ok(self.data.projects.values().cloned().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_project(name: &str) -> Project {
        Project {
            name: name.to_string(),
            repo_url: format!("https://github.com/owner/{name}"),
            default_branch: "main".to_string(),
            stack_hint: None,
            tags: vec![],
            description: None,
        }
    }

    #[tokio::test]
    async fn store_load_save_round_trip() {
        let dir = TempDir::new().expect("tempdir");
        let mut store = ProjectStore::load(dir.path()).await.expect("load empty");
        assert!(store.all().await.expect("all").is_empty());

        store.upsert(make_project("alpha")).await.expect("upsert");

        let mut store2 = ProjectStore::load(dir.path()).await.expect("reload");
        let p = store2.get("alpha").await.expect("get after reload");
        assert_eq!(p.name, "alpha");
    }

    #[tokio::test]
    async fn store_upsert_idempotent() {
        let dir = TempDir::new().expect("tempdir");
        let mut store = ProjectStore::load(dir.path()).await.expect("load");

        let p1 = make_project("beta");
        store.upsert(p1).await.expect("first upsert");

        // Update the description — must replace, not duplicate.
        let p2 = Project {
            description: Some("updated".into()),
            ..make_project("beta")
        };
        store.upsert(p2).await.expect("second upsert");

        let all = store.all().await.expect("all");
        assert_eq!(all.len(), 1, "idempotent upsert must not duplicate");
        assert_eq!(all[0].description.as_deref(), Some("updated"));
    }

    #[tokio::test]
    async fn store_list_and_get() {
        let dir = TempDir::new().expect("tempdir");
        let mut store = ProjectStore::load(dir.path()).await.expect("load");

        store
            .upsert(make_project("gamma"))
            .await
            .expect("upsert gamma");
        store
            .upsert(make_project("delta"))
            .await
            .expect("upsert delta");

        let all = store.all().await.expect("all");
        assert_eq!(all.len(), 2);

        let p = store.get("gamma").await.expect("get gamma");
        assert_eq!(p.name, "gamma");

        let err = store.get("missing").await;
        assert!(
            matches!(err, Err(ProjectStoreError::NotFound(_))),
            "expected NotFound"
        );
    }

    #[tokio::test]
    async fn store_reload_picks_up_external_write() {
        let dir = TempDir::new().expect("tempdir");

        let mut store_a = ProjectStore::load(dir.path()).await.expect("load A");
        store_a
            .upsert(make_project("epsilon"))
            .await
            .expect("seed A");

        // Simulate a second process writing a new project.
        let mut store_b = ProjectStore::load(dir.path()).await.expect("load B");
        store_b.upsert(make_project("zeta")).await.expect("write B");

        // Store A must pick up the external write on its next read.
        let all = store_a.all().await.expect("all A after external write");
        let names: Vec<&str> = all.iter().map(|p| p.name.as_str()).collect();
        assert!(
            names.contains(&"zeta"),
            "store A must reload zeta: {names:?}"
        );
    }

    #[tokio::test]
    async fn store_reload_noop_when_unchanged() {
        let dir = TempDir::new().expect("tempdir");
        let mut store = ProjectStore::load(dir.path()).await.expect("load");
        store.upsert(make_project("eta")).await.expect("upsert");

        // No external write — reload must be a no-op that preserves data.
        store.reload_if_changed().await.expect("reload no-op");
        let p = store.get("eta").await.expect("get");
        assert_eq!(p.name, "eta");
    }

    #[tokio::test]
    async fn json_round_trip_preserves_all_fields() {
        let dir = TempDir::new().expect("tempdir");
        let mut store = ProjectStore::load(dir.path()).await.expect("load");

        let full = Project {
            name: "full-project".into(),
            repo_url: "https://github.com/owner/full-project.git".into(),
            default_branch: "develop".into(),
            stack_hint: Some("typescript".into()),
            tags: vec!["frontend".into(), "oss".into()],
            description: Some("a fully-populated project".into()),
        };
        store.upsert(full.clone()).await.expect("upsert full");

        let mut store2 = ProjectStore::load(dir.path()).await.expect("reload");
        let back = store2.get("full-project").await.expect("get");
        assert_eq!(back, full);
    }

    /// Verify that a NotFound path still starts with an empty store (happy-path
    /// absence handling) and that a subsequent upsert+reload round-trips correctly.
    ///
    /// Why: the bug fix narrows the "start fresh" arm to NotFound only; this test
    /// confirms that the intended absent-file path still works as before.
    /// What: loads from a directory that contains no `projects.json`, asserts the
    /// store is empty, then writes and reloads to confirm persistence works.
    /// Test: this IS the test.
    #[tokio::test]
    async fn store_not_found_starts_fresh() {
        let dir = TempDir::new().expect("tempdir");
        // No projects.json exists yet — load must return an empty store.
        let mut store = ProjectStore::load(dir.path())
            .await
            .expect("not-found should start fresh");
        assert!(
            store.all().await.expect("all").is_empty(),
            "fresh store must be empty"
        );

        // Confirm we can round-trip through a save after starting fresh.
        store
            .upsert(make_project("theta"))
            .await
            .expect("upsert after fresh load");
        let mut store2 = ProjectStore::load(dir.path()).await.expect("reload");
        assert_eq!(store2.get("theta").await.expect("get theta").name, "theta");
    }

    /// Verify that a non-NotFound I/O error (e.g. a directory occupying the file
    /// path) is propagated rather than silently treated as absent.
    ///
    /// Why: the data-loss bug caused any I/O error to be swallowed, so the next
    /// `save()` would overwrite projects.json with an empty store. This test
    /// exercises the fix by pointing `read_file` at a directory, which produces
    /// `IsADirectory` (or equivalent) — not `NotFound`.
    /// What: creates a directory at `projects.json`'s expected path, then calls
    /// `ProjectStore::load`; asserts the result is an `Io` error, not `Ok`.
    /// Test: this IS the test.
    #[tokio::test]
    async fn store_other_io_error_propagates() {
        let dir = TempDir::new().expect("tempdir");
        // Plant a directory where projects.json would live so read_to_string
        // returns an OS error that is NOT NotFound.
        let blocking_dir = dir.path().join("projects.json");
        tokio::fs::create_dir_all(&blocking_dir)
            .await
            .expect("create blocking dir");

        let result = ProjectStore::load(dir.path()).await;
        assert!(
            matches!(result, Err(ProjectStoreError::Io(_))),
            "expected Io error when path is a directory, got: {result:?}"
        );
    }
}
