//! On-disk JSON persistence for project registry entries.
//!
//! Why: the project registry must survive daemon restarts without losing track
//! of which projects an operator has registered. A simple JSON file at a
//! well-known path gives crash recovery without requiring a database dependency,
//! mirroring the session-store pattern.
//! What: [`ProjectStore`] loads and saves a map of [`Project`]s from/to
//! `~/.trusty-mpm/projects.json`. Every mutation runs through
//! [`ProjectStore::mutate`], which delegates the whole read-modify-write cycle
//! to [`trusty_common::json_rmw::update`] so concurrent `tm` processes serialise
//! on a file lock and publish atomically. Reads stay lock-free and use
//! reload-on-read freshness detection.
//! Test: `cargo test -p trusty-mpm -- project::store::tests` (unit) plus the
//! cross-process `projects_json_multiprocess_upsert_no_lost_updates` and
//! `projects_json_survives_killed_writer`.
//!
//! # Concurrency
//!
//! `projects.json` is written by several independent processes — the daemon, the
//! `tm project …` CLI, and MCP tool calls. The mutation path used to be
//! `reload_if_changed()` → mutate in memory → write the whole file back, with no
//! synchronisation of any kind: `ProjectRegistry`'s `tokio::sync::RwLock` only
//! serialises tasks WITHIN one process. Two processes interleaving
//! read/read/write/write dropped one of the two entries while both callers saw
//! `Ok`, and because every writer shared one fixed `projects.json.tmp` scratch
//! path, overlapping writes could also publish a mangled document that no longer
//! parsed. [`ProjectStore::mutate`] closes both holes.
//!
//! Lock discipline for anyone extending this module:
//!
//! - **One acquisition per operation.** Mutations must funnel through
//!   [`ProjectStore::mutate`]; it takes the file lock exactly once. Never call a
//!   mutating store method from inside a `mutate` closure — the nested
//!   acquisition uses a fresh descriptor and self-deadlocks.
//! - **Readers take no file lock.** [`ProjectStore::get`] / [`ProjectStore::all`]
//!   read without locking, which is safe because every publish is an atomic
//!   rename: a reader sees a complete old or complete new document, never a torn
//!   one. This also means a reader can never deadlock against a writer.
//! - **Lock ordering.** Where `ProjectRegistry` holds its async `RwLock`, that
//!   lock is always taken BEFORE the file lock and the file lock is released
//!   before the guard drops. No path acquires them in the other order.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::fs;
use tracing::debug;
use trusty_common::json_rmw::{self, JsonRmwError};

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

    /// The cross-process write lock on `projects.json` could not be acquired.
    ///
    /// Why: a caller that sees this must NOT retry by writing unlocked — that is
    /// precisely the lost-update bug. It is surfaced as its own variant so the
    /// remedy (stale sidecar, permissions) is distinguishable from a data fault.
    #[error("project store lock error: {0}")]
    Lock(String),
}

/// Map the shared locked-RMW failure modes onto the store's own error type.
///
/// Why: `json_rmw::update` is generic over the caller's error so `ProjectStore`
/// keeps ONE return type; this is the conversion that buys that. Preserving the
/// underlying `io::Error` (rather than stringifying it) keeps `ErrorKind` intact
/// for callers that branch on it.
/// What: `Io` keeps its source error, `Lock` becomes [`ProjectStoreError::Lock`],
/// and `Serialize` becomes [`ProjectStoreError::Serialize`].
/// Test: `store_lock_failure_is_not_fail_open`, `store_other_io_error_propagates`.
impl From<JsonRmwError> for ProjectStoreError {
    fn from(e: JsonRmwError) -> Self {
        match e {
            JsonRmwError::Io { source, .. } => Self::Io(source),
            JsonRmwError::Serialize { message, .. } => Self::Serialize(message),
            lock @ JsonRmwError::Lock { .. } => Self::Lock(lock.to_string()),
        }
    }
}

