//! On-disk JSON persistence + boot reconciliation for workstream records
//! (DOC-48 §3).
//!
//! Why: the daemon must survive restarts without losing track of which
//! workstreams it owns, or silently changing which one is active
//! (§3.2: "A daemon restart never silently changes which workstream is
//! active"). A single flat JSON file with an atomic temp+rename write gives
//! crash-safe persistence without a database dependency — mirroring
//! `trusty_mpm::session_manager::store::SessionStore`'s proven pattern
//! (fingerprint-gated reload, atomic write, structured errors), adapted to
//! this spec's flat-array-plus-pointer shape (§3.1) instead of a keyed map.
//! What: [`StoreError`] (structured failure surface), [`ReconcileOutcome`]
//! (what boot reconciliation did), and [`WorkstreamStore`] — load/save,
//! create/get/list, the low-level `set_active` persistence primitive (NOT
//! the future `workstream.activate` RPC verb — see its docs), and
//! [`WorkstreamStore::reconcile_on_boot`] (§3.3: non-destructive, restores
//! the active pointer or clears it if the target vanished).
//! Test: `store_tests`.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::fs;
use tracing::debug;

use super::model::{Workstream, WorkstreamId};
use super::path::store_path;
use crate::binding::ProjectBinding;

/// The `version` field stamped into every store file (§3.1).
pub const STORE_VERSION: &str = "1.0";

/// Errors that can arise from store I/O or serialization.
///
/// Why: callers (the daemon boot path, future RPC handlers) need structured
/// errors they can pattern-match rather than opaque strings.
/// What: one variant per failure mode.
/// Test: `store_tests::load_returns_error_on_corrupt_file`,
/// `store_tests::get_missing_id_returns_not_found`.
#[derive(Debug, Error)]
pub enum StoreError {
    /// An I/O operation on the backing file failed.
    #[error("workstream store I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// JSON serialization or deserialization failed.
    #[error("workstream store serialization error: {0}")]
    Serialize(String),
    /// The requested workstream id was not found in the store.
    #[error("workstream not found: {0}")]
    NotFound(String),
}

/// The versioned, on-disk JSON shape (§3.1): `version`, `active_workstream_id`,
/// and a flat `workstreams` array — NOT a file-per-record or a keyed map.
#[derive(Debug, Serialize, Deserialize)]
struct StoredData {
    version: String,
    #[serde(default)]
    active_workstream_id: Option<WorkstreamId>,
    #[serde(default)]
    workstreams: Vec<Workstream>,
}

impl Default for StoredData {
    fn default() -> Self {
        Self {
            version: STORE_VERSION.to_string(),
            active_workstream_id: None,
            workstreams: Vec::new(),
        }
    }
}

/// A cheap freshness fingerprint for the backing file (mirrors
/// `SessionStore`'s `FileSig`: mtime alone is insufficient on filesystems
/// with coarse mtime resolution, so pairing it with byte length catches a
/// same-second write that changed the file size).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct FileSig {
    mtime: Option<SystemTime>,
    len: u64,
}

/// What a boot-time reconciliation pass (§3.3) observed and did.
///
/// Why: the daemon boot path needs to log/report what reconciliation found
/// without re-deriving it from before/after store snapshots.
/// What: how many workstream records were loaded, the active id restored (if
/// any), and whether a stale pointer (referencing a now-nonexistent
/// workstream) was cleared.
/// Test: `store_tests::reconcile_restores_active_pointer`,
/// `store_tests::reconcile_clears_active_when_target_missing`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReconcileOutcome {
    pub workstream_count: usize,
    pub active_restored: Option<WorkstreamId>,
    pub active_cleared: bool,
}