/// In-memory representation of the serialized store file.
///
/// Why: serde needs a stable top-level shape for the JSON file; wrapping the
/// map in a versioned struct makes future schema migrations possible.
/// What: a flat map from project name to [`Project`].
/// Test: round-tripped implicitly by `ProjectStore` tests.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
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
/// Why: the project registry must persist across daemon restarts, and the
/// daemon, the CLI and MCP tool calls all read and WRITE the same
/// `projects.json` from different processes. See the module-level `Concurrency`
/// section for the lock discipline that makes those writes safe.
/// What: holds the in-memory map, the path to `projects.json`, and the file
/// signature observed at the last load/write. All mutations go through
/// [`ProjectStore::mutate`], which re-reads under an exclusive cross-process
/// lock and publishes atomically; reads call `reload_if_changed()` first and
/// take no lock.
/// Test: `store_load_save_round_trip`, `store_upsert_idempotent`,
/// `store_reload_picks_up_external_write`,
/// `store_concurrent_tasks_do_not_lose_writes`.
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

    /// Mutate the persisted project map under the cross-process write lock.
    ///
    /// Why: this is the ONLY sanctioned write path, and the fix for the
    /// `projects.json` lost-update race. The previous shape — `reload_if_changed`
    /// then mutate in memory then write the whole file — left an unguarded window
    /// between the read and the write in which another `tm` process could publish
    /// its own version, which this process then clobbered. Nothing reported an
    /// error: both writers returned `Ok` and one project simply vanished.
    /// Centralising the cycle here means every mutation (registration, PATCH,
    /// config seeding) inherits the guarantee instead of re-deriving it, and the
    /// worktree registry of epic #4207 gets a primitive to build on.
    /// What: runs [`trusty_common::json_rmw::update`] on a blocking-safe thread
    /// (the advisory lock is a blocking syscall and must never park a runtime
    /// worker). Under the lock the map is re-read from disk — a caller's
    /// in-memory copy is never trusted — `f` is applied, and the result is
    /// published by atomic rename. `f` returning `Err` aborts the write with the
    /// file unchanged, so a rejected mutation cannot advance state. The refreshed
    /// map is then adopted as this store's in-memory view. A failure at ANY stage
    /// propagates; there is no path that proceeds unlocked or writes partially.
    ///
    /// Not reentrant: `f` must not call back into a mutating store method.
    /// Test: `store_upsert_idempotent`, `store_lock_failure_is_not_fail_open`,
    /// `store_concurrent_tasks_do_not_lose_writes`, and the cross-process
    /// `projects_json_multiprocess_upsert_no_lost_updates`.
    pub async fn mutate<R, F>(&mut self, f: F) -> Result<R, ProjectStoreError>
    where
        F: FnOnce(&mut HashMap<String, Project>) -> Result<R, ProjectStoreError> + Send + 'static,
        R: Send + 'static,
    {
        let path = self.path.clone();
        let joined = tokio::task::spawn_blocking(move || {
            json_rmw::update::<StoredData, _, ProjectStoreError, _>(&path, move |data| {
                let result = f(&mut data.projects)?;
                // Hand the freshly-published map back so the caller's in-memory
                // view is exact without a second read.
                Ok((result, data.clone()))
            })
        })
        .await;

        // A panicked or cancelled blocking task is a failure, never a silent
        // success — the write may not have happened.
        let (result, snapshot) = match joined {
            Ok(inner) => inner?,
            Err(e) => {
                return Err(ProjectStoreError::Io(std::io::Error::other(format!(
                    "project store update task failed: {e}"
                ))));
            }
        };

        self.data = snapshot;
        self.last_sig = Self::sig_of(&self.path).await;
        debug!(path = %self.path.display(), "project store saved under write lock");
        Ok(result)
    }

    /// Insert or update a project record, keyed by `project.name`.
    ///
    /// Why: registration is idempotent — re-registering a project with the same
    /// name updates its fields rather than creating a duplicate.
    /// What: inserts or replaces the record keyed by `name` inside a single
    /// [`Self::mutate`] critical section, so a concurrent writer in another
    /// process can neither be lost nor lose this one.
    /// Test: `store_upsert_idempotent`.
    pub async fn upsert(&mut self, project: Project) -> Result<(), ProjectStoreError> {
        self.mutate(move |projects| {
            projects.insert(project.name.clone(), project);
            Ok(())
        })
        .await
    }

    /// Read-modify-persist a single named project under one held lock.
    ///
    /// Why: `PATCH /api/v1/projects/{name}` reads a record, edits some fields and
    /// writes the whole record back. Splitting that across a `get` and a later
    /// `upsert` reintroduces the lost-update race across processes even though
    /// each half is individually safe — two PATCHes editing DIFFERENT fields
    /// would each persist a record built from the same stale snapshot.
    /// What: fetches `name` from the freshly-read map inside [`Self::mutate`]
    /// (propagating [`ProjectStoreError::NotFound`] without writing), applies
    /// `f`, stores the result and returns it — all before the lock is released.
    /// Test: `update_with_serializes_concurrent_field_edits`,
    /// `update_with_unknown_project_errors`.
    pub async fn update_with<F>(&mut self, name: &str, f: F) -> Result<Project, ProjectStoreError>
    where
        F: FnOnce(Project) -> Project + Send + 'static,
    {
        let name = name.to_string();
        self.mutate(move |projects| {
            let current = projects
                .get(&name)
                .cloned()
                .ok_or(ProjectStoreError::NotFound(name))?;
            let updated = f(current);
            projects.insert(updated.name.clone(), updated.clone());
            Ok(updated)
        })
        .await
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
#[path = "store_tests.rs"]
mod tests;