/// Async, file-backed store for [`Workstream`] records (DOC-48 §3).
///
/// Why: gives the daemon crash-safe persistence with restart-time
/// reconciliation, matching the exact atomic-write + fingerprint-reload
/// contract `SessionStore` already proved out for a sibling harness.
/// What: holds the in-memory [`StoredData`], the backing file path, and the
/// last-observed [`FileSig`] so a read can detect an external write (e.g. a
/// second daemon process, or a test writing the file directly) and reload
/// before answering.
///
/// **Concurrency model:** `create`/`set_active`/`reconcile_on_boot` are
/// read-modify-write cycles (reload → mutate `self.data` → `save`) with NO
/// cross-process lock — unlike
/// [`trusty_agents_common::workstreams::WorkstreamLedger`], which at least
/// serializes concurrent *threads within one process* behind an in-process
/// `Mutex`, this store doesn't even do that; `&mut self` on every mutating
/// method is the only serialization, so it is safe only for a single
/// `WorkstreamStore` handle used from one task at a time. Two *processes*
/// racing a read-modify-write on the same file can still lose an update: the
/// last writer's atomic `rename` always leaves a whole, parseable file
/// (never a torn one), but an interleaved writer can silently overwrite the
/// other's change with a stale in-memory copy. This is inert today because
/// DOC-48's architecture binds exactly one daemon process to one
/// `ProjectBinding` (§2.3), and that daemon is the store's only writer; it
/// becomes a real constraint only if a future caller opens a second
/// `WorkstreamStore` over the same file from another process — such a
/// caller must route writes through the owning daemon instead of writing
/// directly, mirroring `WorkstreamLedger`'s documented callers-must-route-
/// through-one-process caveat.
/// Test: `store_tests::store_load_save_round_trip`,
/// `store_tests::store_reload_picks_up_external_write`.
#[derive(Debug)]
pub struct WorkstreamStore {
    data: StoredData,
    path: PathBuf,
    last_sig: Option<FileSig>,
}

impl WorkstreamStore {
    /// Load (or create) the store at an explicit file path.
    ///
    /// Why: the filename is derived from the daemon's `ProjectBinding`
    /// (§3.1), not a fixed name under a directory — so this takes the full
    /// resolved path rather than a directory, unlike `SessionStore::load`.
    /// What: reads `path` if it exists; returns a fresh default store
    /// (`version` stamped, empty `workstreams`, no active pointer) if the
    /// file is absent. Propagates I/O and JSON errors — a CORRUPT file is
    /// never silently treated as empty (see [`Self::read_file`]).
    /// Test: `store_tests::store_load_save_round_trip`,
    /// `store_tests::load_returns_error_on_corrupt_file`.
    pub async fn load(path: impl Into<PathBuf>) -> Result<Self, StoreError> {
        let path = path.into();
        let (data, last_sig) = Self::read_file(&path).await?;
        Ok(Self {
            data,
            path,
            last_sig,
        })
    }

    /// Load the store for a daemon's project binding, deriving the filename
    /// per §3.1.
    ///
    /// Why: the entry point a daemon boot path (or a test standing in for
    /// one) calls — it never assembles the filename itself.
    /// What: `Self::load(store_path(data_dir, binding))`.
    /// Test: `store_tests::store_load_for_binding_derives_filename`.
    pub async fn load_for_binding(
        data_dir: &Path,
        binding: &ProjectBinding,
    ) -> Result<Self, StoreError> {
        Self::load(store_path(data_dir, binding)).await
    }

    async fn sig_of(path: &Path) -> Option<FileSig> {
        let meta = fs::metadata(path).await.ok()?;
        Some(FileSig {
            mtime: meta.modified().ok(),
            len: meta.len(),
        })
    }

    /// Read and parse the backing file, returning the data and its signature.
    ///
    /// Why: a corrupt (malformed/truncated) file must surface as a loud
    /// error, not silently reset to an empty store — that would look like
    /// every workstream the user had was deleted (violates AC-6.1's
    /// non-destructive spirit at the read layer too).
    /// What: file absent -> `(default, None)`; file present and parseable ->
    /// `(data, Some(sig))`; file present and unparseable -> propagates
    /// `StoreError::Serialize`. The signature is captured AFTER the read
    /// (closes the same TOCTOU window `SessionStore` documents).
    /// Test: `store_tests::load_returns_error_on_corrupt_file`.
    async fn read_file(path: &Path) -> Result<(StoredData, Option<FileSig>), StoreError> {
        match fs::read_to_string(path).await {
            Ok(raw) => {
                let data = serde_json::from_str::<StoredData>(&raw)
                    .map_err(|e| StoreError::Serialize(e.to_string()))?;
                let sig = Self::sig_of(path).await.unwrap_or_default();
                Ok((data, Some(sig)))
            }
            Err(_) => {
                debug!(path = %path.display(), "no workstream store file found; starting fresh");
                Ok((StoredData::default(), None))
            }
        }
    }

    /// Reload the in-memory state from disk if the backing file changed.
    ///
    /// Why: mirrors `SessionStore::reload_if_changed` — another process (or,
    /// in tests, a second store handle) may have written the shared file
    /// since this handle last touched it.
    /// What: cheap no-op when the (mtime, len) fingerprint is unchanged;
    /// otherwise re-reads and replaces the in-memory data + signature.
    /// Test: `store_tests::store_reload_picks_up_external_write`,
    /// `store_tests::store_reload_noop_when_unchanged`.
    pub async fn reload_if_changed(&mut self) -> Result<(), StoreError> {
        let current = Self::sig_of(&self.path).await;
        let unchanged = matches!((current, self.last_sig), (Some(a), Some(b)) if a == b);
        if unchanged {
            return Ok(());
        }
        let (data, sig) = Self::read_file(&self.path).await?;
        self.data = data;
        self.last_sig = sig;
        debug!(path = %self.path.display(), "workstream store reloaded after external change");
        Ok(())
    }

    /// Persist the current in-memory state to disk atomically.
    ///
    /// Why: §3.1 requires "no partial writes even if the daemon crashes
    /// mid-write" — a plain `fs::write` truncates then rewrites, exposing a
    /// half-written file to a concurrent reader.
    /// What: serializes to pretty JSON, writes a sibling `.json.tmp`, then
    /// renames it over the real path (atomic swap on POSIX); records the
    /// resulting fingerprint so a subsequent `reload_if_changed` treats this
    /// write as "unchanged".
    /// Test: exercised by every mutating test; `store_tests::store_load_save_round_trip`
    /// verifies the round-trip.
    pub async fn save(&mut self) -> Result<(), StoreError> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).await?;
        }
        let json = serde_json::to_string_pretty(&self.data)
            .map_err(|e| StoreError::Serialize(e.to_string()))?;
        let tmp = self.path.with_extension("json.tmp");
        fs::write(&tmp, json).await?;
        fs::rename(&tmp, &self.path).await?;
        self.last_sig = Self::sig_of(&self.path).await;
        debug!(path = %self.path.display(), "workstream store saved");
        Ok(())
    }

    /// Create a new workstream and persist it.
    ///
    /// Why: the one creation path this ticket owns (the `workstream.create`
    /// RPC verb itself is #3295 scope; this is the persistence primitive it
    /// will call).
    /// What: reloads first (so a concurrent write is not clobbered),
    /// appends a fresh [`Workstream::new`], saves, and returns its id.
    /// Test: `store_tests::create_persists_and_is_gettable`.
    pub async fn create(&mut self, name: impl Into<String>) -> Result<WorkstreamId, StoreError> {
        self.reload_if_changed().await?;
        let ws = Workstream::new(name);
        let id = ws.id;
        self.data.workstreams.push(ws);
        self.save().await?;
        Ok(id)
    }

    /// Look up a workstream by id, reloading first.
    ///
    /// Test: `store_tests::create_persists_and_is_gettable`,
    /// `store_tests::get_missing_id_returns_not_found`.
    pub async fn get(&mut self, id: WorkstreamId) -> Result<Workstream, StoreError> {
        self.reload_if_changed().await?;
        self.data
            .workstreams
            .iter()
            .find(|w| w.id == id)
            .cloned()
            .ok_or_else(|| StoreError::NotFound(id.to_string()))
    }

    /// Return every stored workstream record, reloading first.
    ///
    /// Why: state is computed, not stored — callers pair each record with
    /// [`Workstream::state`] and [`Self::active_workstream_id`] themselves.
    /// Test: `store_tests::list_returns_all_records`.
    pub async fn list(&mut self) -> Result<Vec<Workstream>, StoreError> {
        self.reload_if_changed().await?;
        Ok(self.data.workstreams.clone())
    }

    /// Return the persisted active-workstream pointer, reloading first.
    ///
    /// Test: `store_tests::set_active_persists_pointer`.
    pub async fn active_workstream_id(&mut self) -> Result<Option<WorkstreamId>, StoreError> {
        self.reload_if_changed().await?;
        Ok(self.data.active_workstream_id)
    }

    /// Low-level persistence primitive: set (or clear) the active-workstream
    /// pointer and persist.
    ///
    /// Why: this is NOT the future `workstream.activate{id, force}` RPC verb
    /// (#3294) — that verb layers conflict detection (`ActiveConflict` when
    /// another workstream is already active) and the prior-workstream
    /// deactivation side effect on top. This method is the mechanical
    /// "write the pointer" primitive that verb — and this ticket's own boot
    /// reconciliation — build on.
    /// What: validates that `Some(id)` refers to an existing record (a
    /// dangling pointer must never be written deliberately — only
    /// reconciliation clears an already-dangling one), then persists.
    /// Test: `store_tests::set_active_persists_pointer`,
    /// `store_tests::set_active_rejects_unknown_id`.
    pub async fn set_active(&mut self, id: Option<WorkstreamId>) -> Result<(), StoreError> {
        self.reload_if_changed().await?;
        if let Some(id) = id
            && !self.data.workstreams.iter().any(|w| w.id == id)
        {
            return Err(StoreError::NotFound(id.to_string()));
        }
        self.data.active_workstream_id = id;
        self.save().await
    }

    /// Boot-time reconciliation (DOC-48 §3.3, AC-1.4, AC-6.1, AC-6.2).
    ///
    /// Why: a daemon restart must never silently change which workstream is
    /// active (§3.2), and must never delete a workstream record (§3.3,
    /// AC-6.1) — session liveness plays NO part in this (§3.3: "does NOT
    /// determine active/idle state").
    /// What: reloads from disk, then: if the persisted active pointer still
    /// references an existing workstream, leaves it untouched (restored);
    /// if it references a workstream that no longer exists, clears it to
    /// `None` and persists that single change; if there was no pointer,
    /// does nothing. Every workstream record is preserved either way — this
    /// method never removes an entry from `workstreams`.
    /// Test: `store_tests::reconcile_restores_active_pointer`,
    /// `store_tests::reconcile_clears_active_when_target_missing`,
    /// `store_tests::reconcile_is_noop_with_no_active_pointer`.
    pub async fn reconcile_on_boot(&mut self) -> Result<ReconcileOutcome, StoreError> {
        self.reload_if_changed().await?;
        let mut outcome = ReconcileOutcome {
            workstream_count: self.data.workstreams.len(),
            ..Default::default()
        };
        match self.data.active_workstream_id {
            Some(id) if self.data.workstreams.iter().any(|w| w.id == id) => {
                outcome.active_restored = Some(id);
            }
            Some(_) => {
                self.data.active_workstream_id = None;
                outcome.active_cleared = true;
                self.save().await?;
            }
            None => {}
        }
        Ok(outcome)
    }
}

#[cfg(test)]
#[path = "store_tests.rs"]
mod store_tests;
